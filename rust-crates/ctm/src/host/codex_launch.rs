//! ADR-022: the pure half of `ctm codex-launch` — turn a codex command line into a
//! plan the runner (`codex_launch_run.rs`) can carry out against the app-server.
//!
//! A thread that lives in the app-server ignores `--cd` and refuses permission flags
//! on `resume`, and a plain remote resume does not restore the flags the session was
//! started with. The API takes them (`thread/settings/update`: `cwd`, a permission
//! profile, an approval policy). So a `resume` is translated: the flags become
//! settings to apply first, and are removed from what codex sees. A new session is
//! left exactly as typed — codex honours its flags at start.
//!
//! Nothing here touches the system; every rule below is unit-tested. Rules the
//! reviewers (Codex, GLM-5.3, 2026-09-22) insisted on and that hold here:
//! - `--help`/`--version` anywhere mean "no side effects at all";
//! - `--` ends option parsing, for us exactly as for codex;
//! - an input we cannot honour is an error, never a silently narrower or wider run;
//! - the set of value-taking flags comes from codex's own `--help`, not a list.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Which thread a `resume` names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThreadRef {
    Id(String),
    /// A session name (codex accepts names; UUIDs take precedence if they parse).
    Name(String),
    /// `--last`: the most recently updated thread — in the plan's directory unless
    /// `--all` was given, interactive unless `--include-non-interactive` was
    /// (codex's own semantics, from its `resume --help`).
    Last {
        all: bool,
        include_non_interactive: bool,
    },
    /// No id, no name, no `--last`: codex's picker. Nothing can be pre-applied.
    Picker,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// Run exactly as typed, remote or not: help/version, or an explicit `--remote`.
    Verbatim(&'static str),
    New,
    Resume(ThreadRef),
    /// `codex fork`: same shape as resume (`[SESSION_ID]`, `--last`, `--all`), and a
    /// remote fork refuses permission flags exactly as a remote resume does (spike
    /// 2026-09-22). The runner forks through the API with the settings, then attaches
    /// to the new thread with `resume`.
    Fork(ThreadRef),
}

/// Thread settings the command line asked for. Later flags win; the bypass sets both.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Settings {
    /// `:read-only` | `:workspace` | `:danger-full-access` | a named profile from
    /// the directory's `.codex/config.toml` (`-c default_permissions=<name>`).
    pub permissions: Option<String>,
    /// `on-request` | `never` (this codex); passed through unvalidated to the API
    /// only after codex's own help listed it.
    pub approval: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub kind: Kind,
    /// Absolute. `-C/--cd` resolved against where the user typed the command, else
    /// that directory itself.
    pub cwd: PathBuf,
    pub settings: Settings,
    /// What codex is given. For a resume, translated flags are gone and the thread
    /// token is at `thread_token`, to be replaced by the resolved id.
    pub codex_args: Vec<String>,
    pub thread_token: Option<usize>,
    /// Index of `resume`/`fork` in `codex_args` (a fork attaches with `resume`).
    pub subcommand_token: Option<usize>,
    /// The user passed `-C/--cd` themselves (a new session must not get a second one).
    pub explicit_cd: bool,
}

/// Why a command line cannot be run through the app-server as typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub message: String,
    /// codex's own exit code for a usage error.
    pub exit_code: i32,
}

fn refuse(message: impl Into<String>) -> Refusal {
    Refusal {
        message: message.into(),
        exit_code: 2,
    }
}

/// Flags that consume the next token, taken from codex's own `--help`
/// (`  -c, --config <key=value>`, `      --add-dir <DIR>`). Both the short and the
/// long spelling are returned. Pure; the runner feeds it real help text.
pub fn value_flags_from_help(help: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in help.lines() {
        let t = line.trim_start();
        if !t.starts_with('-') || !t.contains(" <") {
            continue;
        }
        // `-c, --config <key=value>` | `--add-dir <DIR>` | `-i, --image <FILE>...`
        let names = t.split(" <").next().unwrap_or("");
        for n in names.split(',').map(str::trim) {
            if n.starts_with('-')
                && n.len() > 1
                && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            {
                out.insert(n.to_string());
            }
        }
    }
    out
}

/// What codex 0.155 prints; used when the runner cannot ask codex.
pub fn fallback_value_flags() -> BTreeSet<String> {
    [
        "-c",
        "--config",
        "--enable",
        "--disable",
        "--remote",
        "--remote-auth-token-env",
        "-i",
        "--image",
        "-m",
        "--model",
        "--local-provider",
        "-p",
        "--profile",
        "-s",
        "--sandbox",
        "-C",
        "--cd",
        "--add-dir",
        "-a",
        "--ask-for-approval",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

const APPROVAL_POLICIES: &[&str] = &["untrusted", "on-failure", "on-request", "never"];

fn sandbox_profile(mode: &str) -> Option<&'static str> {
    match mode {
        "read-only" => Some(":read-only"),
        "workspace-write" => Some(":workspace"),
        "danger-full-access" => Some(":danger-full-access"),
        _ => None,
    }
}

/// One token as codex would see it: `--long`, `--long=value`, `-x`, `-xvalue`.
struct Tok<'a> {
    flag: &'a str,
    attached: Option<String>,
}

fn split_token<'a>(a: &'a str, value_flags: &BTreeSet<String>) -> Tok<'a> {
    if let Some(rest) = a.strip_prefix("--") {
        if let Some((f, v)) = rest.split_once('=') {
            return Tok {
                flag: &a[..2 + f.len()],
                attached: Some(v.to_string()),
            };
        }
        return Tok {
            flag: a,
            attached: None,
        };
    }
    if a.len() > 2 && a.starts_with('-') && value_flags.contains(&a[..2]) {
        let v = a[2..].strip_prefix('=').unwrap_or(&a[2..]);
        return Tok {
            flag: &a[..2],
            attached: Some(v.to_string()),
        };
    }
    Tok {
        flag: a,
        attached: None,
    }
}

fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}

fn absolute(path: &str, pwd: &Path) -> Result<PathBuf, Refusal> {
    let p = Path::new(path);
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        pwd.join(p)
    };
    std::fs::canonicalize(&joined)
        .map_err(|e| refuse(format!("directory {} is not usable: {e}", joined.display())))
}

/// Translate a codex command line. `pwd` is where the user typed it.
pub fn plan(args: &[String], pwd: &Path, value_flags: &BTreeSet<String>) -> Result<Plan, Refusal> {
    // Pass 1: things that decide the kind without translating anything.
    let mut positionals: Vec<(usize, String)> = Vec::new(); // (index in args, token)
    let mut i = 0;
    let mut terminator = args.len();
    while i < args.len() {
        let a = &args[i];
        if a == "--" {
            terminator = i;
            break;
        }
        if a == "-h" || a == "--help" || a == "-V" || a == "--version" {
            return Ok(verbatim(args, pwd, "help"));
        }
        let t = split_token(a, value_flags);
        if t.flag == "--remote" {
            return Ok(verbatim(args, pwd, "explicit --remote"));
        }
        if a.starts_with('-') {
            if value_flags.contains(t.flag) && t.attached.is_none() {
                i += 1; // the value
            }
        } else {
            positionals.push((i, a.clone()));
        }
        i += 1;
    }
    for (idx, a) in args.iter().enumerate().skip(terminator + 1) {
        positionals.push((idx, a.clone()));
    }

    let explicit_cd = find_cd(args, value_flags, terminator);
    let cwd = match &explicit_cd {
        Some(v) => absolute(v, pwd)?,
        None => absolute(&pwd.display().to_string(), pwd)?,
    };

    // The subcommand is the first positional *before* `--`; after it, `resume` is a
    // word of the prompt.
    let subcommand = positionals
        .first()
        .filter(|(idx, s)| *idx < terminator && (s == "resume" || s == "fork"))
        .map(|(idx, s)| (*idx, s.clone()));
    let Some((resume_idx, subcommand)) = subcommand else {
        return Ok(Plan {
            kind: Kind::New,
            cwd,
            settings: Settings::default(),
            codex_args: args.to_vec(),
            thread_token: None,
            subcommand_token: None,
            explicit_cd: explicit_cd.is_some(),
        });
    };

    // Pass 2 (resume/fork only): translate and strip.
    let mut settings = Settings::default();
    let mut out: Vec<String> = Vec::new();
    let mut thread_token: Option<usize> = None;
    let mut subcommand_token: Option<usize> = None;
    let mut last = false;
    let mut all = false;
    let mut include_non_interactive = false;
    let mut i = 0;
    while i < terminator {
        let a = &args[i];
        let t = split_token(a, value_flags);
        let takes = value_flags.contains(t.flag);
        let value: Option<String> = if takes {
            match t.attached.clone() {
                Some(v) => Some(v),
                None => {
                    i += 1;
                    match args.get(i) {
                        Some(v) if i < terminator => Some(v.clone()),
                        _ => return Err(refuse(format!("a value is required for '{}'", t.flag))),
                    }
                }
            }
        } else {
            None
        };
        match t.flag {
            "--dangerously-bypass-approvals-and-sandbox" | "--yolo" => {
                settings.permissions = Some(":danger-full-access".into());
                settings.approval = Some("never".into());
            }
            "-s" | "--sandbox" => {
                let v = value.clone().unwrap_or_default();
                settings.permissions = Some(
                    sandbox_profile(&v)
                        .ok_or_else(|| refuse(format!("invalid value '{v}' for '--sandbox <SANDBOX_MODE>' [possible values: read-only, workspace-write, danger-full-access]")))?
                        .into(),
                );
            }
            "-a" | "--ask-for-approval" => {
                let v = value.clone().unwrap_or_default();
                if !APPROVAL_POLICIES.contains(&v.as_str()) {
                    return Err(refuse(format!(
                        "invalid value '{v}' for '--ask-for-approval <APPROVAL_POLICY>' [possible values: {}]",
                        APPROVAL_POLICIES.join(", ")
                    )));
                }
                settings.approval = Some(v);
            }
            "-C" | "--cd" => {} // already in `cwd`
            "--add-dir" => {
                return Err(refuse(format!(
                    "--add-dir {} cannot be applied to a session in the app-server; grant it under [permissions] in {}/.codex/config.toml and resume with -c default_permissions=<name>",
                    value.unwrap_or_default(),
                    cwd.display()
                )));
            }
            "-c" | "--config" => {
                let kv = value.clone().unwrap_or_default();
                let (key, val) = kv.split_once('=').unwrap_or((kv.as_str(), ""));
                let key = key.trim();
                if key == "default_permissions" {
                    let name = val.trim().trim_matches('"').trim_matches('\'').to_string();
                    if name.is_empty() {
                        return Err(refuse("-c default_permissions needs a profile name"));
                    }
                    settings.permissions = Some(name);
                } else if key.starts_with("permissions")
                    || key.starts_with("sandbox")
                    || key == "approval_policy"
                {
                    return Err(refuse(format!(
                        "-c {kv}: permission overrides cannot be applied to a session in the app-server; define the profile in {}/.codex/config.toml ([permissions.<name>]) and resume with -c default_permissions=<name>",
                        cwd.display()
                    )));
                } else {
                    out.push(a.clone());
                    if t.attached.is_none() {
                        out.push(kv);
                    }
                }
            }
            "--last" => {
                last = true;
                thread_token = Some(out.len());
                out.push(a.clone());
            }
            "--all" => {
                all = true;
                out.push(a.clone());
            }
            "--include-non-interactive" => {
                include_non_interactive = true;
                out.push(a.clone());
            }
            _ if a.starts_with('-') => {
                out.push(a.clone());
                if takes && t.attached.is_none() {
                    out.push(value.unwrap_or_default());
                }
            }
            _ => {
                if i == resume_idx {
                    subcommand_token = Some(out.len());
                    out.push(a.clone());
                } else if thread_token.is_none() && i > resume_idx {
                    // The first positional after `resume` names the session.
                    thread_token = Some(out.len());
                    out.push(a.clone());
                } else {
                    out.push(a.clone());
                }
            }
        }
        i += 1;
    }
    out.extend(args.iter().skip(terminator).cloned());

    let last_ref = ThreadRef::Last {
        all,
        include_non_interactive,
    };
    let thread = match thread_token {
        Some(idx) if out[idx] == "--last" => last_ref,
        Some(idx) if is_uuid(&out[idx]) => ThreadRef::Id(out[idx].clone()),
        Some(idx) => ThreadRef::Name(out[idx].clone()),
        None if last => last_ref,
        None => ThreadRef::Picker,
    };
    Ok(Plan {
        kind: if subcommand == "fork" {
            Kind::Fork(thread)
        } else {
            Kind::Resume(thread)
        },
        cwd,
        settings,
        codex_args: out,
        thread_token,
        subcommand_token,
        explicit_cd: explicit_cd.is_some(),
    })
}

fn verbatim(args: &[String], pwd: &Path, why: &'static str) -> Plan {
    Plan {
        kind: Kind::Verbatim(why),
        cwd: pwd.to_path_buf(),
        settings: Settings::default(),
        codex_args: args.to_vec(),
        thread_token: None,
        subcommand_token: None,
        explicit_cd: false,
    }
}

fn find_cd(args: &[String], value_flags: &BTreeSet<String>, terminator: usize) -> Option<String> {
    let mut i = 0;
    while i < terminator {
        let t = split_token(&args[i], value_flags);
        if t.flag == "-C" || t.flag == "--cd" {
            return match t.attached {
                Some(v) => Some(v),
                None => args.get(i + 1).filter(|_| i + 1 < terminator).cloned(),
            };
        }
        if value_flags.contains(t.flag) && t.attached.is_none() {
            i += 1;
        }
        i += 1;
    }
    None
}

/// With the thread resolved, what codex is given: the selector replaced by the exact
/// id. For a fork, `id` is the thread the runner already forked, so codex attaches to
/// it with `resume` — it must not fork again.
pub fn args_with_thread(plan: &Plan, id: &str) -> Vec<String> {
    let mut out = plan.codex_args.clone();
    if let (Kind::Fork(_), Some(sub)) = (&plan.kind, plan.subcommand_token) {
        out[sub] = "resume".to_string();
    }
    match plan.thread_token {
        Some(idx) => out[idx] = id.to_string(),
        // A picker resume has no token: the id goes right after the subcommand.
        None => {
            if let Some(sub) = plan.subcommand_token {
                out.insert(sub + 1, id.to_string());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }
    const ID: &str = "01a0aba7-cec2-76a1-8915-610dd6677c88";
    fn vf() -> BTreeSet<String> {
        fallback_value_flags()
    }
    fn pwd() -> PathBuf {
        std::fs::canonicalize(std::env::temp_dir()).unwrap()
    }

    #[test]
    fn value_flags_come_from_codex_help() {
        let help = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/codex-resume-help.txt"
        ))
        .unwrap();
        let f = value_flags_from_help(&help);
        for want in [
            "-c",
            "--config",
            "--add-dir",
            "-C",
            "--cd",
            "-s",
            "--sandbox",
            "-a",
            "--ask-for-approval",
            "-i",
            "--image",
            "-m",
            "--model",
        ] {
            assert!(f.contains(want), "{want} missing from {f:?}");
        }
        assert!(!f.contains("--last") && !f.contains("--all") && !f.contains("--yolo"));
    }

    #[test]
    fn csp_resume_from_a_worktree_becomes_full_access_there() {
        let p = plan(
            &args(&format!(
                "--dangerously-bypass-approvals-and-sandbox resume {ID}"
            )),
            &pwd(),
            &vf(),
        )
        .unwrap();
        assert_eq!(p.kind, Kind::Resume(ThreadRef::Id(ID.into())));
        assert_eq!(p.cwd, pwd());
        assert_eq!(
            p.settings.permissions.as_deref(),
            Some(":danger-full-access")
        );
        assert_eq!(p.settings.approval.as_deref(), Some("never"));
        assert_eq!(
            p.codex_args,
            args(&format!("resume {ID}")),
            "the refused flag is gone"
        );
        assert_eq!(args_with_thread(&p, ID), args(&format!("resume {ID}")));
        // `--yolo` is codex's alias for the same thing.
        let y = plan(&args(&format!("--yolo resume {ID}")), &pwd(), &vf()).unwrap();
        assert_eq!(y.settings, p.settings);
    }

    #[test]
    fn sandbox_approval_and_cd_are_translated_and_stripped() {
        let d = tempfile::tempdir().unwrap();
        let sub = d.path().join("wt");
        std::fs::create_dir(&sub).unwrap();
        let p = plan(
            &args(&format!(
                "resume {ID} --cd wt -s workspace-write -a on-request a prompt"
            )),
            d.path(),
            &vf(),
        )
        .unwrap();
        assert_eq!(
            p.cwd,
            std::fs::canonicalize(&sub).unwrap(),
            "relative -C is resolved against pwd"
        );
        assert!(p.explicit_cd);
        assert_eq!(p.settings.permissions.as_deref(), Some(":workspace"));
        assert_eq!(p.settings.approval.as_deref(), Some("on-request"));
        assert_eq!(p.codex_args, args(&format!("resume {ID} a prompt")));
        // Attached and `=` forms.
        let p = plan(
            &args(&format!("resume {ID} --sandbox=read-only -anever")),
            &pwd(),
            &vf(),
        )
        .unwrap();
        assert_eq!(p.settings.permissions.as_deref(), Some(":read-only"));
        assert_eq!(p.settings.approval.as_deref(), Some("never"));
        // Later flags win.
        let p = plan(
            &args(&format!("--yolo resume {ID} -s read-only")),
            &pwd(),
            &vf(),
        )
        .unwrap();
        assert_eq!(p.settings.permissions.as_deref(), Some(":read-only"));
        assert_eq!(p.settings.approval.as_deref(), Some("never"));
    }

    #[test]
    fn forwarded_flags_keep_their_exact_spelling() {
        let p = plan(
            &args(&format!(
                "resume {ID} --model=o3 -m o4 --config model=\"x\" --enable a"
            )),
            &pwd(),
            &vf(),
        )
        .unwrap();
        assert_eq!(
            p.codex_args,
            args(&format!(
                "resume {ID} --model=o3 -m o4 --config model=\"x\" --enable a"
            ))
        );
    }

    #[test]
    fn what_cannot_be_applied_is_refused_not_dropped() {
        let e = plan(
            &args(&format!(
                "resume {ID} -c permissions.x.extends=\":workspace\""
            )),
            &pwd(),
            &vf(),
        )
        .unwrap_err();
        assert!(
            e.message.contains(".codex/config.toml") && e.message.contains("default_permissions"),
            "{}",
            e.message
        );
        let e = plan(&args(&format!("resume {ID} --add-dir /d")), &pwd(), &vf()).unwrap_err();
        assert!(e.message.contains("--add-dir /d"));
        let e = plan(&args(&format!("resume {ID} -s yolo")), &pwd(), &vf()).unwrap_err();
        assert!(e.message.contains("possible values"));
        let e = plan(&args(&format!("resume {ID} -a onrequest")), &pwd(), &vf()).unwrap_err();
        assert!(e.message.contains("on-request"));
        let e = plan(&args(&format!("resume {ID} -C")), &pwd(), &vf()).unwrap_err();
        assert!(e.message.contains("value is required"));
        let e = plan(
            &args(&format!("resume {ID} -C /no/such/dir/anywhere")),
            &pwd(),
            &vf(),
        )
        .unwrap_err();
        assert!(e.message.contains("not usable"));
    }

    #[test]
    fn a_named_profile_is_a_setting() {
        let p = plan(
            &args(&format!(
                "resume {ID} -c default_permissions=\"r2c-stage1\""
            )),
            &pwd(),
            &vf(),
        )
        .unwrap();
        assert_eq!(p.settings.permissions.as_deref(), Some("r2c-stage1"));
        assert_eq!(p.codex_args, args(&format!("resume {ID}")));
    }

    #[test]
    fn help_version_and_explicit_remote_are_verbatim() {
        for a in [
            format!("--yolo resume {ID} --help"),
            format!("resume {ID} -h"),
            "--version".into(),
            format!("--remote unix:///x resume {ID} -s read-only"),
        ] {
            let p = plan(&args(&a), &pwd(), &vf()).unwrap();
            assert!(matches!(p.kind, Kind::Verbatim(_)), "{a}");
            assert_eq!(p.codex_args, args(&a));
            assert_eq!(p.settings, Settings::default());
        }
    }

    #[test]
    fn last_name_and_picker_are_recognised_and_replaceable() {
        let p = plan(&args("resume --last -s danger-full-access"), &pwd(), &vf()).unwrap();
        assert_eq!(
            p.kind,
            Kind::Resume(ThreadRef::Last {
                all: false,
                include_non_interactive: false
            })
        );
        assert_eq!(
            args_with_thread(&p, ID),
            args(&format!("resume {ID}")),
            "--last is replaced by the resolved id"
        );
        let p = plan(
            &args("resume --last --all --include-non-interactive"),
            &pwd(),
            &vf(),
        )
        .unwrap();
        assert_eq!(
            p.kind,
            Kind::Resume(ThreadRef::Last {
                all: true,
                include_non_interactive: true
            })
        );
        assert_eq!(
            args_with_thread(&p, ID),
            args(&format!("resume {ID} --all --include-non-interactive"))
        );
        let p = plan(&args("resume stage_1 --yolo"), &pwd(), &vf()).unwrap();
        assert_eq!(p.kind, Kind::Resume(ThreadRef::Name("stage_1".into())));
        assert_eq!(args_with_thread(&p, ID), args(&format!("resume {ID}")));
        let p = plan(&args("--yolo resume"), &pwd(), &vf()).unwrap();
        assert_eq!(p.kind, Kind::Resume(ThreadRef::Picker));
        assert_eq!(p.codex_args, args("resume"));
        assert_eq!(
            args_with_thread(&p, ID),
            args(&format!("resume {ID}")),
            "a picked id goes after the subcommand"
        );
        // A uuid-shaped VALUE of a flag is not the session.
        let p = plan(&args(&format!("resume -m {ID}")), &pwd(), &vf()).unwrap();
        assert_eq!(p.kind, Kind::Resume(ThreadRef::Picker));
        assert!(!is_uuid("01a0aba7"), "a prefix is not an id");
    }

    #[test]
    fn fork_is_translated_like_resume_and_attaches_with_resume() {
        let p = plan(&args(&format!("--yolo fork {ID} -C /tmp")), &pwd(), &vf()).unwrap();
        assert_eq!(p.kind, Kind::Fork(ThreadRef::Id(ID.into())));
        assert_eq!(
            p.settings.permissions.as_deref(),
            Some(":danger-full-access")
        );
        assert_eq!(p.codex_args, args(&format!("fork {ID}")));
        // The runner forks through the API; codex then attaches to the NEW thread.
        assert_eq!(args_with_thread(&p, "new-id"), args("resume new-id"));
        let p = plan(&args("fork --last"), &pwd(), &vf()).unwrap();
        assert!(matches!(p.kind, Kind::Fork(ThreadRef::Last { .. })));
        assert_eq!(args_with_thread(&p, "new-id"), args("resume new-id"));
        let p = plan(&args("fork"), &pwd(), &vf()).unwrap();
        assert_eq!(p.kind, Kind::Fork(ThreadRef::Picker));
        assert_eq!(args_with_thread(&p, "new-id"), args("resume new-id"));
    }

    #[test]
    fn double_dash_ends_options_for_us_as_for_codex() {
        // Everything after `--` is a prompt, even a flag-looking token.
        let p = plan(&args(&format!("resume {ID} -- --yolo")), &pwd(), &vf()).unwrap();
        assert_eq!(p.settings, Settings::default());
        assert_eq!(p.codex_args, args(&format!("resume {ID} -- --yolo")));
        // `-- resume` is a prompt for a NEW session, not a resume.
        let p = plan(&args("-- resume"), &pwd(), &vf()).unwrap();
        assert_eq!(p.kind, Kind::New);
    }

    #[test]
    fn a_new_session_is_left_exactly_as_typed() {
        let a = args("--dangerously-bypass-approvals-and-sandbox fix-the-bug");
        let p = plan(&a, &pwd(), &vf()).unwrap();
        assert_eq!(p.kind, Kind::New);
        assert_eq!(p.codex_args, a);
        assert!(!p.explicit_cd);
        let d = tempfile::tempdir().unwrap();
        let p = plan(
            &args(&format!("-C {} --yolo", d.path().display())),
            &pwd(),
            &vf(),
        )
        .unwrap();
        assert_eq!(p.cwd, std::fs::canonicalize(d.path()).unwrap());
        assert!(p.explicit_cd, "run must not add a second -C");
        // A flag value that happens to be `resume` is not the subcommand.
        assert_eq!(
            plan(&args("-m resume"), &pwd(), &vf()).unwrap().kind,
            Kind::New
        );
        // A new session with a bad flag value is codex's to reject, not ours.
        assert_eq!(
            plan(&args("-s yolo hi"), &pwd(), &vf()).unwrap().kind,
            Kind::New
        );
    }
}
