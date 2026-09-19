//! ADR-016 §Default enablement: find host installations from a daemon that runs under
//! launchd/systemd with a PATH that is not the operator's interactive one.
//!
//! Every lookup here is a plain filesystem probe. Nothing is executed and nothing is
//! cached, so a host installed after the daemon started is found on the next probe.

use std::path::{Path, PathBuf};

fn home() -> PathBuf {
    crate::config::home_dir()
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Directories user-installed CLIs commonly live in, probed after `$PATH`. Stable
/// locations only — no per-shell shim dirs (fnm multishells die with their shell).
fn well_known_bin_dirs() -> Vec<PathBuf> {
    let h = home();
    let mut dirs = vec![
        h.join(".local/bin"),
        h.join(".opencode/bin"),
        h.join(".codex/packages/standalone/current/bin"),
        h.join(".cargo/bin"),
        h.join(".bun/bin"),
        h.join(".volta/bin"),
        h.join(".npm-global/bin"),
        h.join(".local/share/fnm/aliases/default/bin"),
        h.join("Library/Application Support/fnm/aliases/default/bin"),
        h.join(".local/share/pnpm"),
        h.join("Library/pnpm"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/bin"),
    ];
    // nvm keeps one bin dir per version; newest first.
    if let Ok(rd) = std::fs::read_dir(h.join(".nvm/versions/node")) {
        let mut versions: Vec<PathBuf> = rd.flatten().map(|e| e.path().join("bin")).collect();
        versions.sort();
        versions.reverse();
        dirs.extend(versions);
    }
    dirs
}

/// `which`-equivalent over `$PATH` (unset PATH is treated as empty).
pub fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| is_executable(p))
}

/// `$PATH` first, then the well-known dirs.
pub fn find_binary(name: &str) -> Option<PathBuf> {
    find_on_path(name).or_else(|| {
        well_known_bin_dirs()
            .into_iter()
            .map(|d| d.join(name))
            .find(|p| is_executable(p))
    })
}

/// The Codex CLI as a **native** executable, never the npm `codex.js` shim.
///
/// Order: explicit override → Codex's own managed standalone copy (created by
/// `codex app-server daemon start`, self-updating, the binary that daemon actually
/// runs) → the native binary bundled inside the npm package → any other `codex`.
/// The shim is rejected because it needs `node` on PATH, which a service does not
/// reliably have (spike: launchd PATH held only an ephemeral fnm multishell dir).
pub fn codex_binary(override_path: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = override_path {
        return is_executable(p).then(|| p.to_path_buf());
    }
    let managed = home().join(".codex/packages/standalone/current/bin/codex");
    if is_executable(&managed) {
        return Some(managed);
    }
    let found = find_binary("codex")?;
    let canonical = std::fs::canonicalize(&found).unwrap_or(found);
    if canonical.extension().is_some_and(|e| e == "js") {
        return npm_bundled_native(&canonical);
    }
    Some(canonical)
}

/// `<pkg>/bin/codex.js` → `<pkg>/node_modules/@openai/codex-<os>-<arch>/vendor/<triple>/bin/codex`.
fn npm_bundled_native(shim: &Path) -> Option<PathBuf> {
    let pkg = shim.parent()?.parent()?;
    let scope = std::fs::read_dir(pkg.join("node_modules/@openai")).ok()?;
    for platform_pkg in scope.flatten() {
        if !platform_pkg
            .file_name()
            .to_string_lossy()
            .starts_with("codex-")
        {
            continue;
        }
        let Ok(vendor) = std::fs::read_dir(platform_pkg.path().join("vendor")) else {
            continue;
        };
        for triple in vendor.flatten() {
            let candidate = triple.path().join("bin/codex");
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// Codex is installed on this machine (binary found, or its state dir exists).
pub fn codex_present(override_path: Option<&Path>) -> bool {
    codex_binary(override_path).is_some() || home().join(".codex").is_dir()
}

/// OpenCode's global config dir, where it loads `plugins/*.js` from. Honours
/// `XDG_CONFIG_HOME` exactly as OpenCode does (spike-verified: a bare `opencode` with
/// `XDG_CONFIG_HOME=<dir>` loaded `<dir>/opencode/plugins/ctm.js`).
pub fn opencode_config_dir() -> PathBuf {
    match std::env::var_os("XDG_CONFIG_HOME") {
        Some(x) if !x.is_empty() => PathBuf::from(x).join("opencode"),
        _ => home().join(".config/opencode"),
    }
}

/// The OpenCode binary, if any (only used for reporting — the pipe plugin runs inside
/// OpenCode, so ctm never executes it).
pub fn opencode_binary() -> Option<PathBuf> {
    find_binary("opencode")
}

/// OpenCode is installed on this machine (binary found, or one of its dirs exists).
pub fn opencode_present() -> bool {
    opencode_binary().is_some()
        || opencode_config_dir().is_dir()
        || home().join(".local/share/opencode").is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn exe(p: &Path) {
        std::fs::write(p, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn find_on_path_requires_executable_bit() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notexec"), "x").unwrap();
        exe(&dir.path().join("isexec"));
        let saved = std::env::var_os("PATH");
        std::env::set_var("PATH", dir.path());
        assert_eq!(find_on_path("isexec"), Some(dir.path().join("isexec")));
        assert_eq!(find_on_path("notexec"), None);
        match saved {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }
    }

    #[test]
    fn npm_shim_resolves_to_bundled_native_binary() {
        let root = tempfile::tempdir().unwrap();
        let pkg = root.path().join("lib/node_modules/@openai/codex");
        let shim = pkg.join("bin/codex.js");
        std::fs::create_dir_all(shim.parent().unwrap()).unwrap();
        std::fs::write(&shim, "#!/usr/bin/env node\n").unwrap();
        let native = pkg
            .join("node_modules/@openai/codex-darwin-arm64/vendor/aarch64-apple-darwin/bin/codex");
        std::fs::create_dir_all(native.parent().unwrap()).unwrap();
        exe(&native);
        assert_eq!(npm_bundled_native(&shim), Some(native));
    }

    #[test]
    fn npm_shim_without_platform_package_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let shim = root
            .path()
            .join("lib/node_modules/@openai/codex/bin/codex.js");
        std::fs::create_dir_all(shim.parent().unwrap()).unwrap();
        std::fs::write(&shim, "#!/usr/bin/env node\n").unwrap();
        assert_eq!(npm_bundled_native(&shim), None);
    }

    #[test]
    fn override_must_be_executable() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("codex");
        std::fs::write(&p, "x").unwrap();
        assert_eq!(codex_binary(Some(&p)), None);
        exe(&p);
        assert_eq!(codex_binary(Some(&p)), Some(p));
    }
}
