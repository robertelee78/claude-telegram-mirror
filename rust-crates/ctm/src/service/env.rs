//! Environment file parsing and generation.

use super::*;

/// Parse an environment file into key-value pairs.
///
/// Handles: `export KEY=value`, `KEY="value"`, `KEY='value'`, inline `# comments`,
/// blank lines, and comment-only lines.
pub fn parse_env_file(path: &Path) -> HashMap<String, String> {
    let mut vars = HashMap::new();
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return vars,
    };

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Remove `export ` prefix
        let clean = if let Some(rest) = trimmed.strip_prefix("export ") {
            rest.trim()
        } else if let Some(rest) = trimmed.strip_prefix("export\t") {
            rest.trim()
        } else {
            trimmed
        };

        // Find `=`
        let eq_index = match clean.find('=') {
            Some(i) => i,
            None => continue,
        };

        let key = clean[..eq_index].trim();
        if key.is_empty() {
            continue;
        }

        let mut value = clean[eq_index + 1..].trim().to_string();

        // Remove inline comments (but not those inside quotes)
        value = strip_inline_comment(&value);

        // Strip surrounding quotes
        if ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
            && value.len() >= 2
        {
            value = value[1..value.len() - 1].to_string();
        }

        vars.insert(key.to_string(), value);
    }

    vars
}

/// Remove an inline `# comment` that is NOT inside quotes.
pub(super) fn strip_inline_comment(value: &str) -> String {
    let mut in_single = false;
    let mut in_double = false;
    for (i, ch) in value.char_indices() {
        match ch {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '#' if !in_single && !in_double => {
                return value[..i].trim_end().to_string();
            }
            _ => {}
        }
    }
    value.to_string()
}

/// Create a systemd-compatible env file from ~/.telegram-env.
pub(super) fn create_systemd_env_file() -> anyhow::Result<PathBuf> {
    let env_vars = parse_env_file(&env_file_path());
    let config_dir = home_dir().join(".config").join(SERVICE_NAME);
    config::ensure_config_dir(&config_dir)?;

    let mut lines = vec!["# Auto-generated from ~/.telegram-env for systemd".to_string()];
    for (key, value) in &env_vars {
        if value.contains(' ')
            || value.contains('$')
            || value.contains('`')
            || value.contains('"')
            || value.contains('\\')
        {
            // Escape backslashes first, then double quotes
            let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
            lines.push(format!("{key}=\"{escaped}\""));
        } else {
            lines.push(format!("{key}={value}"));
        }
    }

    let dest = systemd_env_file_path();
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&dest, lines.join("\n") + "\n")?;
    fs::set_permissions(&dest, fs::Permissions::from_mode(0o600))?;

    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_parse_env_file_basic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("env");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "# Comment line").unwrap();
        writeln!(f, "export TOKEN=abc123").unwrap();
        writeln!(f, "CHAT_ID=\"-100999\"").unwrap();
        writeln!(f, "SINGLE='hello world'").unwrap();
        writeln!(f, "INLINE=value # comment").unwrap();
        writeln!(f, "").unwrap();
        writeln!(f, "NOVAL=").unwrap();
        drop(f);

        let vars = parse_env_file(&path);
        assert_eq!(vars.get("TOKEN").unwrap(), "abc123");
        assert_eq!(vars.get("CHAT_ID").unwrap(), "-100999");
        assert_eq!(vars.get("SINGLE").unwrap(), "hello world");
        assert_eq!(vars.get("INLINE").unwrap(), "value");
        assert_eq!(vars.get("NOVAL").unwrap(), "");
    }

    #[test]
    fn test_parse_env_file_missing() {
        let vars = parse_env_file(Path::new("/nonexistent/path/.env"));
        assert!(vars.is_empty());
    }

    #[test]
    fn test_parse_env_file_hash_in_quotes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("env");
        fs::write(&path, "KEY=\"value#with#hashes\"\n").unwrap();
        let vars = parse_env_file(&path);
        assert_eq!(vars.get("KEY").unwrap(), "value#with#hashes");
    }

    #[test]
    fn test_strip_inline_comment() {
        assert_eq!(strip_inline_comment("value # comment"), "value");
        assert_eq!(strip_inline_comment("\"val#ue\" # comment"), "\"val#ue\"");
        assert_eq!(strip_inline_comment("no_comment"), "no_comment");
    }

    /// L6.6: Verify parse_env_file handles `KEY="value # not a comment" # real comment`.
    #[test]
    fn test_parse_env_file_quoted_inline_comment() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("env");
        fs::write(
            &path,
            "KEY=\"value # not a comment\" # real comment\n\
             SINGLE='also # not a comment' # real\n",
        )
        .unwrap();
        let vars = parse_env_file(&path);
        assert_eq!(vars.get("KEY").unwrap(), "value # not a comment");
        assert_eq!(vars.get("SINGLE").unwrap(), "also # not a comment");
    }
}
