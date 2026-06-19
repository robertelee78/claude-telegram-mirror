//! Hook Installer — programmatic `settings.json` modification.
//!
//! Ported from `src/hooks/installer.ts`.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const HOOK_TYPES: &[&str] = &[
    "PreToolUse",
    "PostToolUse",
    "Notification",
    "Stop",
    "UserPromptSubmit",
    "PreCompact",
    // ADR-014 A1: SessionEnd activates the already-built event-driven teardown
    // handler (types.rs / hook.rs / socket_handlers.rs). Uses the standard
    // create_hook_entry() with no custom timeout (unlike PreToolUse).
    "SessionEnd",
];

fn home_dir() -> PathBuf {
    crate::config::home_dir()
}

fn global_claude_dir() -> PathBuf {
    home_dir().join(".claude")
}

fn global_settings_path() -> PathBuf {
    global_claude_dir().join("settings.json")
}

// ---------------------------------------------------------------------------
// Hook scopes (Claude Code merges hooks across all three; identical command
// strings are deduplicated at runtime, but DIFFERING ctm command strings across
// scopes — or within one file — double-fire. We therefore keep ctm's hook in
// exactly ONE scope and detect/clean the others.)
// ---------------------------------------------------------------------------

/// The three file-based settings scopes Claude Code reads, in coverage order
/// (broadest first). `project_base` is the directory containing `.claude/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HookScope {
    /// `~/.claude/settings.json` — applies to every project.
    Global,
    /// `<project>/.claude/settings.json` — committable, single project.
    Project,
    /// `<project>/.claude/settings.local.json` — gitignored, single project.
    Local,
}

impl HookScope {
    pub(crate) fn label(self) -> &'static str {
        match self {
            HookScope::Global => "global",
            HookScope::Project => "project",
            HookScope::Local => "local (settings.local.json)",
        }
    }
}

fn project_settings_path(base: &Path) -> PathBuf {
    base.join(".claude").join("settings.json")
}

fn local_settings_path(base: &Path) -> PathBuf {
    base.join(".claude").join("settings.local.json")
}

/// Resolve the settings file path for a scope. Project/Local resolve against
/// `base` (the project directory, typically cwd).
fn scope_settings_path(scope: HookScope, base: &Path) -> PathBuf {
    match scope {
        HookScope::Global => global_settings_path(),
        HookScope::Project => project_settings_path(base),
        HookScope::Local => local_settings_path(base),
    }
}

/// Count ctm hook COMMANDS (inner-level) for a hook type in a settings Value.
/// New-format items may hold several inner hooks; each CTM inner command counts,
/// so two duplicates inside one item are both detected.
fn ctm_command_count(settings: &Value, hook_type: &str) -> usize {
    settings
        .get("hooks")
        .and_then(|h| h.get(hook_type))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|item| {
                    if let Some(inner) = item.get("hooks").and_then(|v| v.as_array()) {
                        inner
                            .iter()
                            .filter(|h| {
                                h.get("command")
                                    .and_then(|c| c.as_str())
                                    .map(is_ctm_command)
                                    .unwrap_or(false)
                            })
                            .count()
                    } else if item
                        .get("command")
                        .and_then(|c| c.as_str())
                        .map(is_ctm_command)
                        .unwrap_or(false)
                    {
                        1
                    } else {
                        0
                    }
                })
                .sum()
        })
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Settings I/O
// ---------------------------------------------------------------------------

fn read_settings(path: &Path) -> Value {
    if !path.exists() {
        return serde_json::json!({});
    }
    match fs::read_to_string(path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({})),
        Err(_) => serde_json::json!({}),
    }
}

fn write_settings(path: &Path, settings: &Value) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent)?;
        }
    }
    let content = serde_json::to_string_pretty(settings)?;
    fs::write(path, content)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// CTM binary path detection
// ---------------------------------------------------------------------------

fn ctm_hook_command() -> String {
    // Use the current binary's path for the hook command
    match std::env::current_exe() {
        Ok(exe) => format!("\"{}\" hook", exe.display()),
        Err(_) => "ctm hook".into(),
    }
}

// ---------------------------------------------------------------------------
// Hook identification
// ---------------------------------------------------------------------------

/// Check if a hook command string belongs to CTM.
///
/// Matches when "ctm hook" appears as a literal substring, when the binary component
/// of the command is exactly `ctm` (optionally followed by a hash suffix used by
/// Rust test binaries, e.g. `ctm-abc123 hook`), or when legacy patterns are present.
///
/// This avoids false positives from tools like `xctm-linter` where "ctm" is embedded
/// inside a longer identifier word.
pub(crate) fn is_ctm_command(cmd: &str) -> bool {
    if cmd.contains("telegram-hook") || cmd.contains("hooks/handler") {
        return true;
    }
    // Extract the binary name: strip optional surrounding quotes from the first
    // token (handles both unquoted `/path/to/ctm hook` and quoted
    // `"/path/to/ctm" hook`), then take the basename after the last '/'.
    let first_token = cmd.split_whitespace().next().unwrap_or("");
    let unquoted = first_token.trim_matches('"');
    let bin_name = unquoted.rsplit('/').next().unwrap_or(unquoted);

    // Accept if the binary name is exactly "ctm" or the full binary name.
    if bin_name == "ctm"
        || bin_name == "claude-telegram-mirror"
        || bin_name.starts_with("claude_telegram_mirror")
        || bin_name.starts_with("claude-telegram-mirror")
    {
        return true;
    }
    // Rust test binary naming is `ctm-<hex hash>` (e.g. `ctm-1a2b3c4d`). Accept that
    // specific shape, but NOT an arbitrary `ctm-wrapper` (which is a different tool).
    if let Some(suffix) = bin_name.strip_prefix("ctm-") {
        if !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_hexdigit()) {
            return true;
        }
    }

    // Accept "ctm hook" or the full binary name appearing at a TOKEN BOUNDARY
    // (e.g. `npx ctm hook`, `sh -c "ctm hook"`), but not embedded in a longer
    // word like `xctm hook` / `my-ctm-wrapper`.
    if contains_token(cmd, "ctm hook") || contains_token(cmd, "claude-telegram-mirror") {
        return true;
    }

    false
}

/// True if `needle` occurs in `cmd` immediately after a token boundary — the
/// preceding char is start-of-string or not an identifier char (alphanumeric /
/// `_` / `-`). Avoids matching `ctm` embedded inside `xctm` / `my-ctm-tool`.
fn contains_token(cmd: &str, needle: &str) -> bool {
    let bytes = cmd.as_bytes();
    let mut from = 0;
    while let Some(rel) = cmd[from..].find(needle) {
        let at = from + rel;
        let boundary = at == 0 || {
            let p = bytes[at - 1];
            !(p.is_ascii_alphanumeric() || p == b'_' || p == b'-')
        };
        if boundary {
            return true;
        }
        from = at + 1;
    }
    false
}

/// Check if a hook item (in the new format) contains a CTM command.
pub(crate) fn item_is_ctm(item: &Value) -> bool {
    // New format: { "matcher": "", "hooks": [{ "type": "command", "command": "..." }] }
    if let Some(hooks_arr) = item.get("hooks").and_then(|v| v.as_array()) {
        return hooks_arr.iter().any(|h| {
            h.get("command")
                .and_then(|c| c.as_str())
                .map(is_ctm_command)
                .unwrap_or(false)
        });
    }
    // Old format: { "type": "command", "command": "..." }
    if let Some(cmd) = item.get("command").and_then(|c| c.as_str()) {
        return is_ctm_command(cmd);
    }
    false
}

// ---------------------------------------------------------------------------
// Hook entry creation
// ---------------------------------------------------------------------------

fn create_hook_entry(command: &str) -> Value {
    serde_json::json!({
        "matcher": "",
        "hooks": [
            {
                "type": "command",
                "command": command
            }
        ]
    })
}

/// Create a hook entry with a custom timeout (for PreToolUse approval workflow).
fn create_hook_entry_with_timeout(command: &str, timeout: u32) -> Value {
    serde_json::json!({
        "matcher": "",
        "hooks": [
            {
                "type": "command",
                "command": command,
                "timeout": timeout
            }
        ]
    })
}

// ---------------------------------------------------------------------------
// Change reporting
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Clone, Copy)]
// `#[non_exhaustive]`: this status set may grow (Skipped was added for cross-scope
// installs); downstream matches must include a wildcard arm rather than break.
#[non_exhaustive]
pub enum HookChangeStatus {
    Added,
    Updated,
    Unchanged,
    /// Cross-scope: a ctm hook of this type already exists in another scope, so
    /// installing here was skipped to avoid double-firing.
    Skipped,
}

impl std::fmt::Display for HookChangeStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Added => write!(f, "added"),
            Self::Updated => write!(f, "updated"),
            Self::Unchanged => write!(f, "unchanged"),
            Self::Skipped => write!(f, "skipped"),
        }
    }
}

// Intentional: public API for library consumers
pub struct HookChangeReport {
    pub hook_type: String,
    pub status: HookChangeStatus,
    /// Human-readable description of what changed (e.g. "added ctm hook",
    /// "updated from old path", "no changes").
    #[allow(dead_code)] // Library API
    pub details: String,
}

// ---------------------------------------------------------------------------
// Install
// ---------------------------------------------------------------------------

/// Install CTM hooks for a specific project directory.
///
/// `project_path` must be the root of a Claude project (i.e. it must contain a `.claude/`
/// subdirectory). This avoids mutating global process state via `set_current_dir`.
/// Install ctm hooks into a specific project directory (project scope).
/// Back-compat signature; delegates with `force = false`.
#[allow(dead_code)] // Library API
pub fn install_hooks_for_project(project_path: &Path) -> anyhow::Result<()> {
    install_hooks_for_project_forced(project_path, false)
}

/// As [`install_hooks_for_project`] with an explicit `force` flag (bypass the
/// cross-scope skip).
#[allow(dead_code)] // Library API
pub fn install_hooks_for_project_forced(project_path: &Path, force: bool) -> anyhow::Result<()> {
    let project_dir = project_path.join(".claude");
    if !project_dir.exists() {
        anyhow::bail!(
            "No .claude directory found in {}. Run from a Claude project directory.",
            project_path.display()
        );
    }
    let settings_path = project_dir.join("settings.json");
    let (changes, warnings) = install_hooks_to_path(
        &settings_path,
        &project_dir,
        HookScope::Project,
        project_path,
        force,
    )?;

    println!("\nClaude Code Telegram Hook Installation (project)\n");
    println!("Settings: {}\n", settings_path.display());

    print_install_results(&changes, &warnings);

    Ok(())
}

/// Install CTM hooks into the given settings file.
///
/// Returns the list of changes made. Idempotent: running twice produces
/// no changes on the second invocation.
///
/// When `custom_path` is provided it is used instead of `current_dir()` for
/// project-mode installations, avoiding reliance on mutable process state.
pub fn install_hooks(project: bool) -> anyhow::Result<()> {
    install_hooks_with_path(project, None)
}

/// Like `install_hooks` but accepts an optional explicit project directory.
/// (Back-compat signature; delegates with `force = false`.)
pub fn install_hooks_with_path(project: bool, custom_path: Option<&Path>) -> anyhow::Result<()> {
    install_hooks_with_path_forced(project, custom_path, false)
}

/// As [`install_hooks_with_path`] with an explicit `force` flag (bypass the
/// cross-scope skip for project installs).
pub fn install_hooks_with_path_forced(
    project: bool,
    custom_path: Option<&Path>,
    force: bool,
) -> anyhow::Result<()> {
    let base = match custom_path {
        Some(p) => p.to_path_buf(),
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };
    let (settings_path, config_dir, scope) = if project {
        let project_dir = base.join(".claude");
        if !project_dir.exists() {
            anyhow::bail!(
                "No .claude directory found in {}. Run from a Claude project directory.",
                base.display()
            );
        }
        (
            project_dir.join("settings.json"),
            project_dir,
            HookScope::Project,
        )
    } else {
        (
            global_settings_path(),
            global_claude_dir(),
            HookScope::Global,
        )
    };

    let (changes, warnings) =
        install_hooks_to_path(&settings_path, &config_dir, scope, &base, force)?;

    // Print results
    let location = if project { "project" } else { "global" };
    println!("\nClaude Code Telegram Hook Installation ({location})\n");
    println!("Settings: {}\n", settings_path.display());

    print_install_results(&changes, &warnings);

    Ok(())
}

/// Print install results with emoji indicators and a restart reminder.
fn print_install_results(changes: &[HookChangeReport], warnings: &[String]) {
    let mut any_changed = false;
    let mut any_skipped = false;
    for report in changes {
        let icon = match report.status {
            HookChangeStatus::Added => "\u{2705}",     // checkmark
            HookChangeStatus::Updated => "\u{1F504}",  // arrows
            HookChangeStatus::Unchanged => "\u{2713}", // simple check
            HookChangeStatus::Skipped => "\u{23ED}",   // skip
        };
        let label = match report.status {
            HookChangeStatus::Added => "Added",
            HookChangeStatus::Updated => "Updated",
            HookChangeStatus::Unchanged => "Already correct",
            HookChangeStatus::Skipped => "Skipped",
        };
        if report.status == HookChangeStatus::Skipped {
            println!(
                "  {icon} {}: {label} \u{2014} {}",
                report.hook_type, report.details
            );
            any_skipped = true;
        } else {
            println!("  {icon} {}: {label}", report.hook_type);
        }
        if matches!(
            report.status,
            HookChangeStatus::Added | HookChangeStatus::Updated
        ) {
            any_changed = true;
        }
    }
    println!();

    for w in warnings {
        println!("\u{26A0}\u{FE0F}  {w}");
    }
    if !warnings.is_empty() {
        println!();
    }

    if any_changed {
        println!("Hooks installed successfully.");
        println!("\u{1F4A1} Restart Claude Code to activate changes.");
    } else if any_skipped {
        println!("Nothing installed — hooks already present in another scope (see above).");
    } else {
        println!("All hooks already up to date.");
    }
    println!();
}

/// Strip ctm command(s) from a single hook item, PRESERVING any non-ctm inner
/// hooks. Returns `None` if the item should be dropped entirely (it was wholly
/// ctm, or its inner hooks became empty). Operates at the inner-command level so
/// an item mixing ctm + user hooks never loses the user's hooks.
fn strip_ctm_from_item(item: &Value) -> Option<Value> {
    // New format: { matcher, hooks: [ {type, command}, ... ] }
    if let Some(inner) = item.get("hooks").and_then(|v| v.as_array()) {
        let kept: Vec<Value> = inner
            .iter()
            .filter(|h| {
                !h.get("command")
                    .and_then(|c| c.as_str())
                    .map(is_ctm_command)
                    .unwrap_or(false)
            })
            .cloned()
            .collect();
        if kept.is_empty() {
            return None;
        }
        if kept.len() == inner.len() {
            return Some(item.clone()); // no ctm inner — unchanged
        }
        let mut new_item = item.clone();
        new_item["hooks"] = Value::Array(kept);
        return Some(new_item);
    }
    // Old format: { type, command }
    if item
        .get("command")
        .and_then(|c| c.as_str())
        .map(is_ctm_command)
        .unwrap_or(false)
    {
        return None;
    }
    Some(item.clone())
}

/// Desired array for a hook type: exactly one canonical ctm entry first, then
/// every non-ctm hook (inner-level preserved), in original order.
fn desired_hook_array(existing: Option<&Value>, expected: &Value) -> Vec<Value> {
    let mut out = vec![expected.clone()];
    if let Some(arr) = existing.and_then(|v| v.as_array()) {
        for item in arr {
            if let Some(stripped) = strip_ctm_from_item(item) {
                out.push(stripped);
            }
        }
    }
    out
}

/// Array with ALL ctm hooks removed (inner-level), preserving non-ctm hooks.
/// Used by uninstall and by `ctm doctor --fix` cross-scope cleanup.
fn array_without_ctm(existing: Option<&Value>) -> Vec<Value> {
    let mut out = Vec::new();
    if let Some(arr) = existing.and_then(|v| v.as_array()) {
        for item in arr {
            if let Some(stripped) = strip_ctm_from_item(item) {
                out.push(stripped);
            }
        }
    }
    out
}

/// Which OTHER scopes currently hold a ctm hook for `hook_type` (any command
/// form, not just the canonical one — a differing command string is exactly the
/// case Claude's runtime dedup misses and double-fires).
/// Resolve the (scope, settings-path) list for a project `base`, broadest first.
/// Production entry point; tests build their own list with isolated paths.
pub(crate) fn resolve_scopes(base: &Path) -> Vec<(HookScope, PathBuf)> {
    [HookScope::Global, HookScope::Project, HookScope::Local]
        .into_iter()
        .map(|s| (s, scope_settings_path(s, base)))
        .collect()
}

fn other_scopes_with_ctm(
    scope: HookScope,
    scopes: &[(HookScope, PathBuf)],
    hook_type: &str,
) -> Vec<HookScope> {
    scopes
        .iter()
        .filter(|(s, _)| *s != scope)
        .filter(|(_, path)| ctm_command_count(&read_settings(path), hook_type) > 0)
        .map(|(s, _)| *s)
        .collect()
}

/// Classify the change for a hook type given the existing array and the desired
/// array. Structural (not command-only): any matcher/format/timeout/order/count
/// difference vs. the desired single-canonical-entry shape is a change.
fn classify_change(existing: Option<&Value>, desired: &[Value]) -> HookChangeStatus {
    match existing.and_then(|v| v.as_array()) {
        Some(arr) if arr.as_slice() == desired => HookChangeStatus::Unchanged,
        Some(arr) => {
            if arr.iter().any(item_is_ctm) {
                HookChangeStatus::Updated
            } else {
                HookChangeStatus::Added
            }
        }
        None => HookChangeStatus::Added,
    }
}

/// Core logic: install hooks to a specific settings path/scope.
///
/// `scope`/`base` drive cross-scope conflict handling: project/local installs
/// SKIP a hook type already present in another scope (it would merge/double-fire),
/// unless `force`; global installs proceed but return warnings. Returns
/// `(per-type change reports, cross-scope warnings)`.
fn install_hooks_to_path(
    settings_path: &Path,
    config_dir: &Path,
    scope: HookScope,
    base: &Path,
    force: bool,
) -> anyhow::Result<(Vec<HookChangeReport>, Vec<String>)> {
    let command = ctm_hook_command();
    let mut settings = read_settings(settings_path);
    let mut changes = Vec::new();
    let mut warnings = Vec::new();
    let mut config_changed = false;

    // Ensure `hooks` is an OBJECT before any `settings["hooks"][type] = ..` write.
    // A malformed-but-valid settings file (e.g. `"hooks": []` or `"hooks": "x"`)
    // would otherwise panic on string-indexing a non-object. A non-object hooks
    // holds no recoverable per-event entries, so replacing it is safe.
    if !settings.get("hooks").map(Value::is_object).unwrap_or(false) {
        settings["hooks"] = serde_json::json!({});
    }

    // ADR-015: PreToolUse timeout derived from the tool-approval wait + buffer
    // (single source of truth in config), so the approval hook is never cancelled
    // before its own send_and_wait completes. Default 300 + 10 = 310 (unchanged).
    let pretooluse_timeout = crate::config::DEFAULT_APPROVAL_WAIT_SECS
        + crate::config::APPROVAL_HOOK_TIMEOUT_BUFFER_SECS;
    let scopes = resolve_scopes(base);
    for &hook_type in HOOK_TYPES {
        let expected = if hook_type == "PreToolUse" {
            create_hook_entry_with_timeout(&command, pretooluse_timeout)
        } else {
            create_hook_entry(&command)
        };
        // Clone out so we can mutate `settings` below without an active borrow.
        let existing = settings["hooks"].get(hook_type).cloned();

        // Cross-scope conflict: hooks MERGE across global/project/local and only
        // byte-identical command strings dedup at runtime, so a ctm hook of this
        // type in another scope risks double-firing.
        let conflicts = other_scopes_with_ctm(scope, &scopes, hook_type);
        if !conflicts.is_empty() {
            let labels: Vec<&str> = conflicts.iter().map(|s| s.label()).collect();
            match scope {
                HookScope::Project | HookScope::Local if !force => {
                    changes.push(HookChangeReport {
                        hook_type: hook_type.to_string(),
                        status: HookChangeStatus::Skipped,
                        details: format!(
                            "already present in {} scope — skipped to avoid double-firing (run `ctm doctor --fix` to consolidate, or `--force` to install anyway)",
                            labels.join(", ")
                        ),
                    });
                    continue;
                }
                HookScope::Global => {
                    warnings.push(format!(
                        "{hook_type}: ctm hooks also present in {} scope; hooks merge across scopes — run `ctm doctor --fix` to consolidate",
                        labels.join(", ")
                    ));
                }
                _ => {} // forced project/local install: fall through
            }
        }

        let desired = desired_hook_array(existing.as_ref(), &expected);
        let status = classify_change(existing.as_ref(), &desired);
        let prior_ctm = existing
            .as_ref()
            .and_then(|v| v.as_array())
            .map(|a| a.iter().map(ctm_inner_count).sum::<usize>())
            .unwrap_or(0);
        let details = match status {
            HookChangeStatus::Added => "added ctm hook".to_string(),
            HookChangeStatus::Updated if prior_ctm > 1 => {
                format!("consolidated {prior_ctm} ctm entries into one",)
            }
            HookChangeStatus::Updated => "updated ctm hook".to_string(),
            HookChangeStatus::Unchanged => "no changes".to_string(),
            HookChangeStatus::Skipped => "skipped".to_string(),
        };
        changes.push(HookChangeReport {
            hook_type: hook_type.to_string(),
            status,
            details,
        });

        if status != HookChangeStatus::Unchanged {
            config_changed = true;
            settings["hooks"][hook_type] = Value::Array(desired);
        }
    }

    if config_changed {
        // Ensure config dir exists
        if !config_dir.exists() {
            fs::create_dir_all(config_dir)?;
        }
        write_settings(settings_path, &settings)?;
    }

    Ok((changes, warnings))
}

/// Count ctm inner commands within a single hook item (both formats).
fn ctm_inner_count(item: &Value) -> usize {
    if let Some(inner) = item.get("hooks").and_then(|v| v.as_array()) {
        return inner
            .iter()
            .filter(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    .map(is_ctm_command)
                    .unwrap_or(false)
            })
            .count();
    }
    if item
        .get("command")
        .and_then(|c| c.as_str())
        .map(is_ctm_command)
        .unwrap_or(false)
    {
        1
    } else {
        0
    }
}

/// Classify what installing `expected` would do to `existing` (structural):
/// Unchanged only when `existing` already equals the desired single-canonical
/// shape (so a right-command-but-wrong-timeout/matcher/format or a duplicated
/// ctm entry is correctly reported as a change). Thin wrapper over the
/// desired-array + structural-compare used by `install_hooks_to_path`.
#[cfg(test)]
fn determine_change_status(existing: Option<&Value>, expected: &Value) -> HookChangeStatus {
    let desired = desired_hook_array(existing, expected);
    classify_change(existing, &desired)
}

// ---------------------------------------------------------------------------
// Uninstall
// ---------------------------------------------------------------------------

/// Remove ALL ctm hooks (inner-level, preserving non-ctm hooks) from a single
/// settings FILE in place. Returns the hook types a ctm hook was removed from.
/// Writes only if something changed; deletes emptied arrays and an emptied
/// `hooks` object. Shared by uninstall and `ctm doctor --fix`.
pub(crate) fn remove_ctm_from_file(settings_path: &Path) -> anyhow::Result<Vec<String>> {
    let original = read_settings(settings_path);
    let mut settings = original.clone();
    let mut removed = Vec::new();

    if let Some(obj) = settings.get_mut("hooks").and_then(|h| h.as_object_mut()) {
        let keys: Vec<String> = obj.keys().cloned().collect();
        for key in keys {
            if ctm_command_count(&original, &key) == 0 {
                continue;
            }
            removed.push(key.clone());
            let kept = array_without_ctm(obj.get(&key));
            if kept.is_empty() {
                obj.remove(&key);
            } else {
                obj.insert(key, Value::Array(kept));
            }
        }
        if obj.is_empty() {
            settings.as_object_mut().map(|root| root.remove("hooks"));
        }
    }

    if !removed.is_empty() {
        write_settings(settings_path, &settings)?;
    }
    Ok(removed)
}

/// Uninstall ctm hooks from the GLOBAL scope (public API; back-compat wrapper).
#[allow(dead_code)] // Library API / back-compat (CLI uses uninstall_hooks_scoped)
pub fn uninstall_hooks() -> anyhow::Result<()> {
    uninstall_hooks_scoped(false)
}

/// Uninstall ctm hooks from a scope. `project = true` removes from BOTH the
/// project `settings.json` and `settings.local.json` (the two project-scope
/// files Claude merges); `project = false` removes from global.
pub fn uninstall_hooks_scoped(project: bool) -> anyhow::Result<()> {
    println!("\nClaude Code Telegram Hook Uninstall\n");
    let mut any = false;

    if project {
        let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        for scope in [HookScope::Project, HookScope::Local] {
            let path = scope_settings_path(scope, &base);
            if !path.exists() {
                continue;
            }
            let removed = remove_ctm_from_file(&path)?;
            if !removed.is_empty() {
                any = true;
                println!("Removed from {} ({}):", scope.label(), path.display());
                for ht in &removed {
                    println!("  - {ht}");
                }
            }
        }
    } else {
        let path = global_settings_path();
        let removed = remove_ctm_from_file(&path)?;
        if !removed.is_empty() {
            any = true;
            println!("Removed from global ({}):", path.display());
            for ht in &removed {
                println!("  - {ht}");
            }
        }
    }

    if !any {
        println!("No CTM hooks found to remove.");
    }
    println!();
    Ok(())
}

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Cross-scope diagnosis + consolidation (shared by `ctm hooks` and `ctm doctor`)
// ---------------------------------------------------------------------------

/// ctm presence in one scope for one hook type.
pub(crate) struct ScopePresence {
    pub scope: HookScope,
    pub path: PathBuf,
    /// Number of ctm command entries (inner-level) for this hook type here.
    pub count: usize,
}

/// Per-hook-type ctm presence across global/project/local for a project `base`.
pub(crate) struct HookDiagnosis {
    /// (hook_type, scopes that contain >=1 ctm command for it).
    pub per_type: Vec<(String, Vec<ScopePresence>)>,
}

impl HookDiagnosis {
    /// Hook types present in more than one scope (would merge → double-fire risk).
    pub(crate) fn cross_scope_dups(&self) -> Vec<&str> {
        self.per_type
            .iter()
            .filter(|(_, p)| p.len() > 1)
            .map(|(t, _)| t.as_str())
            .collect()
    }

    /// Hook types with more than one ctm command entry inside a single scope.
    pub(crate) fn in_file_dups(&self) -> Vec<&str> {
        self.per_type
            .iter()
            .filter(|(_, p)| p.iter().any(|s| s.count > 1))
            .map(|(t, _)| t.as_str())
            .collect()
    }

    pub(crate) fn is_clean(&self) -> bool {
        self.cross_scope_dups().is_empty() && self.in_file_dups().is_empty()
    }
}

/// Diagnose ctm hook presence across the given (scope, path) list (broadest
/// first). Build the list with [`resolve_scopes`] in production; tests inject
/// isolated paths.
pub(crate) fn diagnose_hooks(scopes: &[(HookScope, PathBuf)]) -> HookDiagnosis {
    let mut per_type = Vec::new();
    for &hook_type in HOOK_TYPES {
        let mut present = Vec::new();
        for (scope, path) in scopes {
            let count = ctm_command_count(&read_settings(path), hook_type);
            if count > 0 {
                present.push(ScopePresence {
                    scope: *scope,
                    path: path.clone(),
                    count,
                });
            }
        }
        if !present.is_empty() {
            per_type.push((hook_type.to_string(), present));
        }
    }
    HookDiagnosis { per_type }
}

/// Remove ctm hooks for ONE hook type from a single settings file (inner-level).
/// Returns true if the file changed.
fn remove_ctm_hook_type_from_file(path: &Path, hook_type: &str) -> anyhow::Result<bool> {
    let mut settings = read_settings(path);
    if ctm_command_count(&settings, hook_type) == 0 {
        return Ok(false);
    }
    let existing = settings["hooks"].get(hook_type).cloned();
    let kept = array_without_ctm(existing.as_ref());
    if let Some(obj) = settings.get_mut("hooks").and_then(|h| h.as_object_mut()) {
        if kept.is_empty() {
            obj.remove(hook_type);
        } else {
            obj.insert(hook_type.to_string(), Value::Array(kept));
        }
        if obj.is_empty() {
            settings.as_object_mut().map(|r| r.remove("hooks"));
        }
    }
    write_settings(path, &settings)?;
    Ok(true)
}

/// Normalize the ctm hook for one type in a single scope to the canonical single
/// entry (collapses in-file duplicates and stale/old-format forms). Returns true
/// if the file changed.
fn normalize_ctm_hook_type_in_file(path: &Path, hook_type: &str) -> anyhow::Result<bool> {
    let mut settings = read_settings(path);
    let command = ctm_hook_command();
    let expected = if hook_type == "PreToolUse" {
        let t = crate::config::DEFAULT_APPROVAL_WAIT_SECS
            + crate::config::APPROVAL_HOOK_TIMEOUT_BUFFER_SECS;
        create_hook_entry_with_timeout(&command, t)
    } else {
        create_hook_entry(&command)
    };
    let existing = settings
        .get("hooks")
        .and_then(|h| h.get(hook_type))
        .cloned();
    let desired = desired_hook_array(existing.as_ref(), &expected);
    if existing.as_ref().and_then(|v| v.as_array()) == Some(&desired) {
        return Ok(false);
    }
    // Ensure `hooks` is an object before the indexed write (see install guard).
    if !settings.get("hooks").map(Value::is_object).unwrap_or(false) {
        settings["hooks"] = serde_json::json!({});
    }
    settings["hooks"][hook_type] = Value::Array(desired);
    write_settings(path, &settings)?;
    Ok(true)
}

/// Consolidate ctm hooks to a single canonical scope + single entry per type.
///
/// Policy: for each hook type present in multiple scopes, KEEP the broadest scope
/// (global > project > local) and remove the ctm hook from the narrower ones; then
/// normalize the kept scope to a single canonical entry (collapsing any in-file
/// duplicates / stale forms). Non-ctm hooks are always preserved. Returns a
/// human-readable list of the changes made.
pub(crate) fn consolidate_hooks(scopes: &[(HookScope, PathBuf)]) -> anyhow::Result<Vec<String>> {
    let mut log = Vec::new();
    let diag = diagnose_hooks(scopes);
    for (hook_type, present) in &diag.per_type {
        // present is ordered global, project, local by construction → [0] is broadest.
        let keeper = present[0].scope;
        for narrower in &present[1..] {
            if remove_ctm_hook_type_from_file(&narrower.path, hook_type)? {
                log.push(format!(
                    "{hook_type}: removed duplicate from {} scope",
                    narrower.scope.label()
                ));
            }
        }
        let keeper_path = &present[0].path;
        let keeper_count = present[0].count;
        if normalize_ctm_hook_type_in_file(keeper_path, hook_type)? && keeper_count > 1 {
            log.push(format!(
                "{hook_type}: collapsed {keeper_count} duplicate entries in {} scope",
                keeper.label()
            ));
        }
    }
    Ok(log)
}

/// Programmatic hook status report (global scope).
// Intentional: public API for library consumers
#[allow(dead_code)] // Library API (print_hook_status now reports across all scopes)
pub struct HookStatus {
    /// Whether all expected hook types are installed.
    #[allow(dead_code)] // Library API
    pub installed: bool,
    /// List of hook types that have a CTM hook installed.
    pub hook_types: Vec<String>,
    /// Any errors encountered while checking.
    #[allow(dead_code)] // Library API
    pub errors: Vec<String>,
}

/// Check hook installation status without printing anything (global scope).
#[allow(dead_code)] // Library API / back-compat
pub fn check_hook_status() -> HookStatus {
    let settings_path = global_settings_path();
    let settings = read_settings(&settings_path);
    let mut hook_types = Vec::new();
    let errors = Vec::new();

    for &hook_type in HOOK_TYPES {
        let hooks = settings
            .get("hooks")
            .and_then(|h| h.get(hook_type))
            .and_then(|v| v.as_array());

        let has_ctm = hooks
            .map(|arr| arr.iter().any(item_is_ctm))
            .unwrap_or(false);

        if has_ctm {
            hook_types.push(hook_type.to_string());
        }
    }

    let installed = hook_types.len() == HOOK_TYPES.len();
    HookStatus {
        installed,
        hook_types,
        errors,
    }
}

pub fn print_hook_status() -> anyhow::Result<()> {
    let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let diag = diagnose_hooks(&resolve_scopes(&base));

    println!("\nClaude Code Telegram Hook Status\n");

    // Per-scope presence (a hook in MULTIPLE scopes is the double-fire risk).
    for scope in [HookScope::Global, HookScope::Project, HookScope::Local] {
        let path = scope_settings_path(scope, &base);
        let settings = read_settings(&path);
        let present: Vec<&str> = HOOK_TYPES
            .iter()
            .copied()
            .filter(|&ht| ctm_command_count(&settings, ht) > 0)
            .collect();
        let exists = path.exists();
        println!("{} ({})", scope.label(), path.display());
        if !exists {
            println!("  (no settings file)");
        } else if present.is_empty() {
            println!("  no ctm hooks");
        } else {
            for &ht in HOOK_TYPES {
                let n = ctm_command_count(&settings, ht);
                if n > 0 {
                    let dup = if n > 1 {
                        format!("  \u{26A0}\u{FE0F} {n} entries (duplicate)")
                    } else {
                        String::new()
                    };
                    println!("  [OK] {ht}{dup}");
                }
            }
        }
        println!();
    }

    // Cross-scope / in-file duplicate summary.
    let cross = diag.cross_scope_dups();
    let infile = diag.in_file_dups();
    if !cross.is_empty() || !infile.is_empty() {
        println!(
            "\u{26A0}\u{FE0F}  Duplicate hooks detected (may execute more than once per event):"
        );
        if !cross.is_empty() {
            println!("   - same hook in multiple scopes: {}", cross.join(", "));
        }
        if !infile.is_empty() {
            println!(
                "   - duplicate entries within a file: {}",
                infile.join(", ")
            );
        }
        println!("   Run `ctm doctor --fix` to consolidate to a single scope.");
    } else {
        // No duplicates — report completeness across ALL scopes (a clean
        // project/local-only install must NOT be told "no global hooks").
        let present_types = diag.per_type.len();
        // If the present hooks are NOT in global, point completion at the same
        // (project) scope so we don't nudge a global install that would then
        // double-fire alongside the project ones.
        let global_present = diag
            .per_type
            .iter()
            .any(|(_, ps)| ps.iter().any(|p| matches!(p.scope, HookScope::Global)));
        let complete_cmd = if global_present {
            "ctm install-hooks"
        } else {
            "ctm install-hooks --project"
        };
        if present_types == HOOK_TYPES.len() {
            println!(
                "All {} hook types present (no duplicates).",
                HOOK_TYPES.len()
            );
        } else if present_types > 0 {
            println!(
                "{present_types}/{} hook types present (no duplicates). Run `{complete_cmd}` to complete.",
                HOOK_TYPES.len()
            );
        } else {
            println!("No ctm hooks installed in any scope. Run `ctm install-hooks` to install.");
        }
    }
    println!();

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_ctm_command() {
        // Positive cases
        assert!(is_ctm_command("/usr/local/bin/ctm hook"));
        assert!(is_ctm_command("ctm hook"));
        assert!(is_ctm_command("ctm"));
        assert!(is_ctm_command("/path/to/ctm"));
        assert!(is_ctm_command("/path/to/telegram-hook.sh"));
        assert!(is_ctm_command("node dist/hooks/handler.js"));
        // Negative cases
        assert!(!is_ctm_command("some-other-tool"));
        assert!(!is_ctm_command(""));
        // M9: must not match tools where "ctm" is embedded inside another word
        // (e.g. a hypothetical "xctm-linter" tool where ctm has a non-separator prefix)
        assert!(!is_ctm_command("xctm-linter --check"));
        assert!(!is_ctm_command("factm-check"));
    }

    #[test]
    fn test_item_is_ctm_new_format() {
        let item = serde_json::json!({
            "matcher": "",
            "hooks": [
                { "type": "command", "command": "ctm hook" }
            ]
        });
        assert!(item_is_ctm(&item));
    }

    #[test]
    fn test_item_is_ctm_old_format() {
        let item = serde_json::json!({
            "type": "command",
            "command": "/path/telegram-hook.sh"
        });
        assert!(item_is_ctm(&item));
    }

    #[test]
    fn test_item_is_not_ctm() {
        let item = serde_json::json!({
            "matcher": "",
            "hooks": [
                { "type": "command", "command": "other-tool check" }
            ]
        });
        assert!(!item_is_ctm(&item));
    }

    #[test]
    fn test_create_hook_entry() {
        let entry = create_hook_entry("ctm hook");
        assert_eq!(entry["matcher"], "");
        assert_eq!(entry["hooks"][0]["type"], "command");
        assert_eq!(entry["hooks"][0]["command"], "ctm hook");
    }

    #[test]
    fn test_install_and_uninstall_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let settings_path = dir.path().join("settings.json");

        // Install
        let (changes, _w) = install_hooks_to_path(
            &settings_path,
            dir.path(),
            HookScope::Global,
            dir.path(),
            false,
        )
        .unwrap();
        assert_eq!(changes.len(), HOOK_TYPES.len());
        for c in &changes {
            assert_eq!(c.status, HookChangeStatus::Added);
        }

        // Verify file contents
        let settings = read_settings(&settings_path);
        for &ht in HOOK_TYPES {
            let arr = settings["hooks"][ht].as_array().unwrap();
            assert!(!arr.is_empty());
            assert!(item_is_ctm(&arr[0]));
        }

        // Install again — idempotent
        let (changes2, _w) = install_hooks_to_path(
            &settings_path,
            dir.path(),
            HookScope::Global,
            dir.path(),
            false,
        )
        .unwrap();
        for c in &changes2 {
            assert_eq!(c.status, HookChangeStatus::Unchanged);
        }
    }

    /// ADR-014 A1: SessionEnd must be registered, and via the standard
    /// create_hook_entry() with NO custom timeout (unlike PreToolUse which gets
    /// 310). Hypothesis: after install, settings.hooks.SessionEnd exists, is a CTM
    /// hook, and its inner hook object has no `timeout` key.
    #[test]
    fn session_end_hook_registered_without_timeout() {
        assert!(
            HOOK_TYPES.contains(&"SessionEnd"),
            "SessionEnd must be in HOOK_TYPES"
        );

        let dir = tempfile::tempdir().unwrap();
        let settings_path = dir.path().join("settings.json");
        install_hooks_to_path(
            &settings_path,
            dir.path(),
            HookScope::Global,
            dir.path(),
            false,
        )
        .unwrap();
        let settings = read_settings(&settings_path);

        let arr = settings["hooks"]["SessionEnd"]
            .as_array()
            .expect("SessionEnd registered");
        assert!(item_is_ctm(&arr[0]), "SessionEnd is a CTM hook");
        let inner = &arr[0]["hooks"][0];
        assert!(
            inner.get("timeout").is_none(),
            "SessionEnd must have no custom timeout, got {inner:?}"
        );

        // PreToolUse, by contrast, must carry the 310s approval timeout.
        let pre = &settings["hooks"]["PreToolUse"].as_array().unwrap()[0]["hooks"][0];
        assert_eq!(pre.get("timeout").and_then(|t| t.as_u64()), Some(310));
    }

    #[test]
    fn test_install_preserves_non_ctm_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let settings_path = dir.path().join("settings.json");

        // Write existing settings with a non-CTM hook
        let existing = serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "",
                        "hooks": [{ "type": "command", "command": "my-custom-tool check" }]
                    }
                ]
            }
        });
        write_settings(&settings_path, &existing).unwrap();

        let (changes, _w) = install_hooks_to_path(
            &settings_path,
            dir.path(),
            HookScope::Global,
            dir.path(),
            false,
        )
        .unwrap();

        let settings = read_settings(&settings_path);
        let pre_tool = settings["hooks"]["PreToolUse"].as_array().unwrap();
        // Should have CTM hook first, then the custom hook
        assert_eq!(pre_tool.len(), 2);
        assert!(item_is_ctm(&pre_tool[0]));
        assert!(!item_is_ctm(&pre_tool[1]));

        // PreToolUse should be "added" since no CTM hook existed before
        let pre_change = changes
            .iter()
            .find(|c| c.hook_type == "PreToolUse")
            .unwrap();
        assert_eq!(pre_change.status, HookChangeStatus::Added);
    }

    #[test]
    fn test_determine_change_status_no_existing() {
        let expected = create_hook_entry("ctm hook");
        assert_eq!(
            determine_change_status(None, &expected),
            HookChangeStatus::Added
        );
    }

    #[test]
    fn test_determine_change_status_exact_match() {
        let cmd = ctm_hook_command();
        // Test with plain entry
        let expected = create_hook_entry(&cmd);
        let existing = Value::Array(vec![expected.clone()]);
        assert_eq!(
            determine_change_status(Some(&existing), &expected),
            HookChangeStatus::Unchanged
        );
        // Test with timeout entry (PreToolUse)
        let expected_timeout = create_hook_entry_with_timeout(&cmd, 310);
        let existing_timeout = Value::Array(vec![expected_timeout.clone()]);
        assert_eq!(
            determine_change_status(Some(&existing_timeout), &expected_timeout),
            HookChangeStatus::Unchanged
        );
    }

    #[test]
    fn test_determine_change_status_different_ctm() {
        let expected = create_hook_entry("ctm hook");
        let old = serde_json::json!([{
            "matcher": "",
            "hooks": [{ "type": "command", "command": "/old/path/ctm hook" }]
        }]);
        assert_eq!(
            determine_change_status(Some(&old), &expected),
            HookChangeStatus::Updated
        );
    }

    // ---- ROUTING/dedup hardening (Codex review) ----

    #[test]
    fn is_ctm_command_is_token_boundary_aware() {
        // Real ctm forms.
        assert!(is_ctm_command("ctm hook"));
        assert!(is_ctm_command("\"/usr/local/bin/ctm\" hook"));
        assert!(is_ctm_command("/path/ctm hook"));
        assert!(is_ctm_command("npx ctm hook"));
        assert!(is_ctm_command("/opt/claude-telegram-mirror hook"));
        assert!(is_ctm_command("sh -c \"ctm hook\""));
        // NOT ctm — "ctm" embedded in a longer identifier.
        assert!(!is_ctm_command("xctm hook"));
        assert!(!is_ctm_command("my-ctm-wrapper hook"));
        assert!(!is_ctm_command("xctm-linter check"));
        assert!(!is_ctm_command("some-other-tool run"));
    }

    #[test]
    fn strip_ctm_preserves_non_ctm_inner_hooks() {
        // An item mixing a ctm inner hook and a user inner hook must keep the user one.
        let mixed = serde_json::json!({
            "matcher": "Bash",
            "hooks": [
                { "type": "command", "command": "ctm hook" },
                { "type": "command", "command": "my-linter run" }
            ]
        });
        let stripped = strip_ctm_from_item(&mixed).expect("non-ctm inner survives");
        let inner = stripped["hooks"].as_array().unwrap();
        assert_eq!(inner.len(), 1);
        assert_eq!(inner[0]["command"], "my-linter run");
        assert_eq!(stripped["matcher"], "Bash");

        // A wholly-ctm item is dropped.
        let all_ctm = serde_json::json!({
            "matcher": "",
            "hooks": [{ "type": "command", "command": "ctm hook" }]
        });
        assert!(strip_ctm_from_item(&all_ctm).is_none());
    }

    #[test]
    fn install_collapses_preexisting_in_file_duplicates() {
        // Two identical ctm entries already present → install consolidates to one
        // (the ② short-circuit bug: it used to report Unchanged and leave both).
        let dir = tempfile::tempdir().unwrap();
        let settings_path = dir.path().join("settings.json");
        let cmd = ctm_hook_command();
        let dup = create_hook_entry(&cmd);
        let seeded = serde_json::json!({
            "hooks": { "Stop": [dup.clone(), dup.clone()] }
        });
        write_settings(&settings_path, &seeded).unwrap();

        assert_eq!(ctm_command_count(&read_settings(&settings_path), "Stop"), 2);
        let (changes, _w) = install_hooks_to_path(
            &settings_path,
            dir.path(),
            HookScope::Global,
            dir.path(),
            false,
        )
        .unwrap();
        let stop = changes.iter().find(|c| c.hook_type == "Stop").unwrap();
        assert_eq!(stop.status, HookChangeStatus::Updated);
        assert_eq!(
            ctm_command_count(&read_settings(&settings_path), "Stop"),
            1,
            "duplicates collapse to one"
        );
    }

    /// Build a (scope, path) list pointing every scope at an isolated temp file.
    fn scoped(paths: &[(HookScope, &std::path::Path)]) -> Vec<(HookScope, PathBuf)> {
        paths.iter().map(|(s, p)| (*s, p.to_path_buf())).collect()
    }

    #[test]
    fn diagnose_and_consolidate_resolve_cross_scope_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global.json");
        let project = dir.path().join("project.json");
        let local = dir.path().join("local.json");
        let entry = create_hook_entry("ctm hook");
        // Stop present in BOTH global and project (cross-scope dup); plus a 2nd
        // duplicate inside global (in-file dup).
        write_settings(
            &global,
            &serde_json::json!({ "hooks": { "Stop": [entry.clone(), entry.clone()] } }),
        )
        .unwrap();
        write_settings(
            &project,
            &serde_json::json!({ "hooks": { "Stop": [entry.clone()] } }),
        )
        .unwrap();
        let scopes = scoped(&[
            (HookScope::Global, &global),
            (HookScope::Project, &project),
            (HookScope::Local, &local),
        ]);

        let diag = diagnose_hooks(&scopes);
        assert!(diag.cross_scope_dups().contains(&"Stop"));
        assert!(diag.in_file_dups().contains(&"Stop"));
        assert!(!diag.is_clean());

        let log = consolidate_hooks(&scopes).unwrap();
        assert!(!log.is_empty());
        // Global (broadest) keeps exactly one; project is cleaned out.
        assert_eq!(ctm_command_count(&read_settings(&global), "Stop"), 1);
        assert_eq!(ctm_command_count(&read_settings(&project), "Stop"), 0);
        // Re-diagnose: clean.
        assert!(diagnose_hooks(&scopes).is_clean());
    }

    #[test]
    fn other_scopes_with_ctm_detects_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("g.json");
        let project = dir.path().join("p.json");
        let local = dir.path().join("l.json");
        write_settings(
            &global,
            &serde_json::json!({ "hooks": { "Stop": [create_hook_entry("/x/ctm hook")] } }),
        )
        .unwrap();
        let scopes = scoped(&[
            (HookScope::Global, &global),
            (HookScope::Project, &project),
            (HookScope::Local, &local),
        ]);
        // A project install of Stop sees the (differing-command) global ctm hook.
        let conflicts = other_scopes_with_ctm(HookScope::Project, &scopes, "Stop");
        assert_eq!(conflicts, vec![HookScope::Global]);
        // No conflict for a hook type nobody has.
        assert!(other_scopes_with_ctm(HookScope::Project, &scopes, "PreCompact").is_empty());
    }

    #[test]
    fn install_survives_malformed_hooks_value() {
        // A valid-JSON-but-wrong-shape `hooks` (array, then string) must not panic.
        for bad in [serde_json::json!([]), serde_json::json!("nope")] {
            let dir = tempfile::tempdir().unwrap();
            let settings_path = dir.path().join("settings.json");
            write_settings(&settings_path, &serde_json::json!({ "hooks": bad })).unwrap();
            let (changes, _w) = install_hooks_to_path(
                &settings_path,
                dir.path(),
                HookScope::Global,
                dir.path(),
                false,
            )
            .unwrap();
            assert!(changes.iter().all(|c| c.status == HookChangeStatus::Added));
            // hooks is now a proper object with the canonical entry.
            assert_eq!(ctm_command_count(&read_settings(&settings_path), "Stop"), 1);
        }
    }

    #[test]
    fn ctm_dash_prefix_only_matches_test_binary_hash() {
        // Rust test binary: ctm-<hex> → ctm.
        assert!(is_ctm_command("/t/deps/ctm-1a2b3c4d hook"));
        assert!(is_ctm_command("ctm-deadbeef hook"));
        // An unrelated `ctm-`-prefixed tool is NOT ctm.
        assert!(!is_ctm_command("ctm-wrapper hook"));
        assert!(!is_ctm_command("/usr/bin/ctm-helper run"));
    }

    #[test]
    fn remove_ctm_from_file_preserves_user_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        write_settings(
            &path,
            &serde_json::json!({ "hooks": {
                "Stop": [create_hook_entry("ctm hook")],
                "PreToolUse": [{ "matcher": "", "hooks": [
                    { "type": "command", "command": "ctm hook" },
                    { "type": "command", "command": "user-tool run" }
                ]}]
            }}),
        )
        .unwrap();
        let removed = remove_ctm_from_file(&path).unwrap();
        assert!(removed.contains(&"Stop".to_string()));
        assert!(removed.contains(&"PreToolUse".to_string()));
        let after = read_settings(&path);
        assert_eq!(ctm_command_count(&after, "Stop"), 0);
        // Stop became empty → key removed; PreToolUse keeps the user hook.
        assert!(after["hooks"].get("Stop").is_none());
        let pre = after["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 1);
        assert_eq!(pre[0]["hooks"][0]["command"], "user-tool run");
    }
}
