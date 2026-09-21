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
    // Which invocations may go to the app-server is not knowable from a list: Codex
    // adds subcommands between releases and rewrites hidden aliases before parsing
    // (`codex auth login` becomes `codex login`; `codex auth --help` reports the
    // TUI). So the function asks codex itself: the `Usage:` line printed for
    // `<args> --help` names the resolved subcommand, or the TUI form
    // `codex [OPTIONS] [PROMPT]`. Costs ~10 ms, needs no terminal, never stale. Only
    // the words before `--` are probed: after it, `--help` would be a prompt word and
    // the probe would start a session.
    //
    // The bare TUI, `codex resume …` and `codex fork …` are handed to `ctm codex-launch`, which
    // starts or resumes the session in the app-server with the typed flags in effect
    // (ADR-022: a remote resume ignores `--cd` and refuses permission flags, so the
    // launcher applies them through the API first) — or runs codex locally, with one
    // line saying so, when there is no app-server to attach to; that decision is the
    // launcher's, made against ctm's configured socket, so the shell and the launcher
    // can never disagree about which daemon counts. Everything else — every other
    // subcommand, and anything that chose its own endpoint with `--remote` — runs
    // exactly as typed.
    match shell {
        Shell::Zsh | Shell::Bash => "codex() {\n\
             \x20 if [ \"${CTM_CODEX_REMOTE:-1}\" = \"0\" ]; then command codex \"$@\"; return; fi\n\
             \x20 local -a probe=(); local a\n\
             \x20 for a in \"$@\"; do case \"$a\" in --) break;; --remote|--remote=*) command codex \"$@\"; return;; esac; probe+=(\"$a\"); done\n\
             \x20 case \"$(command codex \"${probe[@]}\" --help 2>/dev/null | sed -n 's/^Usage: //p' | head -n 1)\" in\n\
             \x20   \"codex [\"*|\"codex <\"*|codex|\"codex resume\"*|\"codex fork\"*) command ctm codex-launch \"$@\";;\n\
             \x20   *) command codex \"$@\";;\n\
             \x20 esac\n\
             }\n"
            .to_string(),
        Shell::Fish => "function codex\n\
             \x20 if test \"$CTM_CODEX_REMOTE\" = 0\n\
             \x20   command codex $argv; return\n\
             \x20 end\n\
             \x20 set -l probe\n\
             \x20 for a in $argv\n\
             \x20   switch $a\n\
             \x20     case --\n\
             \x20       break\n\
             \x20     case --remote '--remote=*'\n\
             \x20       command codex $argv; return\n\
             \x20   end\n\
             \x20   set -a probe $a\n\
             \x20 end\n\
             \x20 set -l usage (command codex $probe --help 2>/dev/null | sed -n 's/^Usage: //p' | head -n 1)\n\
             \x20 switch \"$usage\"\n\
             \x20   case 'codex [*' 'codex <*' codex 'codex resume*' 'codex fork*'\n\
             \x20     command ctm codex-launch $argv\n\
             \x20   case '*'\n\
             \x20     command codex $argv\n\
             \x20 end\n\
             end\n"
            .to_string(),
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
    use std::process::Command;

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
        // The bare TUI and `resume` go to the launcher, which attaches them to the
        // app-server with the typed flags in effect (ADR-022).
        // The probe sees only the words before `--`: after it `--help` is a prompt.
        assert!(b.contains(
            r#"command codex "${probe[@]}" --help 2>/dev/null | sed -n 's/^Usage: //p'"#
        ));
        assert!(b.contains(r#"case "$a" in --) break;; --remote|--remote=*) command codex "$@"; return;; esac; probe+=("$a")"#));
        assert!(b.contains(
            r#""codex ["*|"codex <"*|codex|"codex resume"*|"codex fork"*) command ctm codex-launch "$@";;"#
        ));
        // Every other subcommand — decided by codex itself, not by a list that goes
        // stale (`codex auth login` is a hidden alias no list would contain) — and an
        // explicit endpoint run exactly as typed.
        assert!(b.contains(r#"*) command codex "$@";;"#));
        assert!(!b.contains("PASSTHROUGH"), "no static subcommand list");
        // Whether an app-server is there to attach to is the launcher's call, against
        // ctm's configured socket — the shell no longer keeps its own idea of it.
        assert!(!b.contains("app-server-control.sock"));
        assert!(
            b.contains(r#""${CTM_CODEX_REMOTE:-1}" = "0""#),
            "opt-out stays"
        );
    }

    /// The generated function, parsed and *run* by the real shells: a stub `codex`
    /// records what the wrapper decided. Skipped per shell that is not installed.
    #[test]
    fn wrapper_routes_in_real_shells() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        // `codex … --help` answers with codex's real Usage lines; anything else logs.
        let stub = r#"#!/bin/sh
log="$CTM_TEST_LOG"
last=""; for a in "$@"; do last="$a"; done
if [ "$last" = "--help" ]; then
  case "$1" in
    resume) echo "Usage: codex resume [OPTIONS] [SESSION_ID] [PROMPT]";;
    fork) echo "Usage: codex fork [OPTIONS] [SESSION_ID] [PROMPT]";;
    auth|login) echo "Usage: codex login [OPTIONS] [COMMAND]";;
    *) echo "Usage: codex [OPTIONS] [PROMPT]";;
  esac
  exit 0
fi
printf 'codex:%s
' "$*" >> "$log"
"#;
        let ctm = "#!/bin/sh\nprintf 'ctm:%s\\n' \"$*\" >> \"$CTM_TEST_LOG\"\n";
        for (name, body) in [("codex", stub), ("ctm", ctm)] {
            let p = bin.join(name);
            std::fs::write(&p, body).unwrap();
            let mut perm = std::fs::metadata(&p).unwrap().permissions();
            std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
            std::fs::set_permissions(&p, perm).unwrap();
        }
        let cases: &[(&str, &str)] = &[
            (
                "codex --yolo resume abc",
                "ctm:codex-launch --yolo resume abc",
            ),
            ("codex", "ctm:codex-launch"),
            ("codex -- resume", "ctm:codex-launch -- resume"),
            ("codex fork --last", "ctm:codex-launch fork --last"),
            (
                "codex auth login --device-auth",
                "codex:auth login --device-auth",
            ),
            (
                "codex --remote unix:///x resume abc",
                "codex:--remote unix:///x resume abc",
            ),
            (
                "codex --remote-auth-token-env T resume abc",
                "ctm:codex-launch --remote-auth-token-env T resume abc",
            ),
            ("CTM_CODEX_REMOTE=0 codex resume abc", "codex:resume abc"),
        ];
        for shell in [Shell::Zsh, Shell::Bash, Shell::Fish] {
            let exe = match shell {
                Shell::Zsh => "zsh",
                Shell::Bash => "bash",
                Shell::Fish => "fish",
            };
            if Command::new(exe).arg("--version").output().is_err() {
                eprintln!("skip: {exe} not installed");
                continue;
            }
            let block = codex_wrapper(shell);
            for (invocation, want) in cases {
                let log = dir.path().join("log");
                let _ = std::fs::remove_file(&log);
                let (env_prefix, cmd) = match invocation.strip_prefix("CTM_CODEX_REMOTE=0 ") {
                    Some(rest) => ("set -x CTM_CODEX_REMOTE 0; ", rest),
                    None => ("", *invocation),
                };
                let script = match shell {
                    Shell::Fish => format!("{block}{}{cmd}", env_prefix),
                    _ => format!(
                        "{block}{}{cmd}",
                        env_prefix.replace("set -x CTM_CODEX_REMOTE 0; ", "CTM_CODEX_REMOTE=0 ")
                    ),
                };
                let out = Command::new(exe)
                    .arg("-c")
                    .arg(&script)
                    .env(
                        "PATH",
                        format!(
                            "{}:{}",
                            bin.display(),
                            std::env::var("PATH").unwrap_or_default()
                        ),
                    )
                    .env("CTM_TEST_LOG", &log)
                    .output()
                    .unwrap();
                let got = std::fs::read_to_string(&log).unwrap_or_default();
                assert_eq!(
                    got.trim_end(),
                    *want,
                    "{exe}: `{invocation}` (stderr: {})",
                    String::from_utf8_lossy(&out.stderr)
                );
            }
        }
    }

    #[test]
    fn fish_wrapper_matches_remote_exactly_not_by_prefix() {
        let b = block_body(
            Shell::Fish,
            Path::new("/home/u/.local/bin"),
            Path::new("/home/u/c"),
        );
        // `--remote*` would also match `--remote-auth-token-env`, which is not an
        // endpoint choice.
        assert!(b.contains("case --remote '--remote=*'"));
        assert!(!b.contains("'--remote*'"));
        assert!(b.contains("case --\n"));
        assert!(b.contains("command codex $probe --help"));
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
