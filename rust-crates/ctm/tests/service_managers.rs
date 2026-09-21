//! ADR-019: the service layer against the REAL service manager — systemd's user
//! manager on Linux, launchd's GUI domain on macOS — using a throwaway service so
//! the operator's ctm service is never touched.
//!
//! Every assertion here is a defect that shipped: "Service started." for a
//! program that had already exited, "Service installed" with nothing installed,
//! a restart that left the old process running. Enabled with
//! `CTM_TEST_SERVICE_MANAGER=1`; CI sets it on both platforms.

use ctm::service::{self, ServiceSpec};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn enabled() -> bool {
    if std::env::var("CTM_TEST_SERVICE_MANAGER").as_deref() == Ok("1") {
        return true;
    }
    eprintln!("skip: set CTM_TEST_SERVICE_MANAGER=1 to exercise the real service manager");
    false
}

/// A program that stays up and exits 0 on SIGTERM (so neither manager treats a
/// stop as a crash to recover from).
fn long_lived(dir: &Path, tag: &str) -> PathBuf {
    let p = dir.join(format!("stay-up-{tag}.sh"));
    fs::write(
        &p,
        "#!/bin/sh\ntrap 'exit 0' TERM INT\nwhile :; do sleep 1; done\n",
    )
    .unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    p
}

/// A program that exits immediately with a known status.
fn dies(dir: &Path, status: i32) -> PathBuf {
    let p = dir.join("dies.sh");
    fs::write(&p, format!("#!/bin/sh\nexit {status}\n")).unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    p
}

struct Throwaway {
    spec: ServiceSpec,
    _dir: tempfile::TempDir,
}

impl Throwaway {
    fn new(tag: &str, program: PathBuf, dir: tempfile::TempDir) -> Self {
        let name = format!("ctm-test-{tag}-{}", std::process::id());
        let spec = ServiceSpec::throwaway(&name, program, vec![], dir.path().join("logs"));
        Self { spec, _dir: dir }
    }
}

impl Drop for Throwaway {
    fn drop(&mut self) {
        // Best effort; the assertions above already checked the real uninstall.
        let _ = service::uninstall_with(&self.spec);
    }
}

fn definition_on_disk(spec: &ServiceSpec) -> PathBuf {
    if cfg!(target_os = "macos") {
        spec.launchd_plist_path()
    } else {
        spec.systemd_unit_path()
    }
}

#[test]
fn the_full_lifecycle_is_reported_from_what_the_manager_says() {
    if !enabled() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let program = long_lived(dir.path(), "a");
    let t = Throwaway::new("lifecycle", program.clone(), dir);
    let spec = &t.spec;

    // install: definition on disk, manager knows it, nothing running yet.
    let r = service::install_with(spec);
    assert!(r.success, "install: {}", r.message);
    assert!(definition_on_disk(spec).exists());
    let s = service::status_with(spec);
    assert!(s.enabled, "installed means enabled: {}", s.info);
    assert!(!s.running, "install alone must not start it");
    assert_eq!(service::pid_with(spec), None);

    // start: a stable PID.
    let r = service::start_with(spec);
    assert!(r.success, "start: {}", r.message);
    let pid1 = service::pid_with(spec).expect("running after start");
    assert!(service::status_with(spec).running);

    // start again: idempotent, same process.
    let r = service::start_with(spec);
    assert!(r.success, "second start: {}", r.message);
    assert_eq!(
        service::pid_with(spec),
        Some(pid1),
        "start must not restart a running service"
    );

    // restart: a new process.
    let r = service::restart_with(spec);
    assert!(r.success, "restart: {}", r.message);
    let pid2 = service::pid_with(spec).expect("running after restart");
    assert_ne!(pid2, pid1, "restart must replace the process");

    // the definition on disk changes (a moved binary): restart must run the NEW
    // program, not the one the manager loaded earlier.
    let program_b = long_lived(t._dir.path(), "b");
    let mut moved = spec.clone();
    moved.program = program_b.clone();
    let r = service::install_with(&moved);
    assert!(r.success, "re-install with a new program: {}", r.message);
    let r = service::restart_with(&moved);
    assert!(r.success, "restart after the program moved: {}", r.message);
    let running = service::program_with(&moved).expect("manager reports a program");
    assert_eq!(
        fs::canonicalize(&running).unwrap(),
        fs::canonicalize(&program_b).unwrap(),
        "the manager must be running the definition on disk"
    );
    let pid3 = service::pid_with(&moved).expect("running after the moved restart");
    assert_ne!(pid3, pid2);

    // stop: no process.
    let r = service::stop_with(&moved);
    assert!(r.success, "stop: {}", r.message);
    assert_eq!(service::pid_with(&moved), None, "stopped means no PID");
    assert!(!service::status_with(&moved).running);

    // stop again: idempotent.
    let r = service::stop_with(&moved);
    assert!(r.success, "second stop: {}", r.message);

    // uninstall: nothing left, and the manager agrees.
    let r = service::uninstall_with(&moved);
    assert!(r.success, "uninstall: {}", r.message);
    assert!(!definition_on_disk(&moved).exists(), "definition removed");
    if cfg!(target_os = "linux") {
        assert!(
            moved.systemd_wants_link().symlink_metadata().is_err(),
            "enable symlink removed"
        );
    }
    let s = service::status_with(&moved);
    assert!(!s.enabled && !s.running, "{}", s.info);
    assert_eq!(service::program_with(&moved), None, "the manager forgot it");
}

#[test]
fn a_program_that_exits_at_once_is_a_start_failure_not_a_success() {
    if !enabled() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let program = dies(dir.path(), 3);
    let t = Throwaway::new("dies", program, dir);
    let spec = &t.spec;

    let r = service::install_with(spec);
    assert!(r.success, "install: {}", r.message);
    // Both managers exit 0 from `start` here (spike, ADR-019). The layer must not.
    let r = service::start_with(spec);
    assert!(
        !r.success,
        "start of a dying program reported success: {}",
        r.message
    );
    assert!(
        r.message.contains("did not stay running"),
        "the report names the problem: {}",
        r.message
    );
    assert!(
        r.message.contains('3'),
        "the report carries the program's exit status: {}",
        r.message
    );
    assert_eq!(service::pid_with(spec), None);
    let r = service::uninstall_with(spec);
    assert!(r.success, "uninstall after a failed start: {}", r.message);
}

#[test]
fn uninstalling_what_was_never_installed_is_honest() {
    if !enabled() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let program = long_lived(dir.path(), "never");
    let t = Throwaway::new("never", program, dir);
    // Nothing to remove, nothing loaded: that is the goal state, so success.
    let r = service::uninstall_with(&t.spec);
    assert!(r.success, "{}", r.message);
    assert!(!definition_on_disk(&t.spec).exists());
}
