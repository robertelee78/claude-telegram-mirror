//! ADR-019: what the systemd user manager says about a unit — the only thing the
//! ops layer is allowed to report from. `systemctl` exit statuses are not evidence:
//! `start` of a unit whose program exits immediately returns 0 (the unit sits in
//! `activating/auto-restart`), and `restart` after the unit file changed returns 0
//! while running the old `ExecStart` (spike, ADR-019).
//!
//! Parsing is pure; `observe` is the one function that shells out.

use super::*;
use std::time::{Duration, Instant};

const SHOW_PROPS: &str = "LoadState,ActiveState,SubState,MainPID,UnitFileState,NeedDaemonReload,Result,ExecMainStatus,ExecStart";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UnitState {
    /// `loaded` | `not-found` | `bad-setting` | …
    pub load: String,
    /// `active` | `activating` | `deactivating` | `inactive` | `failed`
    pub active: String,
    /// `running` | `auto-restart` | `dead` | `exited` | …
    pub sub: String,
    pub main_pid: Option<u32>,
    /// `enabled` | `disabled` | `` (unknown unit)
    pub unit_file: String,
    /// The unit file on disk differs from what the manager loaded.
    pub need_reload: bool,
    /// Last run's result: `success` | `exit-code` | `signal` | …
    pub result: String,
    /// Last main process exit status, when `result` is `exit-code`.
    pub exec_main_status: Option<i32>,
    /// The program the manager will exec — from its loaded definition, which is
    /// stale when `need_reload` is set.
    pub exec_path: Option<PathBuf>,
}

impl UnitState {
    pub fn known(&self) -> bool {
        self.load == "loaded"
    }
    pub fn running(&self) -> bool {
        self.active == "active" && self.main_pid.is_some()
    }
    pub fn stopped(&self) -> bool {
        matches!(self.active.as_str(), "inactive" | "failed") && self.main_pid.is_none()
    }
    /// One line for a failure report.
    pub fn summary(&self) -> String {
        let mut s = format!("{}/{}", self.active, self.sub);
        if let Some(p) = self.main_pid {
            s.push_str(&format!(", pid {p}"));
        }
        if self.result != "success" && !self.result.is_empty() {
            s.push_str(&format!(", last result {}", self.result));
            if let Some(code) = self.exec_main_status {
                s.push_str(&format!(" (status {code})"));
            }
        }
        s
    }
}

/// `systemctl --user show <unit> -p …` prints one `Key=Value` per line.
pub fn parse_show(text: &str) -> UnitState {
    let mut u = UnitState::default();
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let v = v.trim();
        match k.trim() {
            "LoadState" => u.load = v.to_string(),
            "ActiveState" => u.active = v.to_string(),
            "SubState" => u.sub = v.to_string(),
            "MainPID" => u.main_pid = v.parse::<u32>().ok().filter(|p| *p > 0),
            "UnitFileState" => u.unit_file = v.to_string(),
            "NeedDaemonReload" => u.need_reload = v == "yes",
            "Result" => u.result = v.to_string(),
            "ExecMainStatus" => u.exec_main_status = v.parse().ok(),
            // `ExecStart={ path=/bin/sleep ; argv[]=/bin/sleep 300 ; … }`
            "ExecStart" => {
                u.exec_path = v
                    .split_once("path=")
                    .and_then(|(_, rest)| rest.split(" ;").next())
                    .map(|p| PathBuf::from(p.trim()));
            }
            _ => {}
        }
    }
    u
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation {
    Unit(UnitState),
    /// `systemctl --user` could not talk to a user manager at all.
    Unreachable(String),
}

/// Ask the manager. The `show` verb succeeds even for an unknown unit
/// (`LoadState=not-found`), so a failure here means the manager itself.
pub(super) fn observe(unit: &str) -> Observation {
    match Command::new("systemctl")
        .args(["--user", "show", unit, "-p", SHOW_PROPS])
        .output()
    {
        Ok(out) if out.status.success() => {
            Observation::Unit(parse_show(&String::from_utf8_lossy(&out.stdout)))
        }
        Ok(out) => {
            Observation::Unreachable(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
        Err(e) => Observation::Unreachable(format!("systemctl could not be run: {e}")),
    }
}

/// What a bounded wait ended with: the goal, the last state seen at the
/// deadline, or a manager that could not be asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Waited {
    Done(UnitState),
    TimedOut(UnitState),
    Unreachable(String),
}

/// Poll until `done(state)` holds, or the budget is spent. The last observation
/// is returned either way so the caller can report what it saw.
pub(super) fn wait_until(
    unit: &str,
    budget: Duration,
    mut done: impl FnMut(&UnitState) -> bool,
) -> Waited {
    const POLL: Duration = Duration::from_millis(500);
    let deadline = Instant::now() + budget;
    let mut last = observe(unit);
    loop {
        if let Observation::Unit(u) = &last {
            if done(u) {
                return Waited::Done(u.clone());
            }
        }
        if Instant::now() >= deadline {
            return match last {
                Observation::Unit(u) => Waited::TimedOut(u),
                Observation::Unreachable(e) => Waited::Unreachable(e),
            };
        }
        std::thread::sleep(POLL);
        last = observe(unit);
    }
}

/// Running with the same non-zero MainPID seen twice in a row: it survived
/// launch. A unit in `activating/auto-restart` never satisfies this.
pub(super) fn wait_running_stable(unit: &str, budget: Duration) -> Waited {
    let mut last_pid = None;
    wait_until(unit, budget, |u| {
        let stable = u.running() && u.main_pid == last_pid;
        last_pid = u.main_pid;
        stable
    })
}

pub(super) fn wait_stopped(unit: &str, budget: Duration) -> Waited {
    wait_until(unit, budget, |u| u.stopped() || !u.known())
}

/// Run one `systemctl --user` verb and keep its stderr for the report.
pub(super) fn systemctl(args: &[&str]) -> Result<(), String> {
    match Command::new("systemctl").arg("--user").args(args).output() {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            Err(if err.is_empty() {
                format!(
                    "`systemctl --user {}` exited {}",
                    args.join(" "),
                    out.status
                )
            } else {
                err
            })
        }
        Err(e) => Err(format!("systemctl could not be run: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Captured on ubuntu-24.04 / systemd 255 (ADR-019 spike).
    const RUNNING: &str = "LoadState=loaded\nActiveState=active\nSubState=running\nMainPID=2063\nUnitFileState=enabled\nNeedDaemonReload=no\nResult=success\nExecMainStatus=0\nExecStart={ path=/bin/sleep ; argv[]=/bin/sleep 300 ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }\n";
    const DYING: &str = "LoadState=loaded\nActiveState=activating\nSubState=auto-restart\nMainPID=0\nUnitFileState=enabled\nNeedDaemonReload=no\nResult=exit-code\nExecMainStatus=1\n";
    const GONE: &str = "LoadState=not-found\nActiveState=inactive\nSubState=dead\nMainPID=0\nUnitFileState=\nNeedDaemonReload=no\nResult=success\n";

    #[test]
    fn a_running_unit_is_running() {
        let u = parse_show(RUNNING);
        assert!(u.known() && u.running() && !u.stopped());
        assert_eq!(u.main_pid, Some(2063));
        assert!(!u.need_reload);
        assert_eq!(u.exec_path, Some(PathBuf::from("/bin/sleep")));
    }

    #[test]
    fn a_unit_whose_program_died_is_not_running_even_though_start_exited_zero() {
        // The exact hole: `systemctl start` returned 0 for this unit.
        let u = parse_show(DYING);
        assert!(!u.running());
        assert!(!u.stopped(), "auto-restart is neither running nor stopped");
        assert_eq!(u.exec_main_status, Some(1));
        assert!(u.summary().contains("activating/auto-restart"));
        assert!(u.summary().contains("status 1"));
    }

    #[test]
    fn a_removed_unit_is_not_found() {
        let u = parse_show(GONE);
        assert!(!u.known());
        assert!(u.stopped());
        assert_eq!(u.unit_file, "");
    }

    #[test]
    fn need_daemon_reload_is_read() {
        let u = parse_show("NeedDaemonReload=yes\nLoadState=loaded\n");
        assert!(u.need_reload);
    }

    #[test]
    fn a_zero_pid_is_no_pid() {
        assert_eq!(parse_show("MainPID=0\n").main_pid, None);
        assert_eq!(parse_show("MainPID=abc\n").main_pid, None);
    }
}
