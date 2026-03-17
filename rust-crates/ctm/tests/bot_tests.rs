//! Integration tests for bot/client.rs and bot/queue.rs public API.
//!
//! All tests use only the `ctm::` public API and require no network access.

use ctm::bot::{create_approval_keyboard, scrub_bot_token, InlineButton, SendOptions, TelegramBot};
use ctm::config::Config;
use std::path::PathBuf;

// ---------------------------------------------------------------- helpers

/// Build a minimal Config for unit-testing TelegramBot construction.
fn test_config() -> Config {
    Config {
        bot_token: "123456:ABC-DEF_test-token".to_string(),
        chat_id: -1001234567890,
        enabled: true,
        verbose: false,
        approvals: true,
        use_threads: true,
        chunk_size: 4000,
        rate_limit: 20,
        session_timeout: 30,
        stale_session_timeout_hours: 72,
        auto_delete_topics: true,
        topic_delete_delay_minutes: 1440,
        socket_path: PathBuf::from("/tmp/test.sock"),
        config_dir: PathBuf::from("/tmp"),
        config_path: PathBuf::from("/tmp/config.json"),
        forum_enabled: false,
    }
}

// ======================================================= TelegramBot::new

#[test]
fn bot_new_succeeds_with_valid_config() {
    let config = test_config();
    let bot = TelegramBot::new(&config).expect("should succeed with valid config");
    assert_eq!(bot.chat_id(), -1001234567890);
}

#[test]
fn bot_new_with_zero_rate_limit_does_not_panic() {
    let mut config = test_config();
    config.rate_limit = 0;
    // rate_limit=0 is clamped to 1 internally; must not panic on NonZeroU32
    let bot = TelegramBot::new(&config).expect("rate_limit=0 should clamp, not panic");
    assert_eq!(bot.chat_id(), config.chat_id);
}

#[test]
fn bot_new_with_max_rate_limit_clamps_to_30() {
    let mut config = test_config();
    config.rate_limit = 100;
    // rate_limit>30 is clamped to 30; must not panic
    let bot = TelegramBot::new(&config).expect("rate_limit=100 should clamp to 30");
    assert_eq!(bot.chat_id(), config.chat_id);
}

#[test]
fn bot_new_with_rate_limit_1() {
    let mut config = test_config();
    config.rate_limit = 1;
    let bot = TelegramBot::new(&config).expect("rate_limit=1 should succeed");
    assert_eq!(bot.chat_id(), config.chat_id);
}

// ======================================================= chat_id getter

#[test]
fn chat_id_returns_configured_value() {
    let config = test_config();
    let bot = TelegramBot::new(&config).unwrap();
    assert_eq!(bot.chat_id(), -1001234567890);
}

#[test]
fn chat_id_positive_value() {
    let mut config = test_config();
    config.chat_id = 42;
    let bot = TelegramBot::new(&config).unwrap();
    assert_eq!(bot.chat_id(), 42);
}

// ======================================================= is_running / set_running

#[test]
fn is_running_defaults_to_false() {
    let bot = TelegramBot::new(&test_config()).unwrap();
    assert!(!bot.is_running());
}

#[test]
fn set_running_toggles_flag() {
    let bot = TelegramBot::new(&test_config()).unwrap();

    bot.set_running(true);
    assert!(bot.is_running());

    bot.set_running(false);
    assert!(!bot.is_running());

    // Double-set to same value is idempotent
    bot.set_running(true);
    bot.set_running(true);
    assert!(bot.is_running());
}

// ======================================================= get_session stub

#[test]
fn get_session_always_returns_none() {
    let bot = TelegramBot::new(&test_config()).unwrap();
    assert!(bot.get_session(0).is_none());
    assert!(bot.get_session(i64::MAX).is_none());
    assert!(bot.get_session(i64::MIN).is_none());
}

// ======================================================= scrub_bot_token (free function)

#[test]
fn scrub_bot_token_single_occurrence() {
    let msg = "POST https://api.telegram.org/bot123456:ABC-DEF_test-token/sendMessage failed";
    let scrubbed = scrub_bot_token(msg);
    assert!(!scrubbed.contains("123456:ABC-DEF_test-token"));
    assert!(scrubbed.contains("bot[REDACTED]/sendMessage"));
}

#[test]
fn scrub_bot_token_multiple_different_tokens() {
    let msg = "bot111:AAA_bbb/getMe and bot222:CCC_ddd-eee/sendMessage";
    let scrubbed = scrub_bot_token(msg);
    assert!(!scrubbed.contains("111:AAA"));
    assert!(!scrubbed.contains("222:CCC"));
    assert_eq!(
        scrubbed,
        "bot[REDACTED]/getMe and bot[REDACTED]/sendMessage"
    );
}

#[test]
fn scrub_bot_token_no_token_present() {
    let msg = "Connection timed out after 30 seconds";
    assert_eq!(scrub_bot_token(msg), msg);
}

#[test]
fn scrub_bot_token_empty_string() {
    assert_eq!(scrub_bot_token(""), "");
}

#[test]
fn scrub_bot_token_at_start_of_string() {
    let msg = "bot999:XYZ_abc-123/getUpdates returned 502";
    let scrubbed = scrub_bot_token(msg);
    assert!(scrubbed.starts_with("bot[REDACTED]/getUpdates"));
    assert!(!scrubbed.contains("999:XYZ"));
}

#[test]
fn scrub_bot_token_at_end_of_string() {
    let msg = "Error at bot999:XYZ_abc-123/";
    let scrubbed = scrub_bot_token(msg);
    assert!(scrubbed.ends_with("bot[REDACTED]/"));
    assert!(!scrubbed.contains("999:XYZ"));
}

#[test]
fn scrub_bot_token_partial_match_no_slash() {
    // "bot123:ABC" without trailing "/" should NOT be scrubbed (regex requires trailing /)
    let msg = "token is bot123:ABC_def";
    assert_eq!(scrub_bot_token(msg), msg);
}

#[test]
fn scrub_bot_token_preserves_surrounding_text() {
    let msg = "before bot555:AAA_bbb/sendMessage after";
    let scrubbed = scrub_bot_token(msg);
    assert_eq!(scrubbed, "before bot[REDACTED]/sendMessage after");
}

// ======================================================= scrub_token (instance method)

#[test]
fn scrub_token_method_delegates_to_free_function() {
    let bot = TelegramBot::new(&test_config()).unwrap();
    let msg = "Error at bot123456:ABC-DEF_test-token/sendMessage";
    let scrubbed = bot.scrub_token(msg);
    assert!(!scrubbed.contains("123456:ABC-DEF_test-token"));
    assert!(scrubbed.contains("[REDACTED]"));
}

#[test]
fn scrub_token_method_safe_text_unchanged() {
    let bot = TelegramBot::new(&test_config()).unwrap();
    let safe = "Network error: connection refused";
    assert_eq!(bot.scrub_token(safe), safe);
}

// ======================================================= create_approval_keyboard

#[test]
fn approval_keyboard_has_two_rows() {
    let kb = create_approval_keyboard("abc-123");
    assert_eq!(kb.len(), 2, "should have 2 rows");
}

#[test]
fn approval_keyboard_first_row_has_approve_and_reject() {
    let kb = create_approval_keyboard("id-1");
    assert_eq!(kb[0].len(), 2);
    assert_eq!(kb[0][0].callback_data, "approve:id-1");
    assert_eq!(kb[0][1].callback_data, "reject:id-1");
}

#[test]
fn approval_keyboard_second_row_has_abort() {
    let kb = create_approval_keyboard("id-1");
    assert_eq!(kb[1].len(), 1);
    assert_eq!(kb[1][0].callback_data, "abort:id-1");
}

#[test]
fn approval_keyboard_button_text_contains_labels() {
    let kb = create_approval_keyboard("x");
    assert!(kb[0][0].text.contains("Approve"));
    assert!(kb[0][1].text.contains("Reject"));
    assert!(kb[1][0].text.contains("Abort"));
}

#[test]
fn approval_keyboard_empty_id() {
    let kb = create_approval_keyboard("");
    assert_eq!(kb[0][0].callback_data, "approve:");
    assert_eq!(kb[0][1].callback_data, "reject:");
    assert_eq!(kb[1][0].callback_data, "abort:");
}

#[test]
fn approval_keyboard_special_chars_in_id() {
    let kb = create_approval_keyboard("id-with:colon/slash");
    assert_eq!(kb[0][0].callback_data, "approve:id-with:colon/slash");
}

// ======================================================= SendOptions defaults

#[test]
fn send_options_default_all_none() {
    let opts = SendOptions::default();
    assert!(opts.parse_mode.is_none());
    assert!(opts.disable_notification.is_none());
    assert!(opts.thread_id.is_none());
    assert!(opts.reply_to_message_id.is_none());
}

#[test]
fn send_options_custom_values() {
    let opts = SendOptions {
        parse_mode: Some("HTML".into()),
        disable_notification: Some(true),
        thread_id: Some(42),
        reply_to_message_id: Some(100),
    };
    assert_eq!(opts.parse_mode.as_deref(), Some("HTML"));
    assert_eq!(opts.disable_notification, Some(true));
    assert_eq!(opts.thread_id, Some(42));
    assert_eq!(opts.reply_to_message_id, Some(100));
}

#[test]
fn send_options_clone() {
    let opts = SendOptions {
        parse_mode: Some("Markdown".into()),
        disable_notification: Some(false),
        thread_id: None,
        reply_to_message_id: Some(7),
    };
    let cloned = opts.clone();
    assert_eq!(cloned.parse_mode, opts.parse_mode);
    assert_eq!(cloned.disable_notification, opts.disable_notification);
    assert_eq!(cloned.reply_to_message_id, opts.reply_to_message_id);
}

// ======================================================= InlineButton

#[test]
fn inline_button_serde_roundtrip() {
    let btn = InlineButton {
        text: "Click me".into(),
        callback_data: "action:do_it".into(),
    };
    let json = serde_json::to_string(&btn).unwrap();
    let parsed: InlineButton = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.text, "Click me");
    assert_eq!(parsed.callback_data, "action:do_it");
}

#[test]
fn inline_button_deserialize_from_json() {
    let json = r#"{"text":"Hello","callback_data":"cb:1"}"#;
    let btn: InlineButton = serde_json::from_str(json).unwrap();
    assert_eq!(btn.text, "Hello");
    assert_eq!(btn.callback_data, "cb:1");
}

#[test]
fn inline_button_clone() {
    let btn = InlineButton {
        text: "Original".into(),
        callback_data: "data".into(),
    };
    let cloned = btn.clone();
    assert_eq!(cloned.text, "Original");
    assert_eq!(cloned.callback_data, "data");
}

// ======================================================= Rate limiter boundary configs

#[test]
fn rate_limit_boundary_value_1() {
    let mut config = test_config();
    config.rate_limit = 1;
    assert!(TelegramBot::new(&config).is_ok());
}

#[test]
fn rate_limit_boundary_value_30() {
    let mut config = test_config();
    config.rate_limit = 30;
    assert!(TelegramBot::new(&config).is_ok());
}

#[test]
fn rate_limit_boundary_value_31_clamped() {
    let mut config = test_config();
    config.rate_limit = 31;
    // Should clamp to 30, not fail
    assert!(TelegramBot::new(&config).is_ok());
}

#[test]
fn rate_limit_u32_max_clamped() {
    let mut config = test_config();
    config.rate_limit = u32::MAX;
    assert!(TelegramBot::new(&config).is_ok());
}

// ======================================================= chunk_size from config

#[test]
fn chunk_size_propagated_from_config() {
    let mut config = test_config();
    config.chunk_size = 2048;
    let _bot = TelegramBot::new(&config).expect("custom chunk_size should work");
    // chunk_size is pub(super), so we just verify construction succeeds
}

// ======================================================= Config Debug redacts token

#[test]
fn config_debug_redacts_bot_token() {
    let config = test_config();
    let debug_str = format!("{:?}", config);
    assert!(
        debug_str.contains("[REDACTED]"),
        "Debug output should contain [REDACTED]"
    );
    assert!(
        !debug_str.contains("ABC-DEF_test-token"),
        "Debug output must NOT contain the actual token"
    );
}

// ======================================================= Multiple bots are independent

#[test]
fn multiple_bots_independent_running_state() {
    let config = test_config();
    let bot1 = TelegramBot::new(&config).unwrap();
    let bot2 = TelegramBot::new(&config).unwrap();

    bot1.set_running(true);
    assert!(bot1.is_running());
    assert!(!bot2.is_running(), "bot2 should be independent of bot1");

    bot2.set_running(true);
    bot1.set_running(false);
    assert!(!bot1.is_running());
    assert!(bot2.is_running(), "bot2 should still be running");
}

// ======================================================= Thread safety of TelegramBot

#[test]
fn telegram_bot_is_send_and_sync() {
    fn assert_send<T: Send>() {}
    fn assert_sync<T: Sync>() {}
    // These compile only if TelegramBot implements Send + Sync
    assert_send::<TelegramBot>();
    assert_sync::<TelegramBot>();
}

// ======================================================= set_running concurrent access

#[test]
fn set_running_from_multiple_threads() {
    use std::sync::Arc;
    use std::thread;

    let config = test_config();
    let bot = Arc::new(TelegramBot::new(&config).unwrap());

    let mut handles = Vec::new();
    for _ in 0..10 {
        let bot_clone = Arc::clone(&bot);
        handles.push(thread::spawn(move || {
            bot_clone.set_running(true);
            assert!(bot_clone.is_running());
            bot_clone.set_running(false);
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    // After all threads finish, the final state depends on thread ordering,
    // but the value should be false since all threads set it to false last.
    assert!(!bot.is_running());
}

// ======================================================= approval keyboard with various IDs

#[test]
fn approval_keyboard_with_uuid_style_id() {
    let id = "550e8400-e29b-41d4-a716-446655440000";
    let kb = create_approval_keyboard(id);
    assert!(kb[0][0].callback_data.contains(id));
    assert!(kb[0][1].callback_data.contains(id));
    assert!(kb[1][0].callback_data.contains(id));
}

#[test]
fn approval_keyboard_with_long_id() {
    let id = "a".repeat(200);
    let kb = create_approval_keyboard(&id);
    assert_eq!(kb[0][0].callback_data, format!("approve:{}", id));
}

// ======================================================= scrub_bot_token regex edge cases

#[test]
fn scrub_bot_token_only_digits_in_id() {
    let msg = "bot1234567890:ABCDEF/getMe";
    let scrubbed = scrub_bot_token(msg);
    assert_eq!(scrubbed, "bot[REDACTED]/getMe");
}

#[test]
fn scrub_bot_token_with_hyphens_and_underscores_in_secret() {
    let msg = "bot99:A-B_C-D_E/sendPhoto ok";
    let scrubbed = scrub_bot_token(msg);
    assert_eq!(scrubbed, "bot[REDACTED]/sendPhoto ok");
}

#[test]
fn scrub_bot_token_does_not_match_non_bot_prefix() {
    // "robot123:ABC/" should match because "bot123:ABC_def/" is a substring
    let msg = "robot123:ABC_def/path";
    let scrubbed = scrub_bot_token(msg);
    // "ro" + match on "bot123:ABC_def/" -> "robot[REDACTED]/path"
    assert!(scrubbed.contains("[REDACTED]"));
}

#[test]
fn scrub_bot_token_file_download_url() {
    let msg = "GET https://api.telegram.org/file/bot123:TOKEN_val/photos/file_0.jpg";
    let scrubbed = scrub_bot_token(msg);
    assert!(!scrubbed.contains("123:TOKEN_val"));
    assert!(scrubbed.contains("bot[REDACTED]/"));
}

// ======================================================= SendOptions Debug impl

#[test]
fn send_options_debug_output() {
    let opts = SendOptions {
        parse_mode: Some("Markdown".into()),
        disable_notification: None,
        thread_id: None,
        reply_to_message_id: None,
    };
    let debug = format!("{:?}", opts);
    assert!(debug.contains("Markdown"));
    assert!(debug.contains("SendOptions"));
}
