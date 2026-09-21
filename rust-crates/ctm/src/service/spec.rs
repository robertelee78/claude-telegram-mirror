//! ADR-019: what a managed service *is*, independent of which manager runs it.
//!
//! `ServiceSpec::ctm()` is the daemon the CLI installs. A test builds a spec that runs
//! a throwaway program under a throwaway label, so the real manager can be exercised
//! end to end without touching the operator's service.

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceSpec {
    /// systemd unit basename (`<name>.service`); launchd label is `com.claude.<name>`.
    pub name: String,
    pub description: String,
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Environment for the process. launchd embeds it in the plist; systemd reads
    /// `env_file` instead, when set.
    pub env: Vec<(String, String)>,
    /// systemd `EnvironmentFile=`; also the file `install` (re)generates for ctm.
    pub env_file: Option<PathBuf>,
    /// Where launchd's stdout/stderr logs go; also systemd's `ReadWritePaths=`.
    pub log_dir: PathBuf,
}

impl ServiceSpec {
    /// The ctm daemon: this binary, `ctm start`, env from `~/.telegram-env`.
    pub fn ctm() -> Self {
        let home = home_dir();
        let mut env: Vec<(String, String)> = env::parse_env_file(&env_file_path())
            .into_iter()
            .filter(|(k, _)| k != "HOME" && k != "PATH")
            .collect();
        env.sort();
        Self {
            name: SERVICE_NAME.to_string(),
            description: "Claude Code Telegram Mirror Bridge".to_string(),
            program: ctm_binary_path(),
            args: vec!["start".to_string()],
            env,
            env_file: Some(systemd_env_file_path()),
            log_dir: home.join(".config").join(SERVICE_NAME),
        }
    }

    /// A throwaway service for exercising the real manager in tests
    /// (`tests/service_managers.rs`, through the library crate; the binary never
    /// builds one).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn throwaway(name: &str, program: PathBuf, args: Vec<String>, log_dir: PathBuf) -> Self {
        Self {
            name: name.to_string(),
            description: format!("ctm test service {name}"),
            program,
            args,
            env: Vec::new(),
            env_file: None,
            log_dir,
        }
    }

    // ---- systemd
    pub fn systemd_unit(&self) -> String {
        format!("{}.service", self.name)
    }
    pub fn systemd_unit_path(&self) -> PathBuf {
        systemd_user_dir().join(self.systemd_unit())
    }
    /// The symlink `systemctl --user enable` creates and `disable` removes.
    pub fn systemd_wants_link(&self) -> PathBuf {
        systemd_user_dir()
            .join("default.target.wants")
            .join(self.systemd_unit())
    }

    // ---- launchd
    pub fn launchd_label(&self) -> String {
        format!("com.claude.{}", self.name)
    }
    pub fn launchd_plist_path(&self) -> PathBuf {
        launchd_dir().join(format!("{}.plist", self.launchd_label()))
    }
    /// The service target in the user's GUI domain: `gui/<uid>/<label>`.
    pub fn launchd_target(&self) -> String {
        format!("gui/{}/{}", launchd_uid(), self.launchd_label())
    }
    pub fn launchd_domain() -> String {
        format!("gui/{}", launchd_uid())
    }
}

fn launchd_uid() -> u32 {
    #[cfg(unix)]
    {
        nix::unistd::getuid().as_raw()
    }
    #[cfg(not(unix))]
    {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ctm_spec_is_this_binary_running_start() {
        let s = ServiceSpec::ctm();
        assert_eq!(s.args, vec!["start"]);
        assert_eq!(s.program, ctm_binary_path());
        assert!(s
            .systemd_unit_path()
            .ends_with("claude-telegram-mirror.service"));
        assert!(s
            .launchd_plist_path()
            .ends_with("com.claude.claude-telegram-mirror.plist"));
        assert!(s.launchd_target().starts_with("gui/"));
        assert!(s
            .launchd_target()
            .ends_with("/com.claude.claude-telegram-mirror"));
    }

    #[test]
    fn a_throwaway_spec_never_collides_with_ctm() {
        let s = ServiceSpec::throwaway(
            "ctm-test-1",
            PathBuf::from("/bin/sleep"),
            vec!["300".into()],
            PathBuf::from("/tmp"),
        );
        assert_eq!(s.systemd_unit(), "ctm-test-1.service");
        assert_eq!(s.launchd_label(), "com.claude.ctm-test-1");
        assert!(s.env_file.is_none());
    }
}
