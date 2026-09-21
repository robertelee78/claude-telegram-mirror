//! ADR-022: the app-server's threads as the launcher needs them — pure parsing of
//! `thread/list` / `thread/read` / `thread/resume` replies, codex's own selection
//! rules for `--last` and names, and the check that what the daemon reports as in
//! effect is what was asked. Shapes were captured from codex 0.155 (2026-09-22).

use super::codex_launch::Settings;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// One thread as `thread/list` / `thread/read` describe it. Pure parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadSummary {
    pub id: String,
    pub name: Option<String>,
    /// Where it runs now: the environment cwd when one is set (a moved thread), else
    /// the cwd it was created in.
    pub cwd: PathBuf,
    pub updated_at: i64,
    pub preview: String,
    /// `active` | `idle` | `notLoaded` | `systemError`
    pub status: String,
    pub active_flags: Vec<String>,
}

pub fn parse_thread(v: &Value) -> Option<ThreadSummary> {
    let id = v.get("id")?.as_str()?.to_string();
    let env_cwd = v
        .get("environments")
        .and_then(Value::as_array)
        .and_then(|e| e.first())
        .and_then(|e| e.get("cwd"))
        .and_then(Value::as_str);
    let cwd = env_cwd
        .or_else(|| v.get("cwd").and_then(Value::as_str))
        .unwrap_or_default();
    let status = v.get("status");
    Some(ThreadSummary {
        id,
        name: v.get("name").and_then(Value::as_str).map(str::to_string),
        cwd: PathBuf::from(cwd),
        updated_at: v.get("updatedAt").and_then(Value::as_i64).unwrap_or(0),
        preview: v
            .get("preview")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        status: status
            .and_then(|s| s.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        active_flags: status
            .and_then(|s| s.get("activeFlags"))
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
    })
}

pub(super) fn same_dir(a: &Path, b: &Path) -> bool {
    a == b
        || match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
            (Ok(x), Ok(y)) => x == y,
            _ => false,
        }
}

/// `--last`: the newest thread, in `cwd` unless `all`. `threads` newest first.
pub fn pick_last<'a>(
    threads: &'a [ThreadSummary],
    cwd: &Path,
    all: bool,
) -> Option<&'a ThreadSummary> {
    threads.iter().find(|t| all || same_dir(&t.cwd, cwd))
}

/// `resume <name>`: exactly one thread must carry the name.
pub fn find_by_name<'a>(
    threads: &'a [ThreadSummary],
    name: &str,
) -> Result<&'a ThreadSummary, String> {
    let hits: Vec<&ThreadSummary> = threads
        .iter()
        .filter(|t| t.name.as_deref() == Some(name))
        .collect();
    match hits.as_slice() {
        [one] => Ok(one),
        [] => Err(format!("no session is named {name:?}")),
        many => Err(format!(
            "{} sessions are named {name:?}; resume by id: {}",
            many.len(),
            many.iter()
                .map(|t| t.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// What `thread/resume` reports as in effect. Pure parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Effective {
    pub cwd: PathBuf,
    /// `readOnly` | `workspaceWrite` | `dangerFullAccess` | … (`sandbox.type`)
    pub sandbox: String,
    pub approval: String,
}

pub fn parse_effective(v: &Value) -> Option<Effective> {
    Some(Effective {
        cwd: PathBuf::from(v.get("cwd")?.as_str()?),
        sandbox: v
            .get("sandbox")
            .and_then(|s| s.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        approval: match v.get("approvalPolicy") {
            Some(Value::String(s)) => s.clone(),
            Some(other) => other.to_string(),
            None => "unknown".into(),
        },
    })
}

/// The built-in profile a `sandbox.type` corresponds to.
pub(super) fn profile_of(sandbox: &str) -> Option<&'static str> {
    match sandbox {
        "readOnly" => Some(":read-only"),
        "workspaceWrite" => Some(":workspace"),
        "dangerFullAccess" => Some(":danger-full-access"),
        _ => None,
    }
}

/// Does what the daemon reports match what was asked? A named profile cannot be
/// told apart by sandbox type (it extends a built-in), so only the daemon's
/// acceptance of the update vouches for it; cwd and approval are always checked.
pub fn satisfied(want: &Settings, cwd: &Path, got: &Effective) -> Result<(), String> {
    if !same_dir(&got.cwd, cwd) {
        return Err(format!(
            "directory is {}, asked {}",
            got.cwd.display(),
            cwd.display()
        ));
    }
    if let Some(p) = &want.permissions {
        if p.starts_with(':') && profile_of(&got.sandbox) != Some(p.as_str()) {
            return Err(format!("sandbox is {}, asked {p}", got.sandbox));
        }
    }
    if let Some(a) = &want.approval {
        if &got.approval != a {
            return Err(format!("approval is {}, asked {a}", got.approval));
        }
    }
    Ok(())
}

pub fn short(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

fn ago(epoch: i64) -> String {
    let now = chrono::Utc::now().timestamp();
    let d = (now - epoch).max(0);
    match d {
        0..=59 => format!("{d}s ago"),
        60..=3599 => format!("{}m ago", d / 60),
        3600..=86399 => format!("{}h ago", d / 3600),
        _ => format!("{}d ago", d / 86400),
    }
}

pub fn describe(t: &ThreadSummary) -> String {
    let preview: String = t.preview.chars().take(48).collect();
    format!(
        "{}  {:<14} {}  {}  {}",
        short(&t.id),
        t.name.as_deref().unwrap_or("-"),
        t.cwd.display(),
        ago(t.updated_at),
        preview.replace('\n', " ")
    )
}

pub fn describe_effective(e: &Effective) -> String {
    let perms = match profile_of(&e.sandbox) {
        Some(":read-only") => "Read Only".to_string(),
        Some(":workspace") => "Workspace".to_string(),
        Some(":danger-full-access") => "Full Access".to_string(),
        _ => e.sandbox.clone(),
    };
    format!(
        "directory {}, {perms}, approval {}",
        e.cwd.display(),
        e.approval
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn t(id: &str, name: Option<&str>, cwd: &str, updated: i64) -> ThreadSummary {
        ThreadSummary {
            id: id.into(),
            name: name.map(str::to_string),
            cwd: PathBuf::from(cwd),
            updated_at: updated,
            preview: "p".into(),
            status: "idle".into(),
            active_flags: vec![],
        }
    }

    #[test]
    fn a_listed_thread_is_read_as_the_daemon_describes_it() {
        // Shape captured from `thread/list` on codex 0.155 (2026-09-22): a thread
        // moved to a worktree keeps its original `cwd` and carries the new one in
        // `environments[0].cwd`.
        let v = json!({
            "id": "01a0aba7-cec2-76a1-8915-610dd6677c88",
            "environments": [{"environmentId": "local", "cwd": "/opt/wt", "runtimeWorkspaceRoots": ["/opt/wt"]}],
            "cwd": "/opt/repo", "name": "stage_1", "updatedAt": 1790029589,
            "preview": "we're picking up", "status": {"type": "active", "activeFlags": ["waitingOnApproval"]}
        });
        let s = parse_thread(&v).unwrap();
        assert_eq!(
            s.cwd,
            PathBuf::from("/opt/wt"),
            "the environment cwd is where it runs"
        );
        assert_eq!(s.status, "active");
        assert_eq!(s.active_flags, vec!["waitingOnApproval"]);
        assert_eq!(s.name.as_deref(), Some("stage_1"));
        let plain = parse_thread(&json!({"id": "x", "cwd": "/a", "status": {"type": "notLoaded"}}))
            .unwrap();
        assert_eq!(plain.cwd, PathBuf::from("/a"));
        assert_eq!(plain.status, "notLoaded");
        assert!(
            parse_thread(&json!({"cwd": "/a"})).is_none(),
            "no id, no thread"
        );
    }

    #[test]
    fn last_is_directory_scoped_unless_all() {
        let threads = vec![
            t("newest", None, "/other", 30),
            t("mine", None, "/here", 20),
            t("older", None, "/here", 10),
        ];
        assert_eq!(
            pick_last(&threads, Path::new("/here"), false).unwrap().id,
            "mine"
        );
        assert_eq!(
            pick_last(&threads, Path::new("/here"), true).unwrap().id,
            "newest"
        );
        assert!(pick_last(&threads, Path::new("/nowhere"), false).is_none());
    }

    #[test]
    fn a_name_must_match_exactly_one_thread() {
        let threads = vec![
            t("a", Some("stage_1"), "/x", 3),
            t("b", Some("stage_1"), "/y", 2),
            t("c", Some("other"), "/z", 1),
        ];
        assert_eq!(find_by_name(&threads, "other").unwrap().id, "c");
        let e = find_by_name(&threads, "stage_1").unwrap_err();
        assert!(e.contains("2 sessions") && e.contains("a, b"), "{e}");
        assert!(find_by_name(&threads, "nope")
            .unwrap_err()
            .contains("no session is named"));
    }

    #[test]
    fn effective_settings_are_checked_against_what_was_asked() {
        // Shape captured from `thread/resume` (2026-09-22).
        let v = json!({"cwd": "/tmp/w2", "approvalPolicy": "never", "sandbox": {"type": "dangerFullAccess"}, "thread": {}});
        let got = parse_effective(&v).unwrap();
        let want = Settings {
            permissions: Some(":danger-full-access".into()),
            approval: Some("never".into()),
        };
        assert!(satisfied(&want, Path::new("/tmp/w2"), &got).is_ok());
        assert!(satisfied(&want, Path::new("/tmp/w1"), &got)
            .unwrap_err()
            .contains("directory"));
        let ro = Settings {
            permissions: Some(":read-only".into()),
            approval: None,
        };
        assert!(satisfied(&ro, Path::new("/tmp/w2"), &got)
            .unwrap_err()
            .contains("sandbox"));
        let ask = Settings {
            permissions: None,
            approval: Some("on-request".into()),
        };
        assert!(satisfied(&ask, Path::new("/tmp/w2"), &got)
            .unwrap_err()
            .contains("approval"));
        // A named profile extends a built-in: the sandbox type cannot refute it.
        let named = Settings {
            permissions: Some("r2c-stage1".into()),
            approval: None,
        };
        assert!(satisfied(&named, Path::new("/tmp/w2"), &got).is_ok());
        assert!(describe_effective(&got).contains("Full Access"));
    }

    #[test]
    fn the_picker_line_is_short_and_says_where_and_when() {
        let s = describe(&t(
            "01a0aba7-cec2-76a1-8915-610dd6677c88",
            Some("stage_1"),
            "/opt/wt",
            chrono::Utc::now().timestamp() - 120,
        ));
        assert!(s.starts_with("01a0aba7  stage_1"), "{s}");
        assert!(s.contains("/opt/wt") && s.contains("2m ago"), "{s}");
    }
}
