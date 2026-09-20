//! ADR-017: `ctm update` — self-update from GitHub Releases.
//!
//! Model (borrowed from hf2q ADR-045, adapted for GitHub-only hosting):
//! 1. Fetch the per-target **release record** `stable-<triple>.json` through GitHub's
//!    `releases/latest/download/<name>` redirect (spike-verified: 302 to the newest
//!    release's asset; no API, no auth, no rate limit).
//! 2. Compare its SemVer against `CARGO_PKG_VERSION`.
//! 3. Stream `ctm-<triple>` from the SAME release tag into `<dir>/.ctm-candidate.partial`,
//!    refusing any redirect off GitHub, bounding bytes by the record's `size`, and
//!    verifying sha256 before anything is renamed.
//! 4. Publish atomically: active → `.ctm-previous`, `rename(candidate → ctm)`, fsync,
//!    re-digest the active file. `--rollback` swaps `.previous` back.
//! 5. Restart the service if one is installed (the unit and the hook registrations
//!    store an absolute path, so a swap at a stable path needs no re-registration —
//!    spike-verified).
//!
//! Channels: only a *standalone* install (marker file beside the binary, written by
//! `install.sh` or by us) is updated in place. An *npm* install is **migrated** to
//! standalone; a *source* build is refused with guidance.

use crate::error::{AppError, Result};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const REPO: &str = "robertelee78/claude-telegram-mirror";
const RECORD_KIND: &str = "ctm.standalone-release";
const RECORD_SCHEMA: u32 = 1;
const MAX_RECORD_BYTES: usize = 4 * 1024;
const MARKER_NAME: &str = ".ctm-channel";
const ACTIVE_NAME: &str = "ctm";
const PREVIOUS_NAME: &str = ".ctm-previous";
const CANDIDATE_PARTIAL: &str = ".ctm-candidate.partial";
const ROLLBACK_PARTIAL: &str = ".ctm-rollback.partial";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const RECORD_TIMEOUT: Duration = Duration::from_secs(30);
const ASSET_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Rust target triple of the running binary, matching the release asset names.
pub fn target_triple() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        _ => "unsupported",
    }
}

pub fn default_install_dir() -> PathBuf {
    crate::config::home_dir().join(".local").join("bin")
}

/// How the running binary got here. Decides what `update` is allowed to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Channel {
    /// Installed by `install.sh` / `ctm update` — marker present beside the binary.
    Standalone { install_dir: PathBuf },
    /// The retired npm distribution (`node_modules/@agidreams/ctm-*/bin/ctm`).
    Npm { exe: PathBuf },
    /// A `cargo build` output — never overwritten.
    Source { exe: PathBuf },
    /// Anything else (copied by hand, packaged by a distro, …).
    Unmanaged { exe: PathBuf },
}

pub fn detect_channel(exe: &Path) -> Channel {
    let exe = fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
    let s = exe.to_string_lossy();
    if let Some(dir) = exe.parent() {
        if dir.join(MARKER_NAME).is_file() {
            return Channel::Standalone {
                install_dir: dir.to_path_buf(),
            };
        }
    }
    if s.contains("/node_modules/@agidreams/")
        || s.contains("/node_modules/claude-telegram-mirror/")
    {
        return Channel::Npm { exe };
    }
    if s.contains("/target/debug/")
        || s.contains("/target/release/")
        || s.contains("/target/") && s.contains("/deps/")
    {
        return Channel::Source { exe };
    }
    Channel::Unmanaged { exe }
}

/// `stable-<triple>.json` as published by the release workflow. Strict: unknown
/// fields are rejected so a differently-shaped file can never be mistaken for ours.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseRecord {
    pub kind: String,
    pub schema_version: u32,
    pub package: String,
    pub channel: String,
    pub target: String,
    pub version: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expectation {
    pub size: u64,
    pub sha256: [u8; 32],
}

pub fn parse_record(bytes: &[u8], target: &str) -> Result<(Version, ReleaseRecord, Expectation)> {
    if bytes.is_empty() || bytes.len() > MAX_RECORD_BYTES {
        return Err(AppError::Config("release record size out of bounds".into()));
    }
    let r: ReleaseRecord = serde_json::from_slice(bytes)
        .map_err(|e| AppError::Config(format!("release record is not valid: {e}")))?;
    if r.kind != RECORD_KIND
        || r.schema_version != RECORD_SCHEMA
        || r.package != "ctm"
        || r.channel != "stable"
    {
        return Err(AppError::Config(
            "release record identity does not match ctm/stable".into(),
        ));
    }
    if r.target != target {
        return Err(AppError::Config(format!(
            "release record is for {}, this binary is {target}",
            r.target
        )));
    }
    let version = Version::parse(&r.version)
        .map_err(|_| AppError::Config(format!("release version is not SemVer: {}", r.version)))?;
    if !version.pre.is_empty() || !version.build.is_empty() {
        return Err(AppError::Config(
            "release version must be a stable SemVer".into(),
        ));
    }
    let sha = hex32(&r.sha256)?;
    Ok((
        version,
        r.clone(),
        Expectation {
            size: r.size,
            sha256: sha,
        },
    ))
}

fn hex32(s: &str) -> Result<[u8; 32]> {
    if s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(AppError::Config("sha256 must be 64 hex characters".into()));
    }
    let mut out = [0u8; 32];
    for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
        out[i] = u8::from_str_radix(std::str::from_utf8(chunk).unwrap(), 16)
            .map_err(|_| AppError::Config("sha256 hex decode failed".into()))?;
    }
    Ok(out)
}

fn sha256_file(path: &Path) -> Result<([u8; 32], u64)> {
    let mut f = fs::File::open(path)?;
    let mut h = Sha256::new();
    let n = std::io::copy(&mut f, &mut h)?;
    Ok((h.finalize().into(), n))
}

// ---------------------------------------------------------------------------- network

fn record_url(target: &str) -> String {
    // The `latest/download` path is a redirect whose target moves with each release, so
    // a cache that keeps it serves an old version forever. GitHub marks the redirect
    // `no-cache`, but an intermediary that ignores that header has been seen to answer
    // with the previous release ("ctm 0.2.39 is current" the same minute 0.2.40 was
    // published, on a machine whose network differed only in its proxy). A unique query
    // string makes the URL uncacheable by construction; GitHub ignores the parameter.
    format!(
        "https://github.com/{REPO}/releases/latest/download/stable-{target}.json?ts={}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    )
}

fn asset_url(version: &str, target: &str) -> String {
    format!("https://github.com/{REPO}/releases/download/v{version}/ctm-{target}")
}

fn allowed_origin(url: &reqwest::Url) -> bool {
    url.scheme() == "https"
        && matches!(
            url.host_str(),
            Some("github.com")
                | Some("release-assets.githubusercontent.com")
                | Some("objects.githubusercontent.com")
        )
}

async fn fetch_record(target: &str) -> Result<(Version, ReleaseRecord, Expectation)> {
    let client = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(RECORD_TIMEOUT)
        .build()?;
    let resp = client
        .get(record_url(target))
        .header("Accept-Encoding", "identity")
        .header("Cache-Control", "no-cache, no-store, max-age=0")
        .header("Pragma", "no-cache")
        .send()
        .await?;
    if !allowed_origin(resp.url()) {
        return Err(AppError::Config(
            "release record left GitHub origins".into(),
        ));
    }
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(AppError::Config(format!(
            "no release record for {target} (asset stable-{target}.json missing on the latest release)"
        )));
    }
    if !resp.status().is_success() {
        return Err(AppError::Config(format!(
            "release record fetch: HTTP {}",
            resp.status()
        )));
    }
    let bytes = resp.bytes().await?;
    parse_record(&bytes, target)
}

/// Stream the asset into `dest`, bounded and digest-verified. Nothing else touches
/// the install directory until this returns Ok.
async fn download_asset(version: &str, target: &str, exp: &Expectation, dest: &Path) -> Result<()> {
    let client = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(ASSET_TIMEOUT)
        .build()?;
    let mut resp = client
        .get(asset_url(version, target))
        .header("Accept-Encoding", "identity")
        .send()
        .await?;
    if !allowed_origin(resp.url()) {
        return Err(AppError::Config("release asset left GitHub origins".into()));
    }
    if !resp.status().is_success() {
        return Err(AppError::Config(format!(
            "release asset download: HTTP {}",
            resp.status()
        )));
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o755)
        .open(dest)?;
    let mut hasher = Sha256::new();
    let mut total: u64 = 0;
    while let Some(chunk) = resp.chunk().await? {
        total += chunk.len() as u64;
        if total > exp.size {
            let _ = fs::remove_file(dest);
            return Err(AppError::Config(
                "release asset larger than the record says".into(),
            ));
        }
        hasher.update(&chunk);
        file.write_all(&chunk)?;
    }
    file.flush()?;
    file.sync_all()?;
    let digest: [u8; 32] = hasher.finalize().into();
    if total != exp.size || digest != exp.sha256 {
        let _ = fs::remove_file(dest);
        return Err(AppError::Config(
            "release asset failed size/sha256 verification".into(),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------- publish

fn write_marker(dir: &Path) -> Result<()> {
    let mut f = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o644)
        .open(dir.join(MARKER_NAME))?;
    f.write_all(b"standalone\n")?;
    f.sync_all()?;
    Ok(())
}

fn sync_dir(dir: &Path) -> Result<()> {
    fs::File::open(dir)?.sync_all()?;
    Ok(())
}

/// Atomically make a verified candidate the active binary. Keeps exactly one
/// `.ctm-previous`. Refuses to replace an active binary this channel does not own.
pub fn publish_candidate(dir: &Path, candidate: &Path, exp: &Expectation) -> Result<()> {
    let active = dir.join(ACTIVE_NAME);
    let marker = dir.join(MARKER_NAME);
    if active.exists() && !marker.is_file() {
        return Err(AppError::Config(format!(
            "{} exists but is not a standalone install (no {MARKER_NAME}); refusing to overwrite",
            active.display()
        )));
    }
    let (digest, size) = sha256_file(candidate)?;
    if size != exp.size || digest != exp.sha256 {
        return Err(AppError::Config(
            "candidate does not match the release record".into(),
        ));
    }
    fs::set_permissions(candidate, fs::Permissions::from_mode(0o755))?;
    if !marker.is_file() {
        write_marker(dir)?;
    }
    if active.exists() {
        // Copy (not rename) so a crash between here and the final rename still leaves a
        // runnable `ctm`; the previous copy is complete before the swap happens.
        fs::copy(&active, dir.join(PREVIOUS_NAME))?;
    }
    fs::rename(candidate, &active)?;
    sync_dir(dir)?;
    let (d2, s2) = sha256_file(&active)?;
    if s2 != exp.size || d2 != exp.sha256 {
        return Err(AppError::Config(
            "active binary failed re-verification after publish".into(),
        ));
    }
    Ok(())
}

pub fn rollback(dir: &Path) -> Result<String> {
    let active = dir.join(ACTIVE_NAME);
    let previous = dir.join(PREVIOUS_NAME);
    if !dir.join(MARKER_NAME).is_file() {
        return Err(AppError::Config(
            "not a standalone install; nothing to roll back".into(),
        ));
    }
    if !previous.is_file() {
        return Err(AppError::Config("no previous binary retained".into()));
    }
    let partial = dir.join(ROLLBACK_PARTIAL);
    fs::copy(&active, &partial)?;
    fs::rename(&previous, &active)?;
    fs::rename(&partial, &previous)?;
    sync_dir(dir)?;
    Ok(binary_version(&active).unwrap_or_else(|| "unknown".into()))
}

fn binary_version(path: &Path) -> Option<String> {
    let out = Command::new(path).arg("--version").output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    s.split_whitespace().nth(1).map(str::to_string)
}

// ---------------------------------------------------------------------------- command

/// `ctm update [--check] [--rollback]`.
pub async fn run_update(check_only: bool, do_rollback: bool) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let channel = detect_channel(&exe);
    let target = target_triple();
    if target == "unsupported" {
        anyhow::bail!(
            "no release is built for {}/{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        );
    }

    if do_rollback {
        let Channel::Standalone { install_dir } = &channel else {
            anyhow::bail!("--rollback only applies to a standalone install (channel: {channel:?})");
        };
        let v = rollback(install_dir)?;
        println!("rolled back to ctm {v}");
        restart_service_if_installed(&install_dir.join(ACTIVE_NAME));
        return Ok(());
    }

    let current = Version::parse(env!("CARGO_PKG_VERSION"))?;
    let (latest, record, exp) = fetch_record(target).await?;

    match &channel {
        Channel::Source { exe } => {
            println!(
                "this is a source build ({}); update by rebuilding, or install a release:",
                exe.display()
            );
            println!(
                "  curl -fsSL https://raw.githubusercontent.com/{REPO}/master/install.sh | sh"
            );
            println!("latest release: {latest}  (this build: {current})");
            return Ok(());
        }
        Channel::Unmanaged { exe } => {
            println!(
                "{} was not installed by ctm's installer; not touching it.",
                exe.display()
            );
            println!("latest release: {latest}  (running: {current})");
            println!("install the managed channel with:");
            println!(
                "  curl -fsSL https://raw.githubusercontent.com/{REPO}/master/install.sh | sh"
            );
            return Ok(());
        }
        _ => {}
    }

    // Standalone: update in place. npm: migrate (always, even if versions match —
    // the point is to leave the retired channel).
    let (install_dir, migrating_from) = match &channel {
        Channel::Standalone { install_dir } => (install_dir.clone(), None),
        Channel::Npm { exe } => (default_install_dir(), Some(exe.clone())),
        _ => unreachable!(),
    };

    if migrating_from.is_none() && latest <= current {
        // Name what was compared against: "is current" used to be indistinguishable
        // from "the lookup returned something stale".
        println!("ctm {current} is current (newest published release: {latest})");
        return Ok(());
    }
    if check_only {
        if let Some(from) = &migrating_from {
            println!("running the retired npm install at {}", from.display());
            println!(
                "`ctm update` will install {latest} to {} and re-point the service and hooks",
                install_dir.display()
            );
        } else {
            println!("update available: {current} -> {latest}");
        }
        return Ok(());
    }

    fs::create_dir_all(&install_dir)?;
    let candidate = install_dir.join(CANDIDATE_PARTIAL);
    println!("downloading ctm {} for {target} …", record.version);
    download_asset(&record.version, target, &exp, &candidate).await?;
    publish_candidate(&install_dir, &candidate, &exp)?;
    let new_bin = install_dir.join(ACTIVE_NAME);
    let installed = binary_version(&new_bin).unwrap_or_else(|| record.version.clone());
    println!("installed ctm {installed} at {}", new_bin.display());

    if let Some(from) = migrating_from {
        // Re-register with the NEW binary so its path (not this npm one) is recorded.
        // The new binary computes its own current_exe(); we must not do it in-process.
        run_new(&new_bin, &["install-hooks"]);
        if crate::service::is_service_installed() {
            run_new(&new_bin, &["service", "install"]);
            run_new(&new_bin, &["service", "restart"]);
        }
        run_new(&new_bin, &["shell-setup"]);
        println!();
        println!("migrated from the retired npm distribution:");
        println!("  old: {}", from.display());
        println!("  new: {}", new_bin.display());
        println!(
            "finish by removing the npm package and making sure {} is on your PATH:",
            install_dir.display()
        );
        println!("  npm uninstall -g claude-telegram-mirror");
        return Ok(());
    }

    restart_service_if_installed(&new_bin);
    // Completions may have gained subcommands; the rc block is idempotent.
    run_new(&new_bin, &["shell-setup"]);
    println!("updated ctm {current} -> {installed}");
    println!("undo with: ctm update --rollback");
    Ok(())
}

fn run_new(bin: &Path, args: &[&str]) {
    match Command::new(bin).args(args).status() {
        Ok(s) if s.success() => {}
        Ok(s) => eprintln!("warning: `{} {}` exited {s}", bin.display(), args.join(" ")),
        Err(e) => eprintln!(
            "warning: could not run `{} {}`: {e}",
            bin.display(),
            args.join(" ")
        ),
    }
}

fn restart_service_if_installed(bin: &Path) {
    if crate::service::is_service_installed() {
        println!("restarting service so the daemon runs the new binary …");
        run_new(bin, &["service", "restart"]);
    } else {
        println!(
            "no service installed — restart any running `ctm start` daemon to use the new binary"
        );
    }
}

/// For `doctor`: the latest published version for this target, if reachable.
pub async fn latest_version() -> Option<Version> {
    fetch_record(target_triple()).await.ok().map(|(v, _, _)| v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record_json(target: &str, version: &str, size: u64, sha: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "kind": RECORD_KIND, "schema_version": RECORD_SCHEMA, "package": "ctm",
            "channel": "stable", "target": target, "version": version, "size": size, "sha256": sha
        }))
        .unwrap()
    }
    const SHA: &str = "0000000000000000000000000000000000000000000000000000000000000000";

    #[test]
    fn record_parses_and_rejects_identity_and_target_mismatches() {
        let t = "aarch64-apple-darwin";
        let (v, r, e) = parse_record(&record_json(t, "0.2.29", 10, SHA), t).unwrap();
        assert_eq!(v, Version::new(0, 2, 29));
        assert_eq!(r.version, "0.2.29");
        assert_eq!(e.size, 10);
        assert!(
            parse_record(&record_json("x86_64-apple-darwin", "0.2.29", 10, SHA), t).is_err(),
            "wrong target"
        );
        assert!(
            parse_record(&record_json(t, "0.2.29-rc1", 10, SHA), t).is_err(),
            "pre-release refused"
        );
        assert!(
            parse_record(&record_json(t, "0.2.29", 10, "abc"), t).is_err(),
            "bad sha"
        );
        let mut extra: serde_json::Value =
            serde_json::from_slice(&record_json(t, "0.2.29", 10, SHA)).unwrap();
        extra["surprise"] = serde_json::json!(1);
        assert!(
            parse_record(&serde_json::to_vec(&extra).unwrap(), t).is_err(),
            "unknown field refused"
        );
        assert!(
            parse_record(&vec![b'x'; MAX_RECORD_BYTES + 1], t).is_err(),
            "oversized refused"
        );
    }

    #[test]
    fn channel_detection() {
        let d = tempfile::tempdir().unwrap();
        let bin = d.path().join("ctm");
        fs::write(&bin, b"x").unwrap();
        assert!(matches!(detect_channel(&bin), Channel::Unmanaged { .. }));
        fs::write(d.path().join(MARKER_NAME), b"standalone\n").unwrap();
        assert!(matches!(detect_channel(&bin), Channel::Standalone { .. }));
        let npm = PathBuf::from("/x/node_modules/@agidreams/ctm-darwin-arm64/bin/ctm");
        assert!(matches!(detect_channel(&npm), Channel::Npm { .. }));
        let src = PathBuf::from("/x/rust-crates/target/release/ctm");
        assert!(matches!(detect_channel(&src), Channel::Source { .. }));
    }

    #[test]
    fn publish_swaps_atomically_keeps_previous_and_rolls_back() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path();
        // "v1" active, owned by the channel
        fs::write(dir.join(ACTIVE_NAME), b"#!/bin/sh\necho ctm 1.0.0\n").unwrap();
        fs::set_permissions(dir.join(ACTIVE_NAME), fs::Permissions::from_mode(0o755)).unwrap();
        write_marker(dir).unwrap();
        // candidate "v2"
        let v2 = b"#!/bin/sh\necho ctm 2.0.0\n";
        let cand = dir.join(CANDIDATE_PARTIAL);
        fs::write(&cand, v2).unwrap();
        let exp = Expectation {
            size: v2.len() as u64,
            sha256: Sha256::digest(v2).into(),
        };
        publish_candidate(dir, &cand, &exp).unwrap();
        assert!(!cand.exists(), "candidate consumed");
        assert_eq!(fs::read(dir.join(ACTIVE_NAME)).unwrap(), v2);
        assert_eq!(
            fs::read(dir.join(PREVIOUS_NAME)).unwrap(),
            b"#!/bin/sh\necho ctm 1.0.0\n"
        );
        assert_eq!(
            binary_version(&dir.join(ACTIVE_NAME)).as_deref(),
            Some("2.0.0")
        );

        // rollback restores v1 and retains v2 as previous
        let v = rollback(dir).unwrap();
        assert_eq!(v, "1.0.0");
        assert_eq!(fs::read(dir.join(PREVIOUS_NAME)).unwrap(), v2);
    }

    #[test]
    fn publish_refuses_wrong_digest_and_unowned_active() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path();
        let cand = dir.join(CANDIDATE_PARTIAL);
        fs::write(&cand, b"new").unwrap();
        let bad = Expectation {
            size: 3,
            sha256: [9u8; 32],
        };
        assert!(
            publish_candidate(dir, &cand, &bad).is_err(),
            "digest mismatch refused"
        );
        assert!(cand.exists(), "candidate untouched on refusal");
        // an active binary with no marker is someone else's — never overwrite
        fs::write(dir.join(ACTIVE_NAME), b"theirs").unwrap();
        let good = Expectation {
            size: 3,
            sha256: Sha256::digest(b"new").into(),
        };
        assert!(
            publish_candidate(dir, &cand, &good).is_err(),
            "unowned active refused"
        );
        assert_eq!(fs::read(dir.join(ACTIVE_NAME)).unwrap(), b"theirs");
    }

    #[test]
    fn the_record_url_is_uncacheable() {
        // A cached `latest/download` redirect serves the previous release forever,
        // which is how one machine reported "is current" minutes after a new release.
        let a = record_url("x86_64-unknown-linux-gnu");
        assert!(a.contains("?ts="), "{a}");
        let ts: u64 = a.split("?ts=").nth(1).unwrap().parse().unwrap();
        assert!(ts > 1_700_000_000, "a real epoch timestamp: {ts}");
    }

    #[test]
    fn urls_and_origins() {
        assert_eq!(
            record_url("aarch64-apple-darwin")
                .split_once("?ts=")
                .map(|(base, _)| base)
                .expect("the record URL carries a cache-busting timestamp"),
            "https://github.com/robertelee78/claude-telegram-mirror/releases/latest/download/stable-aarch64-apple-darwin.json"
        );
        assert_eq!(
            asset_url("0.2.29", "x86_64-unknown-linux-gnu"),
            "https://github.com/robertelee78/claude-telegram-mirror/releases/download/v0.2.29/ctm-x86_64-unknown-linux-gnu"
        );
        assert!(allowed_origin(
            &reqwest::Url::parse("https://release-assets.githubusercontent.com/x").unwrap()
        ));
        assert!(
            !allowed_origin(&reqwest::Url::parse("http://github.com/x").unwrap()),
            "https only"
        );
        assert!(!allowed_origin(
            &reqwest::Url::parse("https://evil.example/x").unwrap()
        ));
    }
}
