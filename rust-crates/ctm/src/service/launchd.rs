//! launchd plist generation and lifecycle.
//!
//! ADR-019: every operation here is act → observe → report. `launchctl`'s exit
//! status is never the verdict (`load`/`unload` exit 0 when they did nothing; `start`
//! exits 0 for a job that dies on launch); what `launchctl print` reports afterwards
//! is. `load`/`unload` are not used at all: `bootstrap`/`bootout` are, gated and
//! verified by `print`.

use super::launchd_state::{
    launchctl, observe, wait_not_running, wait_running_stable, wait_unloaded, JobState,
};
use super::*;
use std::time::Duration;

/// Longer than the plist's `ThrottleInterval` (10 s): a first launch that macOS
/// kills and launchd relaunches is ridden out and reported as the success it is.
const START_BUDGET: Duration = Duration::from_secs(14);
const STOP_BUDGET: Duration = Duration::from_secs(10);

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

pub(super) fn generate_launchd_plist(spec: &ServiceSpec) -> String {
    let home = home_dir();
    let log_file = spec.log_dir.join("daemon.log");
    let err_file = spec.log_dir.join("daemon.err.log");

    let mut env_lines = vec![
        format!(
            "        <key>HOME</key>\n        <string>{}</string>",
            escape_xml(&home.display().to_string())
        ),
        format!(
            "        <key>PATH</key>\n        <string>{}</string>",
            escape_xml(&get_macos_path())
        ),
    ];
    for (key, value) in &spec.env {
        env_lines.push(format!(
            "        <key>{}</key>\n        <string>{}</string>",
            escape_xml(key),
            escape_xml(value),
        ));
    }
    let args = std::iter::once(&spec.program.display().to_string())
        .chain(spec.args.iter())
        .map(|a| format!("        <string>{}</string>", escape_xml(a)))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>

    <key>ProgramArguments</key>
    <array>
{args}
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
        label = spec.launchd_label(),
        home_dir = escape_xml(&home.display().to_string()),
        env_block = env_lines.join("\n"),
        log_file = escape_xml(&log_file.display().to_string()),
        err_file = escape_xml(&err_file.display().to_string()),
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

/// The first `ProgramArguments` entry in the plist on disk.
fn plist_program(plist: &Path) -> Option<PathBuf> {
    let text = fs::read_to_string(plist).ok()?;
    let after = text.split("<key>ProgramArguments</key>").nth(1)?;
    let start = after.find("<string>")? + "<string>".len();
    let end = after[start..].find("</string>")? + start;
    Some(PathBuf::from(after[start..end].trim()))
}

fn same_program(a: &Path, b: &Path) -> bool {
    a == b || fs::canonicalize(a).ok() == fs::canonicalize(b).ok()
}

/// Goal: a plist launchd will accept, on disk. Loading is `start`'s job.
pub(super) fn install_with(spec: &ServiceSpec) -> ServiceResult {
    if let Err(e) = fs::create_dir_all(launchd_dir()) {
        return fail(format!("Failed to create LaunchAgents dir: {e}"));
    }
    if let Err(e) = config::ensure_config_dir(&spec.log_dir) {
        return fail(format!("Failed to ensure log dir: {e}"));
    }
    let plist = spec.launchd_plist_path();
    if let Err(e) = fs::write(&plist, generate_launchd_plist(spec)) {
        return fail(format!("Failed to write plist file: {e}"));
    }
    // launchd would reject a malformed plist at bootstrap time with a bare
    // "Input/output error"; check it here, where the message can say so.
    if let Err(err) = run_checked("plutil", &["-lint", "-s", &plist.display().to_string()]) {
        return fail(format!(
            "Wrote {}, but it is not a valid plist: {err}",
            plist.display()
        ));
    }
    ok(format!(
        "Service installed: {plist}\n\nCommands:\n  Start:   ctm service start\n  Stop:    ctm service stop\n  Status:  ctm service status\n  Logs:    tail -f {log}/daemon.log",
        plist = plist.display(),
        log = spec.log_dir.display(),
    ))
}

fn run_checked(program: &str, args: &[&str]) -> Result<(), String> {
    match Command::new(program).args(args).output() {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => Err(String::from_utf8_lossy(&out.stderr).trim().to_string()),
        Err(e) => Err(format!("{program} could not be run: {e}")),
    }
}

/// Goal: not loaded, not running, no plist.
pub(super) fn uninstall_with(spec: &ServiceSpec) -> ServiceResult {
    let target = spec.launchd_target();
    let plist = spec.launchd_plist_path();
    let mut problems = Vec::new();
    if observe(&target).loaded {
        if let Err(err) = launchctl(&["bootout", &target]) {
            problems.push(format!("`launchctl bootout {target}`: {err}"));
        }
        if let Err(j) = wait_unloaded(&target, STOP_BUDGET) {
            problems.push(format!("job still loaded: {}", j.summary()));
        }
    }
    if plist.exists() {
        if let Err(e) = fs::remove_file(&plist) {
            problems.push(format!("could not remove {}: {e}", plist.display()));
        }
    }
    let after = observe(&target);
    if !after.loaded && !plist.exists() {
        ok("Service uninstalled.".into())
    } else {
        fail(format!(
            "Uninstall incomplete: {}{}.\n{}",
            if after.loaded {
                format!("job still loaded ({})", after.summary())
            } else {
                String::new()
            },
            if plist.exists() {
                format!(
                    "{}{} still exists",
                    if after.loaded { "; " } else { "" },
                    plist.display()
                )
            } else {
                String::new()
            },
            problems.join("\n")
        ))
    }
}

/// Bootstrap the plist on disk into the GUI domain unless it is already loaded, and
/// verify launchd knows it afterwards.
fn ensure_loaded(spec: &ServiceSpec) -> Result<JobState, String> {
    let target = spec.launchd_target();
    let before = observe(&target);
    if before.loaded {
        return Ok(before);
    }
    let plist = spec.launchd_plist_path().display().to_string();
    let issued = launchctl(&["bootstrap", &ServiceSpec::launchd_domain(), &plist]);
    let after = observe(&target);
    if after.loaded {
        Ok(after)
    } else {
        Err(match issued {
            Err(e) => format!("`launchctl bootstrap` failed: {e}"),
            Ok(()) => "`launchctl bootstrap` returned, but launchd does not know the job".into(),
        })
    }
}

/// Goal: running with a stable PID.
pub(super) fn start_with(spec: &ServiceSpec) -> ServiceResult {
    if !spec.launchd_plist_path().exists() {
        let installed = install_with(spec);
        if !installed.success {
            return fail(format!(
                "Service is not installed, and installing it failed.\n{}",
                installed.message
            ));
        }
        println!("Service was not installed; installed it first.");
    }
    if let Err(err) = ensure_loaded(spec) {
        return fail(format!("Failed to load the service: {err}"));
    }
    let target = spec.launchd_target();
    // RunAtLoad starts a freshly bootstrapped job by itself; for a loaded-but-idle
    // job, kickstart it. Neither exit status is the verdict.
    let issued = if observe(&target).running() {
        Ok(())
    } else {
        launchctl(&["kickstart", &target])
    };
    report_running(spec, &target, issued, "start")
}

/// Goal: not running (launchd's `stop` is asynchronous, so wait for it).
pub(super) fn stop_with(spec: &ServiceSpec) -> ServiceResult {
    let target = spec.launchd_target();
    if !observe(&target).running() {
        return ok("Service stopped.".into());
    }
    let issued = launchctl(&["stop", &spec.launchd_label()]);
    match wait_not_running(&target, STOP_BUDGET) {
        Ok(_) => ok("Service stopped.".into()),
        Err(j) => fail(format!(
            "Service did not stop: {}.{}",
            j.summary(),
            issued
                .err()
                .map(|e| format!("\n  launchctl said: {e}"))
                .unwrap_or_default()
        )),
    }
}

/// Goal: running with a stable PID that differs from before, as the plist on disk
/// defines it.
///
/// `kickstart -k` restarts launchd's LOADED definition and never re-reads the plist,
/// so when the plist names a different program (binary moved: reinstall elsewhere,
/// migration) the job is booted out and bootstrapped again first — verified at
/// each step, because the old binary silently staying up is exactly the failure
/// this exists to prevent (found live on 2026-09-19).
pub(super) fn restart_with(spec: &ServiceSpec) -> ServiceResult {
    let target = spec.launchd_target();
    let plist = spec.launchd_plist_path();
    if !plist.exists() {
        return fail("Service is not installed; run `ctm service install`.".into());
    }
    let before = observe(&target);
    let on_disk = plist_program(&plist);
    let stale = match (&before.program, &on_disk) {
        (Some(loaded), Some(disk)) => before.loaded && !same_program(loaded, disk),
        _ => false,
    };
    if stale {
        if let Err(err) = launchctl(&["bootout", &target]) {
            return fail(format!(
                "The loaded job runs a different binary and `bootout` failed: {err}"
            ));
        }
        if let Err(j) = wait_unloaded(&target, STOP_BUDGET) {
            return fail(format!(
                "The loaded job runs a different binary and did not unload: {}",
                j.summary()
            ));
        }
    }
    if let Err(err) = ensure_loaded(spec) {
        return fail(format!("Failed to load the service: {err}"));
    }
    let issued = launchctl(&["kickstart", "-k", &target]);
    let r = report_running(spec, &target, issued, "restart");
    if !r.success {
        return r;
    }
    let after = observe(&target);
    if before.pid.is_some() && after.pid == before.pid {
        return fail(format!(
            "Restart left the previous process running (pid {}).",
            after.pid.unwrap_or(0)
        ));
    }
    if let (Some(loaded), Some(disk)) = (&after.program, &on_disk) {
        if !same_program(loaded, disk) {
            return fail(format!(
                "Restarted, but launchd is running {} while the plist names {}.",
                loaded.display(),
                disk.display()
            ));
        }
    }
    ok("Service restarted.".into())
}

fn report_running(
    spec: &ServiceSpec,
    target: &str,
    issued: Result<(), String>,
    verb: &str,
) -> ServiceResult {
    match wait_running_stable(target, START_BUDGET) {
        Ok(_) => ok(format!("Service {verb}ed.")),
        Err(j) => fail(start_failure_hint(
            spec,
            &format!(
                "Service did not stay running after {verb}: {}.{}",
                j.summary(),
                issued
                    .err()
                    .map(|e| format!("\n  launchctl said: {e}"))
                    .unwrap_or_default()
            ),
        )),
    }
}

/// Actionable text for a job that launched but did not stay up.
fn start_failure_hint(spec: &ServiceSpec, headline: &str) -> String {
    format!(
        "{headline}\n\
         \n  Logs:           {log}/daemon.err.log\
         \n  Crash reports:  ls ~/Library/Logs/DiagnosticReports/ctm-*.ips\
         \n  Signature:      codesign -dvv {bin}   (a source build is ad-hoc signed; releases are Developer ID signed)\
         \n  Then:           ctm doctor\n\
         \n\
         launchd keeps retrying in the background (every 10s), so check `ctm status`\n\
         again in a moment before assuming it is down for good.",
        log = spec.log_dir.display(),
        bin = spec.program.display(),
    )
}

pub(super) fn status_with(spec: &ServiceSpec) -> ServiceStatus {
    let plist = spec.launchd_plist_path();
    let enabled = plist.exists();
    let running = observe(&spec.launchd_target()).running();
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

    #[test]
    fn the_plist_names_the_program_and_the_policy() {
        let content = generate_launchd_plist(&ServiceSpec::ctm());
        assert!(content.contains("<key>Label</key>"));
        assert!(content.contains("<string>com.claude.claude-telegram-mirror</string>"));
        assert!(content.contains("<key>KeepAlive</key>"));
        assert!(content.contains("<key>Crashed</key>"));
        assert!(content.contains("<key>ThrottleInterval</key>"));
        assert!(content.contains("<integer>10</integer>"));
        assert!(content.contains("<string>start</string>"));
        assert!(content.contains("<key>StandardErrorPath</key>"));
        assert!(!content.contains("NODE_ENV"), "no TypeScript-era artifacts");
        assert!(!content.contains(".nvm"), "no NVM paths");
    }

    #[test]
    fn a_throwaway_spec_renders_its_own_program_and_args() {
        let s = ServiceSpec::throwaway(
            "ctm-test",
            PathBuf::from("/bin/sleep"),
            vec!["300".into()],
            PathBuf::from("/tmp/ctm-test"),
        );
        let content = generate_launchd_plist(&s);
        assert!(content.contains("<string>com.claude.ctm-test</string>"));
        assert!(content.contains("<string>/bin/sleep</string>\n        <string>300</string>"));
        assert!(content.contains("/tmp/ctm-test/daemon.log"));
    }

    #[test]
    fn plist_program_reads_the_first_argument() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.plist");
        fs::write(&p, generate_launchd_plist(&ServiceSpec::ctm())).unwrap();
        assert_eq!(plist_program(&p), Some(ctm_binary_path()));
    }
}
