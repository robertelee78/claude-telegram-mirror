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
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"))
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
        Ok(exe) => format!("{} hook", exe.display()),
        Err(_) => "ctm hook".into(),
    }
}

// ---------------------------------------------------------------------------
// Hook identification
// ---------------------------------------------------------------------------

/// Check if a hook command string belongs to CTM.
fn is_ctm_command(cmd: &str) -> bool {
    cmd.contains("ctm") || cmd.contains("telegram-hook") || cmd.contains("hooks/handler")
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

pub struct HookChangeReport {
    pub hook_type: String,
    pub status: HookChangeStatus,
}

// ---------------------------------------------------------------------------
// Install
// ---------------------------------------------------------------------------

/// Install CTM hooks into the given settings file.
///
/// Returns the list of changes made. Idempotent: running twice produces
/// no changes on the second invocation.
pub fn install_hooks(project: bool) -> anyhow::Result<()> {
    let (settings_path, config_dir) = if project {
        let cwd = std::env::current_dir()?;
        let project_dir = cwd.join(".claude");
        if !project_dir.exists() {
            anyhow::bail!(
                "No .claude directory found in {}. Run from a Claude project directory.",
                cwd.display()
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

    let mut any_changed = false;
    for report in &changes {
        let icon = match report.status {
            HookChangeStatus::Added => "+",
            HookChangeStatus::Updated => "~",
            HookChangeStatus::Unchanged => " ",
        };
        println!("  [{icon}] {}: {}", report.hook_type, report.status);
        if report.status != HookChangeStatus::Unchanged {
            any_changed = true;
        }
    }
    println!();

    if any_changed {
        println!("Hooks installed successfully.");
    } else {
        println!("All hooks already up to date.");
    }
    println!();

    Ok(())
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
        changes.push(HookChangeReport {
            hook_type: hook_type.to_string(),
            status: if status == HookChangeStatus::Unchanged {
                HookChangeStatus::Unchanged
            } else {
                status
            },
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

pub fn print_hook_status() -> anyhow::Result<()> {
    let settings_path = global_settings_path();
    let settings = read_settings(&settings_path);

    println!("\nClaude Code Telegram Hook Status\n");
    println!("Settings: {}\n", settings_path.display());

    let mut installed_count = 0;

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
            // Extract the CTM command for display
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
        if has_ctm {
            installed_count += 1;
        }
    }

    println!();
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
        assert!(is_ctm_command("/usr/local/bin/ctm hook"));
        assert!(is_ctm_command("/path/to/telegram-hook.sh"));
        assert!(is_ctm_command("node dist/hooks/handler.js"));
        assert!(!is_ctm_command("some-other-tool"));
        assert!(!is_ctm_command(""));
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
        let expected = create_hook_entry(&cmd);
        let existing = Value::Array(vec![expected.clone()]);
        assert_eq!(
            determine_change_status(Some(&existing), &expected),
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
