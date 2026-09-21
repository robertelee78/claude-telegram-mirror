//! ADR-019: what launchd says about a job — the only thing the ops layer is allowed
//! to report from. `launchctl` exit statuses are not evidence: `load` of an already
//! loaded job and `unload` of an unloaded one both exit 0 (printing "failed"), and
//! `start` of a job whose program exits immediately exits 0 while the job sits in
//! `spawn scheduled` (spike, ADR-019).
//!
//! `launchctl print gui/<uid>/<label>` is the probe: exit 113 means "not loaded";
//! otherwise its text carries `state`, `pid`, `program` and `last exit code`.
//! Parsing is pure; `observe` is the one function that shells out.

use super::*;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct JobState {
    /// launchd knows the job (it was bootstrapped / loaded).
    pub loaded: bool,
    pub pid: Option<i32>,
    /// `running` | `spawn scheduled` | `not running` | …
    pub state: Option<String>,
    pub program: Option<PathBuf>,
    pub last_exit_code: Option<i32>,
}

impl JobState {
    pub fn running(&self) -> bool {
        self.loaded && self.pid.is_some()
    }
    pub fn summary(&self) -> String {
        if !self.loaded {
            return "not loaded".into();
        }
        let mut s = self.state.clone().unwrap_or_else(|| "loaded".into());
        if let Some(p) = self.pid {
            s.push_str(&format!(", pid {p}"));
        }
        if let Some(code) = self.last_exit_code {
            s.push_str(&format!(", last exit code {code}"));
        }
        s
    }
}

/// Parse `launchctl print <target>` output for a job that exists. The job's own
/// keys are the first `state =`, `pid =`, `program =` and `last exit code =`
/// lines; deeper-indented repeats belong to its endpoints and are ignored.
pub fn parse_print(text: &str) -> JobState {
    let mut j = JobState {
        loaded: true,
        ..Default::default()
    };
    for raw in text.lines() {
        let line = raw.trim();
        if j.state.is_none() {
            if let Some(v) = line.strip_prefix("state = ") {
                j.state = Some(v.trim().to_string());
                continue;
            }
        }
        if j.pid.is_none() {
            if let Some(v) = line.strip_prefix("pid = ") {
                j.pid = v.trim().parse::<i32>().ok().filter(|p| *p > 0);
                continue;
            }
        }
        if j.program.is_none() {
            if let Some(v) = line.strip_prefix("program = ") {
                j.program = Some(PathBuf::from(v.trim()));
                continue;
            }
        }
        if j.last_exit_code.is_none() {
            if let Some(v) = line.strip_prefix("last exit code = ") {
                j.last_exit_code = v.trim().parse().ok();
            }
        }
    }
    j
}

/// Ask launchd. Any failure of `print` (exit 113 when the job is not in the domain)
/// is "not loaded"; there is no separate "manager unreachable" state for the GUI
/// domain of the calling user.
pub(super) fn observe(target: &str) -> JobState {
    match Command::new("launchctl").args(["print", target]).output() {
        Ok(out) if out.status.success() => parse_print(&String::from_utf8_lossy(&out.stdout)),
        _ => JobState::default(),
    }
}

pub(super) fn wait_until(
    target: &str,
    budget: Duration,
    mut done: impl FnMut(&JobState) -> bool,
) -> Result<JobState, JobState> {
    const POLL: Duration = Duration::from_millis(500);
    let deadline = Instant::now() + budget;
    let mut last = observe(target);
    loop {
        if done(&last) {
            return Ok(last);
        }
        if Instant::now() >= deadline {
            return Err(last);
        }
        std::thread::sleep(POLL);
        last = observe(target);
    }
}

/// Running with the same PID seen twice in a row: it survived launch. A job that
/// launchd keeps respawning (`spawn scheduled`) never satisfies this, and the
/// budget must exceed `ThrottleInterval` (10 s) so a first-launch kill that
/// launchd recovers from is reported as the success it becomes.
pub(super) fn wait_running_stable(target: &str, budget: Duration) -> Result<JobState, JobState> {
    let mut last_pid = None;
    wait_until(target, budget, |j| {
        let stable = j.running() && j.pid == last_pid;
        last_pid = j.pid;
        stable
    })
}

pub(super) fn wait_not_running(target: &str, budget: Duration) -> Result<JobState, JobState> {
    wait_until(target, budget, |j| !j.running())
}

pub(super) fn wait_unloaded(target: &str, budget: Duration) -> Result<JobState, JobState> {
    wait_until(target, budget, |j| !j.loaded)
}

/// Run one `launchctl` verb and keep its stderr for the report.
pub(super) fn launchctl(args: &[&str]) -> Result<(), String> {
    match Command::new("launchctl").args(args).output() {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            Err(if err.is_empty() {
                format!("`launchctl {}` exited {}", args.join(" "), out.status)
            } else {
                err
            })
        }
        Err(e) => Err(format!("launchctl could not be run: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Captured on macOS 26.6 (ADR-019 spike): a running job, then one whose
    // program exits 1 (launchd keeps scheduling it).
    const RUNNING: &str = "gui/501/com.ctm.spike = {\n\tactive count = 1\n\tpath = /Users/runner/Library/LaunchAgents/com.ctm.spike.plist\n\tstate = running\n\n\tprogram = /bin/sleep\n\targuments = {\n\t\t/bin/sleep\n\t\t300\n\t}\n\n\tpid = 17855\n\tendpoints = {\n\t\t\"com.ctm.spike\" = {\n\t\t\tstate = active\n\t\t}\n\t}\n}\n";
    const DYING: &str = "gui/501/com.ctm.dies = {\n\tstate = spawn scheduled\n\tprogram = /usr/bin/false\n\tlast exit code = 1\n\tendpoints = {\n\t\t\"x\" = {\n\t\t\tstate = active\n\t\t}\n\t}\n}\n";

    #[test]
    fn a_running_job_is_running() {
        let j = parse_print(RUNNING);
        assert!(j.loaded && j.running());
        assert_eq!(j.pid, Some(17855));
        assert_eq!(j.state.as_deref(), Some("running"));
        assert_eq!(j.program, Some(PathBuf::from("/bin/sleep")));
    }

    #[test]
    fn a_job_launchd_keeps_respawning_is_not_running_even_though_start_exited_zero() {
        let j = parse_print(DYING);
        assert!(j.loaded && !j.running());
        assert_eq!(j.state.as_deref(), Some("spawn scheduled"));
        assert_eq!(j.last_exit_code, Some(1));
        assert!(j.summary().contains("spawn scheduled"));
        assert!(j.summary().contains("last exit code 1"));
    }

    #[test]
    fn the_endpoint_state_does_not_shadow_the_job_state() {
        // `state = active` appears deeper in the tree for endpoints; the job's
        // own `state = spawn scheduled` comes first and must win.
        assert_eq!(parse_print(DYING).state.as_deref(), Some("spawn scheduled"));
    }

    #[test]
    fn not_loaded_is_the_default() {
        let j = JobState::default();
        assert!(!j.loaded && !j.running());
        assert_eq!(j.summary(), "not loaded");
    }
}
