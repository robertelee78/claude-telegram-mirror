//! Config validation integration tests.
//!
//! Tests config utilities, validation functions, and path safety.
//! NOTE: Tests that call `load_config()` are sensitive to the host's
//! `~/.config/claude-telegram-mirror/config.json`. We avoid asserting
//! on values that come from the config file and instead focus on
//! functions with deterministic behavior.

use ctm::config;
use tempfile::tempdir;

#[test]
fn load_config_without_require_auth_succeeds() {
    // load_config(false) should always succeed regardless of env/file state
    let result = config::load_config(false);
    assert!(
        result.is_ok(),
        "load_config(false) should succeed: {:?}",
        result.err()
    );
}

#[test]
fn load_config_returns_expected_default_types() {
    // Verify that non-credential fields have sane defaults/types
    let cfg = config::load_config(false).unwrap();
    // These are always defaulted regardless of config file
    assert!(cfg.chunk_size > 0, "chunk_size should be positive");
    assert!(cfg.rate_limit > 0, "rate_limit should be positive");
    assert!(
        cfg.session_timeout > 0,
        "session_timeout should be positive"
    );
    assert!(
        !cfg.socket_path.as_os_str().is_empty(),
        "socket_path should not be empty"
    );
}

#[test]
fn load_config_require_auth_with_env_vars_succeeds() {
    // Set both required env vars, should succeed regardless of config file
    std::env::set_var("TELEGRAM_BOT_TOKEN", "test-token-for-validation");
    std::env::set_var("TELEGRAM_CHAT_ID", "123456");

    let result = config::load_config(true);

    // Cleanup
    std::env::remove_var("TELEGRAM_BOT_TOKEN");
    std::env::remove_var("TELEGRAM_CHAT_ID");

    assert!(
        result.is_ok(),
        "load_config(true) should succeed with both env vars set"
    );
    let cfg = result.unwrap();
    assert_eq!(cfg.bot_token, "test-token-for-validation");
    assert_eq!(cfg.chat_id, 123456);
}

#[test]
fn validate_config_reports_empty_credentials() {
    // Construct a Config-like scenario via validate_config with known empty fields.
    // We create a config with empty bot_token and zero chat_id via env overrides.
    std::env::set_var("TELEGRAM_BOT_TOKEN", "");
    std::env::set_var("TELEGRAM_CHAT_ID", "0");
    std::env::set_var("TELEGRAM_MIRROR", "false");

    let cfg = config::load_config(false).unwrap();

    // Cleanup
    std::env::remove_var("TELEGRAM_BOT_TOKEN");
    std::env::remove_var("TELEGRAM_CHAT_ID");
    std::env::remove_var("TELEGRAM_MIRROR");

    // Env vars take priority over config file, so these should always hold
    let (errors, warnings) = config::validate_config(&cfg);
    assert!(
        errors.iter().any(|e| e.contains("BOT_TOKEN")),
        "Should report missing bot token in errors: {:?}",
        errors
    );
    assert!(
        errors.iter().any(|e| e.contains("CHAT_ID")),
        "Should report missing chat_id in errors: {:?}",
        errors
    );
    assert!(
        warnings.iter().any(|w| w.contains("TELEGRAM_MIRROR")),
        "Should warn about mirror not enabled: {:?}",
        warnings
    );
}

#[test]
fn validate_socket_path_rejects_traversal() {
    assert!(!config::validate_socket_path("/tmp/../etc/evil.sock"));
    assert!(!config::validate_socket_path(""));
    assert!(!config::validate_socket_path("relative/path.sock"));
}

#[test]
fn validate_socket_path_accepts_valid() {
    assert!(config::validate_socket_path("/tmp/bridge.sock"));
    assert!(config::validate_socket_path(
        "/home/user/.config/ctm/bridge.sock"
    ));
}

#[test]
fn validate_socket_path_rejects_too_long() {
    let long_path = format!("/{}", "a".repeat(104));
    assert!(
        !config::validate_socket_path(&long_path),
        "Socket paths over 104 bytes should be rejected (AF_UNIX limit)"
    );
}

#[test]
fn ensure_config_dir_creates_directory() {
    let tmp = tempdir().unwrap();
    let sub = tmp.path().join("newdir");
    assert!(!sub.exists());

    config::ensure_config_dir(&sub).expect("ensure_config_dir should succeed");
    assert!(sub.exists());

    // Verify permissions are 0o700
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::metadata(&sub).unwrap().permissions();
        assert_eq!(
            perms.mode() & 0o777,
            0o700,
            "Config dir should have 0o700 permissions"
        );
    }
}

#[test]
fn ensure_config_dir_idempotent() {
    let tmp = tempdir().unwrap();
    let sub = tmp.path().join("idempotent");

    config::ensure_config_dir(&sub).unwrap();
    // Second call should not fail
    config::ensure_config_dir(&sub).unwrap();
    assert!(sub.exists());
}

#[test]
fn mirror_status_round_trip() {
    let tmp = tempdir().unwrap();

    // Default: true when file doesn't exist
    assert!(config::read_mirror_status(tmp.path()));

    // Write false, read back
    config::write_mirror_status(tmp.path(), false, Some(1234));
    assert!(!config::read_mirror_status(tmp.path()));

    // Write true, read back
    config::write_mirror_status(tmp.path(), true, None);
    assert!(config::read_mirror_status(tmp.path()));
}

#[test]
fn mirror_status_corrupt_file_defaults_true() {
    let tmp = tempdir().unwrap();
    std::fs::write(tmp.path().join("status.json"), "not valid json").unwrap();
    assert!(
        config::read_mirror_status(tmp.path()),
        "Corrupt status file should default to true"
    );
}

#[test]
fn mirror_status_file_permissions() {
    let tmp = tempdir().unwrap();
    config::write_mirror_status(tmp.path(), true, None);

    let status_path = tmp.path().join("status.json");
    assert!(status_path.exists());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::metadata(&status_path).unwrap().permissions();
        assert_eq!(
            perms.mode() & 0o777,
            0o600,
            "Status file should have 0o600 permissions"
        );
    }
}
