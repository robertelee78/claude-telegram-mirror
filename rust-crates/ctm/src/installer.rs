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
fn is_ctm_command(cmd: &str) -> bool {
    if cmd.contains("telegram-hook") || cmd.contains("hooks/handler") {
        return true;
    }
    // Extract the binary name: strip optional surrounding quotes from the first
    // token (handles both unquoted `/path/to/ctm hook` and quoted
    // `"/path/to/ctm" hook`), then take the basename after the last '/'.
    let first_token = cmd.split_whitespace().next().unwrap_or("");
    let unquoted = first_token.trim_matches('"');
    let bin_name = unquoted.rsplit('/').next().unwrap_or(unquoted);

    // Accept if the binary name is "ctm", starts with "ctm-" (Rust test binary naming),
    // or matches the full binary name "claude-telegram-mirror" / "claude_telegram_mirror".
    if bin_name == "ctm"
        || bin_name.starts_with("ctm-")
        || bin_name == "claude-telegram-mirror"
        || bin_name.starts_with("claude_telegram_mirror")
        || bin_name.starts_with("claude-telegram-mirror")
    {
        return true;
    }

    // Also accept the literal "ctm hook" substring for any path variation.
    if cmd.contains("ctm hook") {
        return true;
    }

    false
}

/// Check if a hook item (in the new format) contains a CTM command.
fn item_is_ctm(item: &Value) -> bool {
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

#[derive(Debug, PartialEq)]
pub enum HookChangeStatus {
    Added,
    Updated,
    Unchanged,
}

impl std::fmt::Display for HookChangeStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Added => write!(f, "added"),
            Self::Updated => write!(f, "updated"),
            Self::Unchanged => write!(f, "unchanged"),
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
pub fn install_hooks_for_project(project_path: &Path) -> anyhow::Result<()> {
    let project_dir = project_path.join(".claude");
    if !project_dir.exists() {
        anyhow::bail!(
            "No .claude directory found in {}. Run from a Claude project directory.",
            project_path.display()
        );
    }
    let settings_path = project_dir.join("settings.json");
    let changes = install_hooks_to_path(&settings_path, &project_dir)?;

    println!("\nClaude Code Telegram Hook Installation (project)\n");
    println!("Settings: {}\n", settings_path.display());

    print_install_results(&changes);

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
pub fn install_hooks_with_path(project: bool, custom_path: Option<&Path>) -> anyhow::Result<()> {
    let (settings_path, config_dir) = if project {
        let base = match custom_path {
            Some(p) => p.to_path_buf(),
            None => std::env::current_dir()?,
        };
        let project_dir = base.join(".claude");
        if !project_dir.exists() {
            anyhow::bail!(
                "No .claude directory found in {}. Run from a Claude project directory.",
                base.display()
            );
        }
        (project_dir.join("settings.json"), project_dir)
    } else {
        (global_settings_path(), global_claude_dir())
    };

    let changes = install_hooks_to_path(&settings_path, &config_dir)?;

    // Print results
    let location = if project { "project" } else { "global" };
    println!("\nClaude Code Telegram Hook Installation ({location})\n");
    println!("Settings: {}\n", settings_path.display());

    print_install_results(&changes);

    Ok(())
}

/// Print install results with emoji indicators and a restart reminder.
fn print_install_results(changes: &[HookChangeReport]) {
    let mut any_changed = false;
    for report in changes {
        let icon = match report.status {
            HookChangeStatus::Added => "\u{2705}",     // checkmark
            HookChangeStatus::Updated => "\u{1F504}",  // arrows
            HookChangeStatus::Unchanged => "\u{2713}", // simple check
        };
        let label = match report.status {
            HookChangeStatus::Added => "Added",
            HookChangeStatus::Updated => "Updated",
            HookChangeStatus::Unchanged => "Already correct",
        };
        println!("  {icon} {}: {label}", report.hook_type);
        if report.status != HookChangeStatus::Unchanged {
            any_changed = true;
        }
    }
    println!();

    if any_changed {
        println!("Hooks installed successfully.");
        println!("\u{1F4A1} Restart Claude Code to activate changes.");
    } else {
        println!("All hooks already up to date.");
    }
    println!();
}

/// Core logic: install hooks to a specific settings path.
fn install_hooks_to_path(
    settings_path: &Path,
    config_dir: &Path,
) -> anyhow::Result<Vec<HookChangeReport>> {
    let command = ctm_hook_command();
    let mut settings = read_settings(settings_path);
    let mut changes = Vec::new();
    let mut config_changed = false;

    // Ensure hooks object exists
    if settings.get("hooks").is_none() {
        settings["hooks"] = serde_json::json!({});
    }

    for &hook_type in HOOK_TYPES {
        // PreToolUse needs timeout: 310 for approval workflow (5 min + 10s buffer)
        let expected = if hook_type == "PreToolUse" {
            create_hook_entry_with_timeout(&command, 310)
        } else {
            create_hook_entry(&command)
        };
        let existing = settings["hooks"].get(hook_type);

        let status = determine_change_status(existing, &expected);
        let details = match &status {
            HookChangeStatus::Added => "added ctm hook".to_string(),
            HookChangeStatus::Updated => "updated from old path".to_string(),
            HookChangeStatus::Unchanged => "no changes".to_string(),
        };
        changes.push(HookChangeReport {
            hook_type: hook_type.to_string(),
            status,
            details,
        });

        if changes.last().unwrap().status != HookChangeStatus::Unchanged {
            config_changed = true;

            // Filter out existing CTM hooks, keep user's other hooks
            let mut filtered: Vec<Value> = existing
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter(|item| !item_is_ctm(item))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();

            // Add CTM hook at the beginning
            filtered.insert(0, expected);
            settings["hooks"][hook_type] = Value::Array(filtered);
        }
    }

    if config_changed {
        // Ensure config dir exists
        if !config_dir.exists() {
            fs::create_dir_all(config_dir)?;
        }
        write_settings(settings_path, &settings)?;
    }

    Ok(changes)
}

fn determine_change_status(existing: Option<&Value>, expected: &Value) -> HookChangeStatus {
    let arr = match existing.and_then(|v| v.as_array()) {
        Some(a) => a,
        None => return HookChangeStatus::Added,
    };

    let expected_cmd = expected["hooks"][0]["command"].as_str().unwrap_or("");

    // Check if any existing hook already matches
    let has_ctm = arr.iter().any(item_is_ctm);

    if !has_ctm {
        return HookChangeStatus::Added;
    }

    // Check if the command matches exactly
    let exact_match = arr.iter().any(|item| {
        if let Some(hooks) = item.get("hooks").and_then(|v| v.as_array()) {
            hooks.iter().any(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    .map(|c| c == expected_cmd)
                    .unwrap_or(false)
            })
        } else {
            item.get("command")
                .and_then(|c| c.as_str())
                .map(|c| c == expected_cmd)
                .unwrap_or(false)
        }
    });

    if exact_match {
        HookChangeStatus::Unchanged
    } else {
        HookChangeStatus::Updated
    }
}

// ---------------------------------------------------------------------------
// Uninstall
// ---------------------------------------------------------------------------

pub fn uninstall_hooks() -> anyhow::Result<()> {
    let settings_path = global_settings_path();
    let mut settings = read_settings(&settings_path);
    let mut removed = Vec::new();

    if let Some(hooks_obj) = settings.get_mut("hooks") {
        if let Some(obj) = hooks_obj.as_object_mut() {
            let keys: Vec<String> = obj.keys().cloned().collect();
            for key in keys {
                if let Some(arr) = obj.get(&key).and_then(|v| v.as_array()) {
                    let filtered: Vec<Value> = arr
                        .iter()
                        .filter(|item| !item_is_ctm(item))
                        .cloned()
                        .collect();

                    if filtered.len() < arr.len() {
                        removed.push(key.clone());
                    }

                    if filtered.is_empty() {
                        obj.remove(&key);
                    } else {
                        obj.insert(key, Value::Array(filtered));
                    }
                }
            }

            // Remove empty hooks object
            if obj.is_empty() {
                settings.as_object_mut().map(|root| root.remove("hooks"));
            }
        }
    }

    write_settings(&settings_path, &settings)?;

    println!("\nClaude Code Telegram Hook Uninstall\n");
    if removed.is_empty() {
        println!("No CTM hooks found to remove.");
    } else {
        println!("Removed hooks from:");
        for hook_type in &removed {
            println!("  - {hook_type}");
        }
    }
    println!();

    Ok(())
}

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

/// Programmatic hook status report.
// Intentional: public API for library consumers
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

/// Check hook installation status without printing anything.
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
    let settings_path = global_settings_path();
    let settings = read_settings(&settings_path);
    let status = check_hook_status();

    println!("\nClaude Code Telegram Hook Status\n");
    println!("Settings: {}\n", settings_path.display());

    for &hook_type in HOOK_TYPES {
        let hooks = settings
            .get("hooks")
            .and_then(|h| h.get(hook_type))
            .and_then(|v| v.as_array());

        let has_ctm = hooks
            .map(|arr| arr.iter().any(item_is_ctm))
            .unwrap_or(false);

        let icon = if has_ctm { "OK" } else { "--" };
        let cmd_display = if has_ctm {
            let cmd = hooks
                .and_then(|arr| {
                    arr.iter().find(|item| item_is_ctm(item)).and_then(|item| {
                        item.get("hooks")
                            .and_then(|h| h.as_array())
                            .and_then(|h| h.first())
                            .and_then(|h| h.get("command"))
                            .and_then(|c| c.as_str())
                            .or_else(|| item.get("command").and_then(|c| c.as_str()))
                    })
                })
                .unwrap_or("?");
            format!(" -> {cmd}")
        } else {
            String::new()
        };

        println!("  [{icon}] {hook_type}{cmd_display}");
    }

    println!();
    let installed_count = status.hook_types.len();
    if installed_count == HOOK_TYPES.len() {
        println!("All hooks installed.");
    } else if installed_count > 0 {
        println!(
            "{installed_count}/{} hooks installed. Run `ctm install-hooks` to fix.",
            HOOK_TYPES.len()
        );
    } else {
        println!("No hooks installed. Run `ctm install-hooks` to install.");
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
        let changes = install_hooks_to_path(&settings_path, dir.path()).unwrap();
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
        let changes2 = install_hooks_to_path(&settings_path, dir.path()).unwrap();
        for c in &changes2 {
            assert_eq!(c.status, HookChangeStatus::Unchanged);
        }
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

        let changes = install_hooks_to_path(&settings_path, dir.path()).unwrap();

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
}
