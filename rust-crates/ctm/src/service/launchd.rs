//! launchd plist generation and lifecycle.

use super::*;

fn get_macos_path() -> String {
    let home = home_dir();
    let mut paths: Vec<String> = vec![
        "/usr/local/bin".into(),
        "/usr/bin".into(),
        "/bin".into(),
        "/usr/sbin".into(),
        "/sbin".into(),
        "/opt/homebrew/bin".into(),
        format!("{}/.local/bin", home.display()),
    ];

    // Merge with current PATH, excluding NVM paths (legacy Node.js artifact)
    if let Ok(current) = std::env::var("PATH") {
        for dir in current.split(':') {
            if !dir.is_empty() && !dir.contains(".nvm") && !paths.contains(&dir.to_string()) {
                paths.push(dir.to_string());
            }
        }
    }

    paths.join(":")
}

pub(super) fn generate_launchd_plist() -> String {
    let binary = ctm_binary_path();
    let home = home_dir();
    let config_dir = home.join(".config").join(SERVICE_NAME);
    let log_file = config_dir.join("daemon.log");
    let err_file = config_dir.join("daemon.err.log");

    let env_vars = env::parse_env_file(&env_file_path());

    let mut env_lines = Vec::new();
    // Essential env vars
    env_lines.push(format!(
        "        <key>HOME</key>\n        <string>{}</string>",
        escape_xml(&home.display().to_string())
    ));
    env_lines.push(format!(
        "        <key>PATH</key>\n        <string>{}</string>",
        escape_xml(&get_macos_path())
    ));
    // User-defined env vars from ~/.telegram-env
    for (key, value) in &env_vars {
        if key == "HOME" || key == "PATH" {
            continue;
        }
        env_lines.push(format!(
            "        <key>{}</key>\n        <string>{}</string>",
            escape_xml(key),
            escape_xml(value),
        ));
    }

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.claude.{service_name}</string>

    <key>ProgramArguments</key>
    <array>
        <string>{binary}</string>
        <string>start</string>
    </array>

    <key>WorkingDirectory</key>
    <string>{home_dir}</string>

    <key>EnvironmentVariables</key>
    <dict>
{env_block}
    </dict>

    <key>RunAtLoad</key>
    <true/>

    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
        <key>Crashed</key>
        <true/>
    </dict>

    <key>ThrottleInterval</key>
    <integer>10</integer>

    <key>StandardOutPath</key>
    <string>{log_file}</string>

    <key>StandardErrorPath</key>
    <string>{err_file}</string>
</dict>
</plist>
"#,
        service_name = SERVICE_NAME,
        binary = escape_xml(&binary.display().to_string()),
        home_dir = escape_xml(&home.display().to_string()),
        env_block = env_lines.join("\n"),
        log_file = escape_xml(&log_file.display().to_string()),
        err_file = escape_xml(&err_file.display().to_string()),
    )
}

pub(super) fn install_launchd_service() -> ServiceResult {
    let plist_dir = launchd_dir();
    if !plist_dir.exists() {
        if let Err(e) = fs::create_dir_all(&plist_dir) {
            return ServiceResult {
                success: false,
                message: format!("Failed to create LaunchAgents dir: {e}"),
            };
        }
    }

    // Ensure config dir for logs
    let config_dir = home_dir().join(".config").join(SERVICE_NAME);
    if let Err(e) = config::ensure_config_dir(&config_dir) {
        return ServiceResult {
            success: false,
            message: format!("Failed to ensure config dir: {e}"),
        };
    }

    let plist_path = launchd_plist();
    let content = generate_launchd_plist();
    if let Err(e) = fs::write(&plist_path, content) {
        return ServiceResult {
            success: false,
            message: format!("Failed to write plist file: {e}"),
        };
    }

    ServiceResult {
        success: true,
        message: format!(
            "Service installed: {plist}\n\nCommands:\n  Load & Start:  launchctl load {plist}\n  Start:         launchctl start com.claude.{SERVICE_NAME}\n  Stop:          launchctl stop com.claude.{SERVICE_NAME}\n  Unload:        launchctl unload {plist}\n  Logs:          tail -f ~/.config/{SERVICE_NAME}/daemon.log",
            plist = plist_path.display(),
        ),
    }
}

pub(super) fn uninstall_launchd_service() -> ServiceResult {
    let plist = launchd_plist();

    // Unload
    let _ = Command::new("launchctl")
        .args(["unload", &plist.display().to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    if plist.exists() {
        let _ = fs::remove_file(&plist);
    }

    ServiceResult {
        success: true,
        message: "Service uninstalled successfully.".into(),
    }
}

pub(super) fn start_launchd_service() -> ServiceResult {
    let plist = launchd_plist();
    // Load if not loaded
    let _ = Command::new("launchctl")
        .args(["load", &plist.display().to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    let kicked = Command::new("launchctl")
        .args(["start", &format!("com.claude.{SERVICE_NAME}")])
        .status();

    if !matches!(kicked, Ok(s) if s.success()) {
        return ServiceResult {
            success: false,
            message: "Failed to start launchd service.".into(),
        };
    }

    wait_for_stable_start()
}

/// Restart the launchd service.
///
/// We do NOT stop-then-start: `launchctl stop` is asynchronous and returns
/// before the process has actually exited, so a following `launchctl start` is
/// coalesced/ignored while launchd is still tearing the old instance down. The
/// old instance then exits cleanly (status 0), and because the plist's
/// `KeepAlive { SuccessfulExit: false }` does not relaunch a clean exit, the
/// service is left DOWN — exactly the "restart failed" symptom.
///
/// `launchctl kickstart -k` instead kills any running instance and starts a
/// fresh one as a single atomic operation, with no race and no dependence on
/// KeepAlive semantics. We fall back to a synchronous stop→wait→start only if
/// kickstart is unavailable or fails.
pub(super) fn restart_launchd_service() -> ServiceResult {
    let plist = launchd_plist();
    // Ensure the job is bootstrapped (no-op if already loaded).
    let _ = Command::new("launchctl")
        .args(["load", &plist.display().to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    let uid = nix::unistd::getuid().as_raw();
    let target = format!("gui/{uid}/com.claude.{SERVICE_NAME}");
    let kicked = Command::new("launchctl")
        .args(["kickstart", "-k", &target])
        .status();

    if matches!(kicked, Ok(s) if s.success()) {
        return wait_for_stable_start();
    }

    // Fallback: synchronous stop (wait for the process to actually exit) then start.
    let _ = Command::new("launchctl")
        .args(["stop", &format!("com.claude.{SERVICE_NAME}")])
        .status();
    use std::{thread::sleep, time::Duration};
    for _ in 0..20 {
        if launchd_pid().is_none() {
            break;
        }
        sleep(Duration::from_millis(250));
    }
    start_launchd_service()
}

/// Wait for the service to actually come up and stay up after a start/restart
/// request, returning a truthful `ServiceResult`.
///
/// `launchctl start`/`kickstart` only confirm launchd *accepted* the request —
/// not that the daemon survived `exec`. On macOS a non-notarized / ad-hoc-signed
/// binary can be SIGKILLed by the kernel for a code-signing / launch-constraint
/// violation milliseconds after launch (EXC_CRASH / "Code Signature Invalid").
/// When that happens the plist's `KeepAlive { Crashed: true }` makes launchd
/// relaunch it — but only after `ThrottleInterval` (10s).
///
/// So we must wait *longer than the throttle window* before declaring failure,
/// or we false-alarm on exactly the transient kill we describe: the first launch
/// is killed, a short poll sees no PID, we cry failure — and then launchd quietly
/// brings it back ~10s later. We instead poll for a *stable* PID (the same live
/// PID observed across two polls): a healthy start settles in ~1s and returns
/// immediately; a first-launch kill is ridden out across the throttle window and
/// reported as success once the relaunched instance is up. Only a service that
/// never stabilises within the budget is a failure.
fn wait_for_stable_start() -> ServiceResult {
    use std::{thread::sleep, time::Duration};
    const POLL: Duration = Duration::from_millis(500);
    const BUDGET: Duration = Duration::from_secs(14); // > ThrottleInterval (10s) + margin
    let mut elapsed = Duration::ZERO;
    let mut last_pid: Option<i32> = None;
    let mut announced_wait = false;
    while elapsed < BUDGET {
        let pid = launchd_pid();
        // Same live PID seen twice in a row ⇒ it survived past launch.
        if pid.is_some() && pid == last_pid {
            return ServiceResult {
                success: true,
                message: "Service started.".into(),
            };
        }
        if !announced_wait && pid.is_none() && elapsed >= Duration::from_millis(1500) {
            // We only get here when the first launch did not stay up. Tell the
            // user we are intentionally waiting rather than hanging silently.
            println!(
                "Waiting for the service to come up (launchd may be retrying after a launch failure)..."
            );
            announced_wait = true;
        }
        last_pid = pid;
        sleep(POLL);
        elapsed += POLL;
    }

    ServiceResult {
        success: false,
        message: start_failure_hint(
            "Service did not stay running — it failed to come up within 14s.",
        ),
    }
}

/// Build an actionable error message for a service that launched but did not
/// stay up. The overwhelmingly common cause on macOS is the kernel killing a
/// non-notarized binary for a code-signing / launch-constraint violation.
fn start_failure_hint(headline: &str) -> String {
    let binary = ctm_binary_path();
    format!(
        "{headline}\n\
         \n\
         On macOS this is almost always a code-signing / Gatekeeper rejection of\n\
         the native binary (it is ad-hoc signed, not notarized). Check:\n\
         \n  • Crash reports:  ls ~/Library/Logs/DiagnosticReports/ctm-*.ips\
         \n  • Signature:      codesign -dvvv {bin}\
         \n  • Quarantine:     xattr -dr com.apple.quarantine {bin}\
         \n  • Then re-run:    ctm doctor\n\
         \n\
         launchd will keep retrying in the background, so the daemon may still\n\
         come up shortly — check `ctm status` again in a moment. If it never\n\
         stays up, reinstall the package or build from source\n\
         (cd rust-crates && cargo build --release).",
        bin = binary.display(),
    )
}

pub(super) fn stop_launchd_service() -> ServiceResult {
    match Command::new("launchctl")
        .args(["stop", &format!("com.claude.{SERVICE_NAME}")])
        .status()
    {
        Ok(s) if s.success() => ServiceResult {
            success: true,
            message: "Service stopped.".into(),
        },
        _ => ServiceResult {
            success: false,
            message: "Failed to stop launchd service.".into(),
        },
    }
}

/// Return the live PID of the launchd job, or `None` if it is loaded but not
/// actually running.
///
/// `launchctl list` prints one tab-separated line per loaded job in the form
/// `PID<TAB>Status<TAB>Label`. A job that is *loaded but dead* (e.g. it exited
/// cleanly and `KeepAlive` did not restart it, or the kernel killed it for a
/// code-signing violation) still appears in the list, but with `-` in the PID
/// column. The previous implementation only checked that the label *appeared*
/// anywhere in the output, so it reported "running" for any loaded job — making
/// `ctm status`/`doctor` lie and turning `ctm start` into a no-op ("already
/// running") that could never recover a dead service. We must parse the PID
/// column and only treat a numeric PID as running.
pub(super) fn launchd_pid() -> Option<i32> {
    let label = format!("com.claude.{SERVICE_NAME}");
    let output = Command::new("launchctl").arg("list").output().ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_launchd_pid(&stdout, &label)
}

/// Pure parser for `launchctl list` output. Extracted so the PID-column logic
/// (the fix for the false "running" report) is unit-testable without shelling
/// out. Returns the live PID for an exact `label` match, or `None` when the job
/// is absent or loaded-but-dead (PID column `-`).
fn parse_launchd_pid(stdout: &str, label: &str) -> Option<i32> {
    for line in stdout.lines() {
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() >= 3 && cols[2].trim() == label {
            // PID column is `-` when loaded-but-dead; a real PID is a positive int.
            return cols[0].trim().parse::<i32>().ok().filter(|&p| p > 0);
        }
    }
    None
}

pub(super) fn get_launchd_status() -> ServiceStatus {
    let plist = launchd_plist();
    let enabled = plist.exists();

    let running = launchd_pid().is_some();

    let info = if !enabled {
        "Service not installed".into()
    } else {
        format!("Plist file: {}", plist.display())
    };

    ServiceStatus {
        running,
        enabled,
        info,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LABEL: &str = "com.claude.claude-telegram-mirror";

    #[test]
    fn parse_launchd_pid_running_returns_pid() {
        // A live job: numeric PID in the first column.
        let out = "PID\tStatus\tLabel\n80739\t0\tcom.claude.claude-telegram-mirror\n";
        assert_eq!(parse_launchd_pid(out, LABEL), Some(80739));
    }

    #[test]
    fn parse_launchd_pid_loaded_but_dead_returns_none() {
        // The exact bug: job is loaded (appears in list) but not running (`-`).
        // The old `contains(label)` check reported this as running.
        let out = "PID\tStatus\tLabel\n-\t0\tcom.claude.claude-telegram-mirror\n";
        assert_eq!(parse_launchd_pid(out, LABEL), None);
    }

    #[test]
    fn parse_launchd_pid_absent_returns_none() {
        let out = "PID\tStatus\tLabel\n123\t0\tcom.apple.something\n";
        assert_eq!(parse_launchd_pid(out, LABEL), None);
    }

    #[test]
    fn parse_launchd_pid_requires_exact_column_match() {
        // A different label that merely *contains* ours as a substring must not
        // match — exact column comparison guards against the substring bug.
        let out = "456\t0\tcom.claude.claude-telegram-mirror-helper\n";
        assert_eq!(parse_launchd_pid(out, LABEL), None);
    }

    #[test]
    fn parse_launchd_pid_rejects_non_positive() {
        let out = "0\t0\tcom.claude.claude-telegram-mirror\n";
        assert_eq!(parse_launchd_pid(out, LABEL), None);
    }

    #[test]
    fn test_generate_launchd_plist_contains_key_fields() {
        let content = generate_launchd_plist();
        assert!(content.contains("<key>Label</key>"));
        assert!(content.contains("<key>KeepAlive</key>"));
        assert!(content.contains("<key>Crashed</key>"));
        assert!(content.contains("<key>ThrottleInterval</key>"));
        assert!(content.contains("<integer>10</integer>"));
        assert!(content.contains("<key>StandardOutPath</key>"));
        assert!(content.contains("<key>StandardErrorPath</key>"));
    }

    #[test]
    fn test_generate_launchd_plist_no_node_artifacts() {
        let content = generate_launchd_plist();
        // NODE_ENV was a TypeScript artifact — should not be in the Rust plist
        assert!(
            !content.contains("NODE_ENV"),
            "plist should not contain NODE_ENV"
        );
        // NVM paths are not needed for the Rust binary
        assert!(
            !content.contains(".nvm"),
            "plist should not contain NVM paths"
        );
    }
}
