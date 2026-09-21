//! ADR-018: Apple Developer ID trust for darwin release binaries.
//!
//! The release pipeline signs every darwin asset with the team's Developer ID and
//! notarizes it; the record it publishes names the team, identifier and CDHash. This
//! module is the consumer side: `ctm update` refuses a candidate whose signature does
//! not match the constants pinned here **and** the record, and `ctm doctor` reports
//! what the running binary carries.
//!
//! The pin lives in code on purpose. The running binary is itself signed by this
//! team, so moving to another team requires shipping code through a release the
//! current team signed — a GitHub compromise alone cannot re-point it.
//!
//! Parsing is pure and tested against captured `codesign --display --verbose=4`
//! output; only the process spawns are macOS-only.

use crate::error::{AppError, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Apple Developer Team that signs ctm releases.
pub const TEAM_ID: &str = "3T2D2YNTVW";
/// Code-signing identifier the release pipeline stamps (`codesign --identifier`).
pub const IDENTIFIER: &str = "us.ctm.cli";

const AUTHORITY_INTERMEDIATE: &str = "Developer ID Certification Authority";
const AUTHORITY_ROOT: &str = "Apple Root CA";
const MAX_SIGNING_INFO_BYTES: usize = 64 * 1024;

/// The `signing` object of a darwin `stable-<triple>.json` record.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecordSigning {
    pub team_id: String,
    pub identifier: String,
    pub cdhash: String,
}

/// What a Developer ID signature says about itself, after strict parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SigningIdentity {
    pub team_id: String,
    pub identifier: String,
    pub cdhash: String,
    /// The leaf authority, e.g. `Developer ID Application: NAME (TEAM)`.
    pub authority: String,
}

/// Coarse state for `doctor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SigningState {
    DeveloperId(SigningIdentity),
    AdHoc,
    Unsigned,
    /// Signed, but not by a complete Developer ID chain with hardened runtime.
    Other(String),
}

fn canonical_team(s: &str) -> bool {
    s.len() == 10
        && s.bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

fn canonical_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
}

fn canonical_cdhash(s: &str) -> bool {
    (40..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn unique_value<'a>(text: &'a str, prefix: &str) -> Result<&'a str> {
    let mut it = text.lines().filter_map(|l| l.strip_prefix(prefix));
    let first = it
        .next()
        .ok_or_else(|| AppError::Config(format!("Apple signature has no `{prefix}` line")))?;
    if it.next().is_some() {
        return Err(AppError::Config(format!(
            "Apple signature has more than one `{prefix}` line"
        )));
    }
    Ok(first)
}

/// Strict parse of `codesign --display --verbose=4` output for a Developer ID
/// signature: canonical team and identifier, the full three-link authority chain,
/// hardened runtime, a secure timestamp, and exactly one CDHash.
pub fn parse_signing_identity(text: &str) -> Result<SigningIdentity> {
    let team_id = unique_value(text, "TeamIdentifier=")?;
    if !canonical_team(team_id) {
        return Err(AppError::Config(
            "Apple Developer ID team is not canonical".into(),
        ));
    }
    let identifier = unique_value(text, "Identifier=")?;
    if !canonical_identifier(identifier) {
        return Err(AppError::Config(
            "Apple signing identifier is not canonical".into(),
        ));
    }
    let authorities: Vec<&str> = text
        .lines()
        .filter_map(|l| l.strip_prefix("Authority="))
        .collect();
    let prefix = "Developer ID Application: ";
    let suffix = format!(" ({team_id})");
    let chain_ok = authorities.len() == 3
        && authorities[0].starts_with(prefix)
        && authorities[0].ends_with(&suffix)
        && authorities[0].len() > prefix.len() + suffix.len()
        && authorities[1] == AUTHORITY_INTERMEDIATE
        && authorities[2] == AUTHORITY_ROOT;
    if !chain_ok {
        return Err(AppError::Config(
            "Apple Developer ID authority chain is incomplete or ambiguous".into(),
        ));
    }
    let code_directory: Vec<&str> = text
        .lines()
        .filter(|l| l.starts_with("CodeDirectory "))
        .collect();
    let runtime = code_directory.len() == 1
        && code_directory[0]
            .split_ascii_whitespace()
            .find_map(|f| f.strip_prefix("flags=0x"))
            .and_then(|f| f.split_once('('))
            .is_some_and(|(hex, names)| {
                !hex.is_empty()
                    && hex.bytes().all(|b| b.is_ascii_hexdigit())
                    && names
                        .trim_end_matches(')')
                        .split(',')
                        .any(|n| n == "runtime")
            });
    if !runtime {
        return Err(AppError::Config(
            "Apple signature does not enable the hardened runtime".into(),
        ));
    }
    if unique_value(text, "Timestamp=")?.is_empty() {
        return Err(AppError::Config(
            "Apple signature has no secure timestamp".into(),
        ));
    }
    let cdhash = unique_value(text, "CDHash=")?;
    if !canonical_cdhash(cdhash) {
        return Err(AppError::Config("Apple CDHash is not canonical".into()));
    }
    Ok(SigningIdentity {
        team_id: team_id.to_owned(),
        identifier: identifier.to_owned(),
        cdhash: cdhash.to_owned(),
        authority: authorities[0].to_owned(),
    })
}

/// Classify `codesign --display` output (or its failure text) for reporting.
pub fn classify(text: &str) -> SigningState {
    if text.contains("code object is not signed at all") {
        return SigningState::Unsigned;
    }
    if text.lines().any(|l| l == "Signature=adhoc") {
        return SigningState::AdHoc;
    }
    match parse_signing_identity(text) {
        Ok(id) => SigningState::DeveloperId(id),
        Err(e) => SigningState::Other(e.to_string()),
    }
}

/// The identity a release candidate must present, cross-checked three ways:
/// the constants in this binary, the record that named the asset, and the
/// signature on the bytes.
pub fn check_expected(identity: &SigningIdentity, record: &RecordSigning) -> Result<()> {
    if identity.team_id != TEAM_ID || identity.identifier != IDENTIFIER {
        return Err(AppError::Config(format!(
            "release is signed by {} as {}, expected {TEAM_ID} as {IDENTIFIER}",
            identity.team_id, identity.identifier
        )));
    }
    if record.team_id != TEAM_ID || record.identifier != IDENTIFIER {
        return Err(AppError::Config(format!(
            "release record names signer {} as {}, expected {TEAM_ID} as {IDENTIFIER}",
            record.team_id, record.identifier
        )));
    }
    if !canonical_cdhash(&record.cdhash) || record.cdhash != identity.cdhash {
        return Err(AppError::Config(
            "release record CDHash does not match the signed binary".into(),
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn codesign(args: &[&str], path: &Path) -> Result<std::process::Output> {
    let out = std::process::Command::new("/usr/bin/codesign")
        .args(args)
        .arg(path)
        .output()?;
    if out.stderr.len() > MAX_SIGNING_INFO_BYTES || out.stdout.len() > MAX_SIGNING_INFO_BYTES {
        return Err(AppError::Config("codesign output exceeded bounds".into()));
    }
    Ok(out)
}

/// `codesign --display --verbose=4` writes its report to stderr; a failure (e.g.
/// unsigned) also goes there, so the text is returned either way for `classify`.
#[cfg(target_os = "macos")]
pub fn read_signing_text(path: &Path) -> Result<String> {
    let out = codesign(&["--display", "--verbose=4"], path)?;
    Ok(String::from_utf8_lossy(&out.stderr).into_owned())
}

/// Everything `ctm update` requires of a downloaded darwin candidate before it is
/// allowed near the install directory. Order: static signature validity, identity
/// against the pins and the record, then Apple's online ticket lookup.
#[cfg(target_os = "macos")]
pub fn verify_release_candidate(path: &Path, record: &RecordSigning) -> Result<()> {
    let v = codesign(&["--verify", "--strict", "--all-architectures"], path)?;
    if !v.status.success() {
        return Err(AppError::Config(
            "release candidate failed Apple code-signature verification".into(),
        ));
    }
    let identity = parse_signing_identity(&read_signing_text(path)?)?;
    check_expected(&identity, record)?;
    let n = codesign(
        &[
            "--verify",
            "--strict",
            "--all-architectures",
            "--check-notarization",
            "--test-requirement",
            "=notarized",
        ],
        path,
    )?;
    if !n.status.success() {
        return Err(AppError::Config(
            "Apple did not confirm the release candidate's notarization ticket".into(),
        ));
    }
    Ok(())
}

/// For `doctor`: what the binary at `path` carries. `None` off macOS.
pub fn signing_state(path: &Path) -> Option<SigningState> {
    #[cfg(target_os = "macos")]
    {
        Some(match read_signing_text(path) {
            Ok(text) => classify(&text),
            Err(e) => SigningState::Other(e.to_string()),
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        None
    }
}

/// One line for humans.
pub fn describe(state: &SigningState) -> String {
    match state {
        SigningState::DeveloperId(id) => format!(
            "Developer ID {} as {}, hardened runtime, notarizable (cdhash {})",
            id.team_id,
            id.identifier,
            &id.cdhash[..12]
        ),
        SigningState::AdHoc => "ad-hoc signed (no Developer ID)".into(),
        SigningState::Unsigned => "not signed at all".into(),
        SigningState::Other(why) => format!("signed, but not as a ctm release: {why}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Captured from the 2026-09-21 spike (ADR-018): a release-shaped signature.
    const SIGNED: &str = "Executable=/tmp/ctm\n\
Identifier=us.ctm.cli\n\
Format=Mach-O thin (arm64)\n\
CodeDirectory v=20500 size=20822 flags=0x10000(runtime) hashes=645+2 location=embedded\n\
Hash type=sha256 size=32\n\
CDHash=51c099661a857daeb11a8b631adfe83b4df470b7\n\
Signature size=9043\n\
Authority=Developer ID Application: ROBERT E LEE (3T2D2YNTVW)\n\
Authority=Developer ID Certification Authority\n\
Authority=Apple Root CA\n\
Timestamp=Sep 21, 2026 at 8:56:57 AM\n\
Info.plist=not bound\n\
TeamIdentifier=3T2D2YNTVW\n\
Runtime Version=27.0.0\n";

    // Captured from the shipped 0.2.44 binary.
    const ADHOC: &str = "Executable=/Users/x/.local/bin/ctm\n\
Identifier=ctm-5555494401ef754c0a19368e99fb08bae2983549\n\
CodeDirectory v=20400 size=20069 flags=0x2(adhoc) hashes=621+2 location=embedded\n\
CDHash=f3b3477ece379b934b1ed8381b2c4547f1089906\n\
Signature=adhoc\n\
TeamIdentifier=not set\n";

    fn record() -> RecordSigning {
        RecordSigning {
            team_id: TEAM_ID.into(),
            identifier: IDENTIFIER.into(),
            cdhash: "51c099661a857daeb11a8b631adfe83b4df470b7".into(),
        }
    }

    #[test]
    fn parses_the_spike_signature() {
        let id = parse_signing_identity(SIGNED).unwrap();
        assert_eq!(id.team_id, "3T2D2YNTVW");
        assert_eq!(id.identifier, "us.ctm.cli");
        assert_eq!(id.cdhash, "51c099661a857daeb11a8b631adfe83b4df470b7");
        assert_eq!(
            id.authority,
            "Developer ID Application: ROBERT E LEE (3T2D2YNTVW)"
        );
        check_expected(&id, &record()).unwrap();
    }

    #[test]
    fn adhoc_and_unsigned_classify_without_being_mistaken_for_developer_id() {
        assert_eq!(classify(ADHOC), SigningState::AdHoc);
        assert_eq!(
            classify("/tmp/x: code object is not signed at all\n"),
            SigningState::Unsigned
        );
        assert!(parse_signing_identity(ADHOC).is_err());
        assert!(matches!(classify(SIGNED), SigningState::DeveloperId(_)));
    }

    #[test]
    fn every_required_property_is_load_bearing() {
        let drop_line = |needle: &str| -> String {
            SIGNED
                .lines()
                .filter(|l| !l.contains(needle))
                .map(|l| format!("{l}\n"))
                .collect()
        };
        assert!(
            parse_signing_identity(&drop_line("Timestamp=")).is_err(),
            "timestamp"
        );
        assert!(
            parse_signing_identity(&drop_line("Apple Root CA")).is_err(),
            "root"
        );
        assert!(
            parse_signing_identity(&drop_line("TeamIdentifier=")).is_err(),
            "team"
        );
        assert!(
            parse_signing_identity(&drop_line("CDHash=")).is_err(),
            "cdhash"
        );
        let no_runtime = SIGNED.replace("flags=0x10000(runtime)", "flags=0x0(none)");
        assert!(
            parse_signing_identity(&no_runtime).is_err(),
            "hardened runtime"
        );
        let two_teams = format!("{SIGNED}TeamIdentifier=3T2D2YNTVW\n");
        assert!(
            parse_signing_identity(&two_teams).is_err(),
            "duplicate team line"
        );
        let other_leaf = SIGNED.replace(
            "Developer ID Application: ROBERT E LEE (3T2D2YNTVW)",
            "Apple Development: ROBERT E LEE (3T2D2YNTVW)",
        );
        assert!(
            parse_signing_identity(&other_leaf).is_err(),
            "not a Developer ID leaf"
        );
    }

    #[test]
    fn runtime_flag_is_found_among_other_flags() {
        let multi = SIGNED.replace(
            "flags=0x10000(runtime)",
            "flags=0x10400(runtime,linker-signed)",
        );
        assert!(parse_signing_identity(&multi).is_ok());
    }

    #[test]
    fn expected_identity_is_pinned_three_ways() {
        let id = parse_signing_identity(SIGNED).unwrap();
        // Binary signed by someone else, even if the record agrees with them.
        let mut other = id.clone();
        other.team_id = "ABCDE12345".into();
        assert!(check_expected(&other, &record()).is_err());
        // Record naming a different team than the pinned one.
        let mut r = record();
        r.team_id = "ABCDE12345".into();
        assert!(check_expected(&id, &r).is_err());
        // Record for a different build of ours (cdhash mismatch).
        let mut r = record();
        r.cdhash = "84abab4406d6baf953f2a6e5a4e3b0ba7d26c9ef".into();
        assert!(check_expected(&id, &r).is_err());
        // Record with a malformed cdhash.
        let mut r = record();
        r.cdhash = "nope".into();
        assert!(check_expected(&id, &r).is_err());
    }

    #[test]
    fn describe_names_the_state() {
        assert!(describe(&classify(SIGNED)).starts_with("Developer ID 3T2D2YNTVW as us.ctm.cli"));
        assert_eq!(
            describe(&SigningState::AdHoc),
            "ad-hoc signed (no Developer ID)"
        );
        assert_eq!(describe(&SigningState::Unsigned), "not signed at all");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_running_test_binary_is_readable_and_not_a_release() {
        // cargo's test binary is linker ad-hoc signed on Apple Silicon; on Intel it
        // may be unsigned. Either way it is not a Developer ID release, and reading
        // it must not error.
        let exe = std::env::current_exe().unwrap();
        let state = signing_state(&exe).expect("macOS reports a state");
        assert!(
            matches!(state, SigningState::AdHoc | SigningState::Unsigned),
            "test binary reported {state:?}"
        );
        // And a candidate check against it fails closed rather than passing.
        assert!(verify_release_candidate(&exe, &record()).is_err());
    }
}
