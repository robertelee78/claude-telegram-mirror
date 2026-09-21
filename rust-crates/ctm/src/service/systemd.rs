//! systemd user-unit generation and lifecycle.
//!
//! ADR-019: every operation here is act → observe → report. `systemctl`'s exit status
//! is never the verdict; the unit's state as the manager reports it afterwards is.

use super::systemd_state::{
    observe, systemctl, wait_running_stable, wait_stopped, Observation, Waited,
};
use super::*;
use std::time::Duration;

/// Longer than `RestartSec=10s`, so a unit that fails and is retried once is
/// caught in `auto-restart` rather than mistaken for a slow start.
const START_BUDGET: Duration = Duration::from_secs(14);
const STOP_BUDGET: Duration = Duration::from_secs(10);

pub(super) fn generate_systemd_service(spec: &ServiceSpec) -> String {
    let exec = std::iter::once(spec.program.display().to_string())
        .chain(spec.args.iter().cloned())
        .collect::<Vec<_>>()
        .join(" ");
    let env_line = spec
        .env_file
        .as_ref()
        .map(|f| format!("EnvironmentFile={}\n", f.display()))
        .unwrap_or_default();
    // WorkingDirectory=%h: the user's home is always present and writable, which is
    // all a self-contained binary needs.
    format!(
        r#"[Unit]
Description={description}
Documentation=https://github.com/robertelee78/claude-telegram-mirror
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
WorkingDirectory=%h
ExecStart={exec}
{env_line}
# Restart policy
Restart=on-failure
RestartSec=10s
StartLimitInterval=300s
StartLimitBurst=5

# Logging
StandardOutput=journal
StandardError=journal
SyslogIdentifier={name}

# Security hardening
NoNewPrivileges=true
PrivateTmp=false

# Allow writes to config directory
ReadWritePaths={log_dir}

[Install]
WantedBy=default.target
"#,
        description = spec.description,
        name = spec.name,
        log_dir = spec.log_dir.display(),
    )
}

fn fail(message: String) -> ServiceResult {
    ServiceResult {
        success: false,
        message,
    }
}

fn ok(message: String) -> ServiceResult {
    ServiceResult {
        success: true,
        message,
    }
}

fn unreachable_report(what: &str, err: &str) -> ServiceResult {
    fail(format!(
        "{what}, but the systemd user manager could not be reached: {}{}",
        if err.is_empty() { "(no output)" } else { err },
        user_manager_hint(err)
    ))
}

fn logs_hint(spec: &ServiceSpec) -> String {
    format!(
        "\n  Logs:  journalctl --user -u {} -n 50 --no-pager",
        spec.systemd_unit()
    )
}

/// Advice for the failures that actually happen on a headless Linux box.
///
/// A user unit needs a running *user manager*, which a plain SSH session may not have:
/// without `loginctl enable-linger` the manager stops with the last session, and without
/// `XDG_RUNTIME_DIR`/`DBUS_SESSION_BUS_ADDRESS` `systemctl --user` cannot reach it at all.
pub(super) fn user_manager_hint(stderr: &str) -> String {
    let s = stderr.to_ascii_lowercase();
    if s.contains("failed to connect to bus") || s.contains("no medium found") {
        return format!(
            "\n\nThis user has no running systemd user manager. Enable it with:\n               sudo loginctl enable-linger {}\n\
             then log in again (or `export XDG_RUNTIME_DIR=/run/user/$(id -u)`) and re-run \
             `ctm service install`.",
            std::env::var("USER").unwrap_or_else(|_| "$USER".into())
        );
    }
    String::new()
}

/// Has the unit file been written? (Distinct from "the manager knows about it".)
pub(super) fn unit_present(spec: &ServiceSpec) -> bool {
    spec.systemd_unit_path().exists()
}

/// Goal: unit file on disk AND the manager has loaded and enabled it.
pub(super) fn install_with(spec: &ServiceSpec) -> ServiceResult {
    let sdir = systemd_user_dir();
    if let Err(e) = fs::create_dir_all(&sdir) {
        return fail(format!("Failed to create systemd dir: {e}"));
    }
    let unit_path = spec.systemd_unit_path();
    if let Err(e) = fs::write(&unit_path, generate_systemd_service(spec)) {
        return fail(format!("Failed to write service file: {e}"));
    }
    let unit = spec.systemd_unit();
    for (args, what) in [
        (vec!["daemon-reload"], "daemon-reload"),
        (vec!["enable", unit.as_str()], "enable"),
    ] {
        if let Err(err) = systemctl(&args) {
            return fail(format!(
                "Unit file written to {}, but `systemctl --user {what}` failed: {err}{}",
                unit_path.display(),
                user_manager_hint(&err)
            ));
        }
    }
    match observe(&unit) {
        Observation::Unit(u) if u.known() && u.unit_file == "enabled" => ok(format!(
            "Service installed: {}\n\nCommands:\n  Start:   systemctl --user start {name}\n  Stop:    systemctl --user stop {name}\n  Status:  systemctl --user status {name}\n  Logs:    journalctl --user -u {name} -f\n\nTo run without being logged in:\n  sudo loginctl enable-linger $USER",
            unit_path.display(),
            name = spec.name,
        )),
        Observation::Unit(u) => fail(format!(
            "Unit file written to {}, but the manager reports it as {} / {}",
            unit_path.display(),
            u.load,
            if u.unit_file.is_empty() { "unknown" } else { &u.unit_file }
        )),
        Observation::Unreachable(err) => unreachable_report("Unit file written", &err),
    }
}

/// Goal: not loaded, not running, no unit file, no enable symlink.
pub(super) fn uninstall_with(spec: &ServiceSpec) -> ServiceResult {
    let unit = spec.systemd_unit();
    let mut problems: Vec<String> = Vec::new();
    // Order matters: `disable` needs the unit file to find its [Install] section.
    for args in [vec!["stop", unit.as_str()], vec!["disable", unit.as_str()]] {
        if let Err(err) = systemctl(&args) {
            problems.push(format!("`systemctl --user {}`: {err}", args.join(" ")));
        }
    }
    let unit_path = spec.systemd_unit_path();
    if unit_path.exists() {
        if let Err(e) = fs::remove_file(&unit_path) {
            problems.push(format!("could not remove {}: {e}", unit_path.display()));
        }
    }
    if let Err(err) = systemctl(&["daemon-reload"]) {
        problems.push(format!("`systemctl --user daemon-reload`: {err}"));
    }
    let link = spec.systemd_wants_link();
    let link_left = link.symlink_metadata().is_ok();
    match observe(&unit) {
        Observation::Unit(u) if !u.known() && !u.running() && !unit_path.exists() && !link_left => {
            ok("Service uninstalled.".into())
        }
        Observation::Unit(u) => {
            let mut left = Vec::new();
            if u.known() {
                left.push(format!(
                    "the manager still knows the unit ({})",
                    u.summary()
                ));
            }
            if u.running() {
                left.push(format!(
                    "it is still running (pid {})",
                    u.main_pid.unwrap_or(0)
                ));
            }
            if unit_path.exists() {
                left.push(format!("{} still exists", unit_path.display()));
            }
            if link_left {
                left.push(format!("{} still exists", link.display()));
            }
            fail(format!(
                "Uninstall incomplete: {}.\n{}",
                left.join("; "),
                problems.join("\n")
            ))
        }
        Observation::Unreachable(err) => {
            let done = if unit_path.exists() {
                "Unit file could not be removed"
            } else {
                "Unit file removed"
            };
            unreachable_report(
                &format!("{done}, so it could not be stopped or disabled"),
                &err,
            )
        }
    }
}

/// Goal: running with a stable PID.
pub(super) fn start_with(spec: &ServiceSpec) -> ServiceResult {
    // Starting a service that was never installed is a thing ctm can do rather than
    // make the operator decode systemd's "Unit ... not found".
    if !unit_present(spec) {
        let installed = install_with(spec);
        if !installed.success {
            return fail(format!(
                "Service is not installed, and installing it failed.\n{}",
                installed.message
            ));
        }
        println!("Service was not installed; installed it first.");
    }
    let unit = spec.systemd_unit();
    let issued = systemctl(&["start", unit.as_str()]);
    report_running(spec, &unit, issued, "start")
}

/// Goal: not running.
pub(super) fn stop_with(spec: &ServiceSpec) -> ServiceResult {
    let unit = spec.systemd_unit();
    let issued = systemctl(&["stop", unit.as_str()]);
    match wait_stopped(&unit, STOP_BUDGET) {
        Waited::Done(_) => ok("Service stopped.".into()),
        Waited::TimedOut(u) => fail(format!(
            "Service did not stop: {}.{}",
            u.summary(),
            issued.err().map(|e| format!("\n  {e}")).unwrap_or_default()
        )),
        Waited::Unreachable(err) => unreachable_report("Stop requested", &err),
    }
}

/// Goal: running with a stable PID that differs from before, from the unit file
/// on disk (a stale definition is reloaded first — `systemctl restart` alone
/// would run the old `ExecStart` and exit 0).
pub(super) fn restart_with(spec: &ServiceSpec) -> ServiceResult {
    let unit = spec.systemd_unit();
    let before = match observe(&unit) {
        Observation::Unit(u) => u,
        Observation::Unreachable(err) => return unreachable_report("Restart requested", &err),
    };
    if before.need_reload {
        if let Err(err) = systemctl(&["daemon-reload"]) {
            return fail(format!(
                "The unit file changed on disk and `daemon-reload` failed: {err}"
            ));
        }
    }
    let issued = systemctl(&["restart", unit.as_str()]);
    let r = report_running(spec, &unit, issued, "restart");
    if !r.success {
        return r;
    }
    match observe(&unit) {
        Observation::Unit(after)
            if before.main_pid.is_some() && after.main_pid == before.main_pid =>
        {
            fail(format!(
                "Restart left the previous process running (pid {}).",
                after.main_pid.unwrap_or(0)
            ))
        }
        Observation::Unit(after) if after.need_reload => fail(
            "Restarted, but the manager still reports the unit file as changed on disk.".into(),
        ),
        _ => ok("Service restarted.".into()),
    }
}

fn report_running(
    spec: &ServiceSpec,
    unit: &str,
    issued: Result<(), String>,
    verb: &str,
) -> ServiceResult {
    match wait_running_stable(unit, START_BUDGET) {
        Waited::Done(_) => ok(format!("Service {verb}ed.")),
        Waited::TimedOut(u) => fail(format!(
            "Service did not stay running after {verb}: {}.{}{}{}",
            u.summary(),
            issued
                .err()
                .map(|e| format!("\n  systemctl said: {e}{}", user_manager_hint(&e)))
                .unwrap_or_default(),
            logs_hint(spec),
            if u.exec_main_status.is_some_and(|c| c != 0) {
                "\n  The program exits on its own; the logs above say why."
            } else {
                ""
            }
        )),
        Waited::Unreachable(err) => unreachable_report(&format!("{verb} requested"), &err),
    }
}

pub(super) fn status_with(spec: &ServiceSpec) -> ServiceStatus {
    let unit = spec.systemd_unit();
    let (running, enabled) = match observe(&unit) {
        Observation::Unit(u) => (u.running(), u.unit_file == "enabled"),
        Observation::Unreachable(_) => (false, false),
    };
    let info = if !unit_present(spec) {
        "Service not installed".into()
    } else {
        format!("Service file: {}", spec.systemd_unit_path().display())
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

    #[test]
    fn the_unit_names_the_program_and_the_policy() {
        let content = generate_systemd_service(&ServiceSpec::ctm());
        assert!(content.contains("[Unit]"));
        assert!(content.contains("Type=simple"));
        assert!(content.contains("Restart=on-failure"));
        assert!(content.contains("RestartSec=10s"));
        assert!(content.contains("StartLimitBurst=5"));
        assert!(content.contains("WantedBy=default.target"));
        assert!(content.contains("EnvironmentFile="));
        assert!(content
            .lines()
            .any(|l| l.starts_with("ExecStart=") && l.ends_with(" start")));
    }

    #[test]
    fn a_throwaway_spec_has_no_env_file_line() {
        let s = ServiceSpec::throwaway(
            "ctm-test",
            PathBuf::from("/bin/sleep"),
            vec!["300".into()],
            PathBuf::from("/tmp"),
        );
        let content = generate_systemd_service(&s);
        assert!(!content.contains("EnvironmentFile="));
        assert!(content.contains("ExecStart=/bin/sleep 300"));
        assert!(content.contains("SyslogIdentifier=ctm-test"));
    }

    #[test]
    fn a_missing_user_manager_is_explained_not_just_reported() {
        let hint = user_manager_hint("Failed to connect to bus: No medium found");
        assert!(hint.contains("loginctl enable-linger"), "{hint}");
        assert!(hint.contains("XDG_RUNTIME_DIR"), "{hint}");
    }

    #[test]
    fn unrelated_errors_get_no_invented_advice() {
        assert_eq!(user_manager_hint("Unit foo.service not found."), "");
        assert_eq!(user_manager_hint(""), "");
    }

    #[test]
    fn the_unit_path_is_the_users_systemd_directory() {
        let p = ServiceSpec::ctm().systemd_unit_path();
        assert!(p.ends_with(format!("{SERVICE_NAME}.service")), "{p:?}");
        assert!(
            p.to_string_lossy().contains(".config/systemd/user"),
            "a user unit, which is why every systemctl call passes --user: {p:?}"
        );
    }
}
