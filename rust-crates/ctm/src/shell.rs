//! ADR-017 §shell integration: PATH and tab-completion that just work.
//!
//! `install.sh` and `ctm update` call [`provision`] after placing the binary. It:
//! 1. writes static clap completion scripts for bash, zsh and fish into each shell's
//!    per-user autoload location, and
//! 2. maintains ONE idempotent, marker-delimited block at the END of the shell's
//!    startup file that puts the install directory first on `PATH` (appending at the
//!    end is what makes it win over version managers like fnm/nvm that prepend their
//!    shim dirs earlier in the same file) and, for zsh, adds the completion dir to
//!    `fpath` and either initialises `compinit` (if the rc has not) or registers
//!    `_ctm` directly with `compdef` (if it has — a later fpath change is invisible to
//!    an already-initialised compinit; this was the "tab completion not working" bug).
//!
//! Rules that keep this from being a nuisance: the block is rewritten in place (never
//! duplicated) and removed exactly by `ctm shell-setup --remove`; only the login
//! shell's rc is created if missing, other shells are touched only if their config
//! already exists; every step is best-effort and reported, never fatal to the install;
//! `CTM_NO_SHELL_SETUP=1` skips all of it.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

const BEGIN: &str = "# >>> ctm >>>";
const END: &str = "# <<< ctm <<<";
const OPT_OUT_VAR: &str = "CTM_NO_SHELL_SETUP";

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
}

impl Shell {
    fn name(self) -> &'static str {
        match self {
            Shell::Bash => "bash",
            Shell::Zsh => "zsh",
            Shell::Fish => "fish",
        }
    }
    fn from_login_shell() -> Option<Shell> {
        let s = std::env::var("SHELL").ok()?;
        match Path::new(&s).file_name()?.to_str()? {
            "bash" => Some(Shell::Bash),
            "zsh" => Some(Shell::Zsh),
            "fish" => Some(Shell::Fish),
            _ => None,
        }
    }
}

/// `ctm completions <shell>`: the static completion script on stdout.
pub fn print_completions(shell: Shell, out: &mut dyn Write) -> std::io::Result<()> {
    let mut cmd = crate::cli::cli_command();
    let gen = match shell {
        Shell::Bash => clap_complete::Shell::Bash,
        Shell::Zsh => clap_complete::Shell::Zsh,
        Shell::Fish => clap_complete::Shell::Fish,
    };
    let mut buf = Vec::new();
    clap_complete::generate(gen, &mut cmd, "ctm", &mut buf);
    out.write_all(&buf)
}

fn completion_script(shell: Shell) -> Vec<u8> {
    let mut buf = Vec::new();
    let _ = print_completions(shell, &mut buf);
    buf
}

/// Where each shell autoloads a per-user completion for `ctm`.
fn completion_path(home: &Path, shell: Shell) -> PathBuf {
    let xdg_data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local").join("share"));
    let xdg_config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    match shell {
        Shell::Bash => xdg_data
            .join("bash-completion")
            .join("completions")
            .join("ctm"),
        Shell::Zsh => xdg_data.join("zsh").join("site-functions").join("_ctm"),
        Shell::Fish => xdg_config.join("fish").join("completions").join("ctm.fish"),
    }
}

/// The startup file(s) that get the managed block.
fn rc_files(home: &Path, shell: Shell) -> Vec<PathBuf> {
    match shell {
        Shell::Zsh => vec![home.join(".zshrc")],
        // macOS login bash reads .bash_profile and commonly does not source .bashrc;
        // Linux interactive bash reads .bashrc. Maintain the block in both when
        // present; create only .bashrc if neither exists.
        Shell::Bash => vec![home.join(".bashrc"), home.join(".bash_profile")],
        Shell::Fish => {
            let cfg = std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config"));
            vec![cfg.join("fish").join("conf.d").join("ctm.fish")]
        }
    }
}

/// ADR-016 §Codex approvals: the shell function that makes a plain `codex` join the
/// app-server, which is the only way an approval can be mirrored *correctly*.
///
/// A bare `codex` owns its thread, and a second client can neither observe it nor
/// answer its approvals: `PermissionRequest` hooks carry no request id, so a remote
/// decision cannot be bound to the request it answers, and driving the prompt by
/// keystroke is blind injection into an unknown screen — the ADR-014 failure class.
/// `codex --remote unix://<socket>` puts the session in the app-server instead, where
/// every approval is a JSON-RPC request with an id that ctm answers atomically and
/// `serverRequest/resolved` retires the other surface. That is ctm's invariant, kept.
///
/// The function is deliberately conservative:
/// - it only touches a plain interactive run — any subcommand (`exec`, `app-server`,
///   `resume`, `login`, …) and any explicit `--remote`/`-C` is passed through untouched;
/// - it requires the control socket to exist, so with no ctm daemon `codex` is just
///   `codex`;
/// - `-C "$PWD"` preserves the working directory (without it the session would silently
///   adopt the daemon's — verified);
/// - `CTM_CODEX_REMOTE=0` turns it off.
fn codex_wrapper(shell: Shell) -> String {
    // Subcommands that must never be rewritten (`codex --help` prints this set).
    const PASSTHROUGH: &str = "agents exec e review login logout mcp plugin app-server remote-control app completion update doctor sandbox debug apply resume queue archive delete migrate-rollouts unarchive help";
    match shell {
        Shell::Zsh | Shell::Bash => format!(
            "codex() {{\n\
             \x20 local sock=\"${{CODEX_HOME:-$HOME/.codex}}/app-server-control/app-server-control.sock\"\n\
             \x20 if [ \"${{CTM_CODEX_REMOTE:-1}}\" = \"0\" ] || [ ! -S \"$sock\" ]; then command codex \"$@\"; return; fi\n\
             \x20 case \" {PASSTHROUGH} \" in *\" ${{1:-}} \"*) command codex \"$@\"; return;; esac\n\
             \x20 for a in \"$@\"; do case \"$a\" in --remote|--remote=*|-C|--cd|--cd=*) command codex \"$@\"; return;; esac; done\n\
             \x20 command codex --remote \"unix://$sock\" -C \"$PWD\" \"$@\"\n\
             }}\n"
        ),
        Shell::Fish => format!(
            "function codex\n\
             \x20 set -l sock (test -n \"$CODEX_HOME\"; and echo $CODEX_HOME; or echo $HOME/.codex)/app-server-control/app-server-control.sock\n\
             \x20 if test \"$CTM_CODEX_REMOTE\" = 0 -o ! -S $sock\n\
             \x20   command codex $argv; return\n\
             \x20 end\n\
             \x20 if contains -- \"$argv[1]\" {PASSTHROUGH_FISH}\n\
             \x20   command codex $argv; return\n\
             \x20 end\n\
             \x20 if string match -q -- '--remote*' $argv; or string match -q -- '-C' $argv; or string match -q -- '--cd*' $argv\n\
             \x20   command codex $argv; return\n\
             \x20 end\n\
             \x20 command codex --remote \"unix://$sock\" -C \"$PWD\" $argv\n\
             end\n",
            PASSTHROUGH_FISH = PASSTHROUGH
        ),
    }
}

fn block_body(shell: Shell, install_dir: &Path, completion_dir: &Path) -> String {
    let dir = install_dir.display();
    match shell {
        // Two cases, both verified in a clean-env login zsh against a real rc:
        // - compinit has NOT run yet: run it; it scans fpath and registers `_ctm` from
        //   the file's `#compdef ctm` header.
        // - compinit HAS run (oh-my-zsh, or another tool's block earlier in the same
        //   rc): its table is already built and a later fpath change is invisible to
        //   it, so register the function directly. `autoload` alone is not enough —
        //   `compdef` is what maps the command to it.
        Shell::Zsh => format!(
            "{BEGIN}\n\
             # managed by `ctm shell-setup`; edits here are overwritten, remove with `ctm shell-setup --remove`\n\
             export PATH=\"{dir}:$PATH\"\n\
             (( ${{fpath[(Ie){cd}]}} )) || fpath=(\"{cd}\" $fpath)\n\
             if (( $+functions[compdef] )); then autoload -Uz _ctm && compdef _ctm ctm; else autoload -Uz compinit && compinit -i; fi\n\
             {codex}\
             {END}\n",
            cd = completion_dir.display(),
            codex = codex_wrapper(shell)
        ),
        Shell::Bash => format!(
            "{BEGIN}\n\
             # managed by `ctm shell-setup`; edits here are overwritten, remove with `ctm shell-setup --remove`\n\
             export PATH=\"{dir}:$PATH\"\n\
             [ -f \"{comp}\" ] && . \"{comp}\"\n\
             {codex}\
             {END}\n",
            comp = completion_dir.join("ctm").display(),
            codex = codex_wrapper(shell)
        ),
        // A whole file we own, so no markers needed — but keep them for symmetry
        // with --remove, which deletes the file.
        Shell::Fish => format!(
            "{BEGIN}\n\
             # managed by `ctm shell-setup`; remove with `ctm shell-setup --remove`\n\
             fish_add_path --global --move \"{dir}\"\n\
             {codex}\
             {END}\n",
            codex = codex_wrapper(shell)
        ),
    }
}

/// Replace or append the managed block. Returns whether the file changed.
fn upsert_block(rc: &Path, block: &str, create: bool) -> std::io::Result<bool> {
    let existing = match fs::read_to_string(rc) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if !create {
                return Ok(false);
            }
            String::new()
        }
        Err(e) => return Err(e),
    };
    let new_text = match (existing.find(BEGIN), existing.find(END)) {
        (Some(b), Some(e)) if e > b => {
            let end = e + END.len();
            let tail_nl = if existing[end..].starts_with('\n') {
                end + 1
            } else {
                end
            };
            format!("{}{}{}", &existing[..b], block, &existing[tail_nl..])
        }
        _ => {
            let sep = if existing.is_empty() || existing.ends_with('\n') {
                ""
            } else {
                "\n"
            };
            format!("{existing}{sep}\n{block}")
        }
    };
    if new_text == existing {
        return Ok(false);
    }
    if let Some(p) = rc.parent() {
        fs::create_dir_all(p)?;
    }
    write_atomic(rc, new_text.as_bytes(), 0o644)?;
    Ok(true)
}

fn remove_block(rc: &Path) -> std::io::Result<bool> {
    let Ok(existing) = fs::read_to_string(rc) else {
        return Ok(false);
    };
    let (Some(b), Some(e)) = (existing.find(BEGIN), existing.find(END)) else {
        return Ok(false);
    };
    if e < b {
        return Ok(false);
    }
    let end = e + END.len();
    let tail = if existing[end..].starts_with('\n') {
        end + 1
    } else {
        end
    };
    // also drop the blank separator line we add before the block
    let head = existing[..b].trim_end_matches('\n');
    let mut out = head.to_string();
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(&existing[tail..]);
    write_atomic(rc, out.as_bytes(), 0o644)?;
    Ok(true)
}

fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = path.with_extension("ctm-tmp");
    {
        let mut f = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(mode)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)
}

#[derive(Debug, Default)]
pub struct Report {
    pub lines: Vec<String>,
    pub errors: Vec<String>,
}

/// Provision completions + PATH for the login shell (always) and for any other
/// supported shell whose config already exists. Best-effort per step.
pub fn provision(install_dir: &Path) -> Report {
    let mut r = Report::default();
    if std::env::var_os(OPT_OUT_VAR).is_some_and(|v| !v.is_empty()) {
        r.lines
            .push(format!("{OPT_OUT_VAR} set — shell integration skipped"));
        return r;
    }
    let home = crate::config::home_dir();
    let login = Shell::from_login_shell();
    for shell in [Shell::Zsh, Shell::Bash, Shell::Fish] {
        let is_login = login == Some(shell);
        let rcs = rc_files(&home, shell);
        let any_rc = rcs.iter().any(|p| p.exists());
        let has_config = match shell {
            Shell::Fish => home.join(".config").join("fish").exists(),
            _ => any_rc,
        };
        if !is_login && !has_config {
            continue; // never litter a shell the user does not use
        }
        // 1. completion file
        let cpath = completion_path(&home, shell);
        match cpath
            .parent()
            .ok_or_else(|| std::io::Error::other("no parent"))
            .and_then(fs::create_dir_all)
            .and_then(|_| write_atomic(&cpath, &completion_script(shell), 0o644))
        {
            Ok(()) => r.lines.push(format!(
                "{}: completions -> {}",
                shell.name(),
                cpath.display()
            )),
            Err(e) => r
                .errors
                .push(format!("{}: completions not written ({e})", shell.name())),
        }
        // 2. rc block
        let cdir = cpath.parent().map(Path::to_path_buf).unwrap_or_default();
        let block = block_body(shell, install_dir, &cdir);
        let mut touched = false;
        for (i, rc) in rcs.iter().enumerate() {
            // create only the first (primary) rc, and only for the login shell / fish
            let create = (is_login || shell == Shell::Fish) && i == 0 && !any_rc;
            if !rc.exists() && !create {
                continue;
            }
            match upsert_block(rc, &block, create) {
                Ok(true) => {
                    touched = true;
                    r.lines.push(format!(
                        "{}: PATH{} block -> {}",
                        shell.name(),
                        if shell == Shell::Zsh { "+fpath" } else { "" },
                        rc.display()
                    ));
                }
                Ok(false) => {}
                Err(e) => r.errors.push(format!(
                    "{}: could not update {} ({e})",
                    shell.name(),
                    rc.display()
                )),
            }
        }
        if !touched {
            r.lines
                .push(format!("{}: startup file already current", shell.name()));
        }
    }
    if login.is_none() {
        r.lines.push(
            "login shell not recognised ($SHELL); provisioned only shells with existing config"
                .into(),
        );
    }
    r
}

/// Undo [`provision`] exactly: delete the completion files and the managed blocks.
pub fn remove() -> Report {
    let mut r = Report::default();
    let home = crate::config::home_dir();
    for shell in [Shell::Zsh, Shell::Bash, Shell::Fish] {
        let cpath = completion_path(&home, shell);
        if cpath.exists() {
            match fs::remove_file(&cpath) {
                Ok(()) => r
                    .lines
                    .push(format!("{}: removed {}", shell.name(), cpath.display())),
                Err(e) => r.errors.push(format!(
                    "{}: {} not removed ({e})",
                    shell.name(),
                    cpath.display()
                )),
            }
        }
        for rc in rc_files(&home, shell) {
            let res = if shell == Shell::Fish {
                if rc.exists() {
                    fs::remove_file(&rc).map(|_| true)
                } else {
                    Ok(false)
                }
            } else {
                remove_block(&rc)
            };
            match res {
                Ok(true) => r
                    .lines
                    .push(format!("{}: cleaned {}", shell.name(), rc.display())),
                Ok(false) => {}
                Err(e) => r.errors.push(format!(
                    "{}: could not clean {} ({e})",
                    shell.name(),
                    rc.display()
                )),
            }
        }
    }
    r
}

/// `ctm shell-setup [--remove]`
pub fn run_shell_setup(remove_it: bool) -> anyhow::Result<()> {
    let report = if remove_it {
        remove()
    } else {
        let exe = std::env::current_exe()?;
        let dir = exe
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(crate::update::default_install_dir);
        provision(&dir)
    };
    for l in &report.lines {
        println!("{l}");
    }
    for e in &report.errors {
        eprintln!("warning: {e}");
    }
    if !remove_it {
        println!("open a new shell (or `exec $SHELL`) for PATH and completions to take effect");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completions_generate_for_every_shell_and_mention_subcommands() {
        for s in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            let script = String::from_utf8(completion_script(s)).unwrap();
            assert!(script.len() > 500, "{:?} script too small", s);
            assert!(script.contains("update"), "{:?} lacks `update`", s);
            assert!(script.contains("doctor"), "{:?} lacks `doctor`", s);
        }
    }

    #[test]
    fn block_is_appended_at_end_then_rewritten_in_place_never_duplicated() {
        let d = tempfile::tempdir().unwrap();
        let rc = d.path().join(".zshrc");
        fs::write(
            &rc,
            "export PATH=\"$HOME/.local/bin:$PATH\"\neval \"$(fnm env)\"\n",
        )
        .unwrap();
        let b1 = block_body(
            Shell::Zsh,
            Path::new("/home/u/.local/bin"),
            Path::new("/home/u/.local/share/zsh/site-functions"),
        );
        assert!(upsert_block(&rc, &b1, false).unwrap());
        let t = fs::read_to_string(&rc).unwrap();
        assert_eq!(t.matches(BEGIN).count(), 1);
        assert!(
            t.ends_with(&format!("{END}\n")),
            "block lands at the END so it wins over fnm"
        );
        assert!(t.find("fnm env").unwrap() < t.find(BEGIN).unwrap());
        // idempotent
        assert!(!upsert_block(&rc, &b1, false).unwrap());
        // changed install dir -> rewritten in place, still exactly one block
        let b2 = block_body(
            Shell::Zsh,
            Path::new("/opt/bin"),
            Path::new("/home/u/.local/share/zsh/site-functions"),
        );
        assert!(upsert_block(&rc, &b2, false).unwrap());
        let t = fs::read_to_string(&rc).unwrap();
        assert_eq!(t.matches(BEGIN).count(), 1);
        assert!(t.contains("/opt/bin") && !t.contains("/home/u/.local/bin:$PATH\"\nfpath"));
        // remove restores the original exactly
        assert!(remove_block(&rc).unwrap());
        assert_eq!(
            fs::read_to_string(&rc).unwrap(),
            "export PATH=\"$HOME/.local/bin:$PATH\"\neval \"$(fnm env)\"\n"
        );
        assert!(!remove_block(&rc).unwrap());
    }

    #[test]
    fn missing_rc_is_created_only_when_asked() {
        let d = tempfile::tempdir().unwrap();
        let rc = d.path().join(".bashrc");
        let b = block_body(Shell::Bash, Path::new("/x/bin"), Path::new("/x/comp"));
        assert!(!upsert_block(&rc, &b, false).unwrap());
        assert!(!rc.exists());
        assert!(upsert_block(&rc, &b, true).unwrap());
        assert!(fs::read_to_string(&rc).unwrap().contains("/x/bin"));
    }

    #[test]
    fn zsh_block_wires_fpath_and_registers_for_both_compinit_orders() {
        let b = block_body(Shell::Zsh, Path::new("/x/bin"), Path::new("/x/sf"));
        assert!(b.contains("(( ${fpath[(Ie)/x/sf]} )) || fpath=(\"/x/sf\" $fpath)"));
        // compinit already ran earlier in the rc → direct registration.
        assert!(b.contains(
            "if (( $+functions[compdef] )); then autoload -Uz _ctm && compdef _ctm ctm;"
        ));
        // compinit not yet run → run it (scans fpath, honours `#compdef ctm`).
        assert!(b.contains("else autoload -Uz compinit && compinit -i; fi"));
        let f = block_body(Shell::Fish, Path::new("/x/bin"), Path::new("/x/c"));
        assert!(f.contains("fish_add_path --global --move \"/x/bin\""));
    }

    #[test]
    fn codex_wrapper_only_rewrites_a_plain_interactive_run() {
        let b = block_body(
            Shell::Zsh,
            Path::new("/home/u/.local/bin"),
            Path::new("/home/u/.zsh"),
        );
        // The rewrite that makes approvals answerable: the session lives in the
        // app-server, and -C keeps the working directory (without it the session
        // silently adopts the daemon's — verified).
        assert!(b.contains(r#"command codex --remote "unix://$sock" -C "$PWD" "$@""#));
        // Never touch a subcommand …
        for sub in ["exec", "app-server", "resume", "login", "mcp"] {
            assert!(
                b.contains(&format!(" {sub} ")),
                "{sub} must be in the passthrough set"
            );
        }
        // … nor an invocation that already chose its own endpoint or directory …
        assert!(b.contains("--remote|--remote=*|-C|--cd|--cd=*"));
        // … nor anything when the daemon is down or the operator opted out.
        assert!(b.contains(r#"[ ! -S "$sock" ]"#));
        assert!(b.contains(r#""${CTM_CODEX_REMOTE:-1}" = "0""#));
    }

    #[test]
    fn codex_wrapper_is_written_for_every_shell() {
        for shell in [Shell::Zsh, Shell::Bash, Shell::Fish] {
            let b = block_body(
                shell,
                Path::new("/home/u/.local/bin"),
                Path::new("/home/u/c"),
            );
            assert!(b.contains("codex"), "{shell:?} block defines the wrapper");
            assert!(b.contains("CTM_CODEX_REMOTE"), "{shell:?} opt-out");
        }
    }
}
