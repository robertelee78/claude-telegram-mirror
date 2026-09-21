//! ADR-020: the release signature every ctm binary carries, on every platform.
//!
//! The release pipeline signs the final bytes of each `ctm-<triple>` with a
//! long-lived Ed25519 key in OpenSSH's signature format (`PROTOCOL.sshsig`), and
//! publishes `ctm-<triple>.sshsig` beside it. This module is the consumer side:
//! `ctm update` refuses a candidate whose signature is not by one of the keys pinned
//! here, in the pinned namespace, over exactly the downloaded bytes; `ctm doctor`
//! re-verifies the running binary against the signature kept beside it.
//!
//! Two keys are pinned: the signing key and an offline standby, so a leak is answered
//! by a release every installed client already trusts (see ADR-020 §Rotation).
//!
//! Verification uses `ring` (already ctm's TLS crypto), `sha2` and `base64` — no
//! new crates. Parsing is strict and pure; only the file read touches the system.

use crate::error::{AppError, Result};
use base64::Engine;
use ring::signature::{UnparsedPublicKey, ED25519};
use sha2::{Digest, Sha256, Sha512};
use std::path::Path;

/// Public keys whose signatures a release may carry: `[signing, standby]`. The
/// same two lines are pinned in `install.sh`; the contract test keeps them equal.
pub const SIGNING_KEYS: &[&str] = &[
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIG7D2ChtUbftl2H92GnPLA4ol3Wws2ksy0zzhKdByWtj",
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFIwO9PqTyjmlysEDMhb9GEhiREqxGUlDRDYCXkOvWAd",
];
/// The sshsig namespace a release signature must be made in. Never reused for
/// anything else, so a signature made for another purpose can never pass here.
pub const NAMESPACE: &str = "ctm.release";
/// An sshsig over a file is ~300 bytes; anything larger is not ours.
pub const MAX_SIG_BYTES: usize = 4096;
/// The signature is kept beside the installed binary under this name.
pub const SIGNATURE_FILE: &str = ".ctm-signature";

const MAGIC: &[u8; 6] = b"SSHSIG";
const KEY_TYPE: &[u8] = b"ssh-ed25519";
const HASH_ALG: &[u8] = b"sha512";
const BEGIN: &str = "-----BEGIN SSH SIGNATURE-----";
const END: &str = "-----END SSH SIGNATURE-----";

/// A parsed, not yet verified, version-1 Ed25519 sshsig.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshSig {
    pub public_key: [u8; 32],
    pub namespace: Vec<u8>,
    reserved: Vec<u8>,
    pub signature: [u8; 64],
}

/// Who signed, for the "verified: release key …" line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignerIdentity {
    /// `SHA256:<base64>` as `ssh-keygen -lf` prints it.
    pub fingerprint: String,
    pub namespace: String,
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.0.len() < n {
            return Err(AppError::Config("release signature is truncated".into()));
        }
        let (head, tail) = self.0.split_at(n);
        self.0 = tail;
        Ok(head)
    }
    fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn string(&mut self) -> Result<&'a [u8]> {
        let n = self.u32()? as usize;
        self.take(n)
    }
}

fn ssh_string(out: &mut Vec<u8>, s: &[u8]) {
    out.extend_from_slice(&(s.len() as u32).to_be_bytes());
    out.extend_from_slice(s);
}

fn bad(what: &str) -> AppError {
    AppError::Config(format!("release signature: {what}"))
}

/// The raw 32-byte key inside an `ssh-ed25519 AAAA…` line.
pub fn parse_public_key_line(line: &str) -> Result<[u8; 32]> {
    let mut parts = line.split_ascii_whitespace();
    if parts.next() != Some("ssh-ed25519") {
        return Err(bad("pinned key is not ssh-ed25519"));
    }
    let b64 = parts.next().ok_or_else(|| bad("pinned key has no data"))?;
    let blob = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|_| bad("pinned key is not base64"))?;
    let mut r = Reader(&blob);
    if r.string()? != KEY_TYPE {
        return Err(bad("pinned key blob is not ssh-ed25519"));
    }
    let raw = r.string()?;
    if !r.0.is_empty() || raw.len() != 32 {
        return Err(bad("pinned key blob is malformed"));
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(raw);
    Ok(key)
}

/// The pinned keys, decoded. A malformed constant is a build defect, not a
/// runtime condition, so this panics rather than returning an error.
pub fn pinned_keys() -> Vec<[u8; 32]> {
    SIGNING_KEYS
        .iter()
        .map(|l| parse_public_key_line(l).expect("SIGNING_KEYS are well-formed"))
        .collect()
}

/// `SHA256:<base64 without padding>` of the ssh public key blob — what
/// `ssh-keygen -lf` prints, so a human can match it against the pins.
pub fn fingerprint(public_key: &[u8; 32]) -> String {
    let mut blob = Vec::with_capacity(4 + KEY_TYPE.len() + 4 + 32);
    ssh_string(&mut blob, KEY_TYPE);
    ssh_string(&mut blob, public_key);
    let digest = Sha256::digest(&blob);
    format!(
        "SHA256:{}",
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(digest)
    )
}

/// Strict parse of an armored sshsig: version 1, `ssh-ed25519` only, `sha512`
/// only, no trailing bytes anywhere.
pub fn parse_sshsig(armored: &str) -> Result<SshSig> {
    if armored.len() > MAX_SIG_BYTES {
        return Err(bad("larger than any release signature"));
    }
    let mut lines = armored.lines().map(str::trim);
    if lines.next() != Some(BEGIN) {
        return Err(bad("missing BEGIN SSH SIGNATURE header"));
    }
    let mut body = String::new();
    let mut ended = false;
    for l in lines.by_ref() {
        if l == END {
            ended = true;
            break;
        }
        body.push_str(l);
    }
    if !ended || lines.any(|l| !l.is_empty()) {
        return Err(bad("missing END SSH SIGNATURE footer or trailing text"));
    }
    let blob = base64::engine::general_purpose::STANDARD
        .decode(body)
        .map_err(|_| bad("body is not base64"))?;
    let mut r = Reader(&blob);
    if r.take(6)? != MAGIC {
        return Err(bad("not an SSH signature"));
    }
    if r.u32()? != 1 {
        return Err(bad("unsupported version"));
    }
    let pk_blob = r.string()?;
    let namespace = r.string()?.to_vec();
    let reserved = r.string()?.to_vec();
    let hash_alg = r.string()?;
    let sig_blob = r.string()?;
    if !r.0.is_empty() {
        return Err(bad("trailing bytes"));
    }
    if namespace.is_empty() {
        return Err(bad("empty namespace"));
    }
    if hash_alg != HASH_ALG {
        return Err(bad("hash algorithm is not sha512"));
    }
    let mut pk = Reader(pk_blob);
    if pk.string()? != KEY_TYPE {
        return Err(bad("key type is not ssh-ed25519"));
    }
    let raw = pk.string()?;
    if !pk.0.is_empty() || raw.len() != 32 {
        return Err(bad("public key is malformed"));
    }
    let mut sg = Reader(sig_blob);
    if sg.string()? != KEY_TYPE {
        return Err(bad("signature algorithm is not ssh-ed25519"));
    }
    let sig = sg.string()?;
    if !sg.0.is_empty() || sig.len() != 64 {
        return Err(bad("signature is malformed"));
    }
    let mut public_key = [0u8; 32];
    public_key.copy_from_slice(raw);
    let mut signature = [0u8; 64];
    signature.copy_from_slice(sig);
    Ok(SshSig {
        public_key,
        namespace,
        reserved,
        signature,
    })
}

/// Verify `armored` over `message` against `allowed` keys in `namespace`. Pure.
pub fn verify_with(
    message: &[u8],
    armored: &str,
    allowed: &[[u8; 32]],
    namespace: &str,
) -> Result<SignerIdentity> {
    let sig = parse_sshsig(armored)?;
    if !allowed.contains(&sig.public_key) {
        return Err(AppError::Config(format!(
            "release signature is by an unpinned key ({})",
            fingerprint(&sig.public_key)
        )));
    }
    if sig.namespace != namespace.as_bytes() {
        return Err(AppError::Config(format!(
            "release signature is in namespace {:?}, expected {namespace:?}",
            String::from_utf8_lossy(&sig.namespace)
        )));
    }
    // PROTOCOL.sshsig: the signed blob is MAGIC || namespace || reserved ||
    // hash_alg || H(message), each as an ssh string except the magic.
    let mut signed =
        Vec::with_capacity(6 + 4 * 4 + sig.namespace.len() + sig.reserved.len() + 6 + 64);
    signed.extend_from_slice(MAGIC);
    ssh_string(&mut signed, &sig.namespace);
    ssh_string(&mut signed, &sig.reserved);
    ssh_string(&mut signed, HASH_ALG);
    ssh_string(&mut signed, &Sha512::digest(message));
    UnparsedPublicKey::new(&ED25519, sig.public_key)
        .verify(&signed, &sig.signature)
        .map_err(|_| AppError::Config("release signature does not match the bytes".into()))?;
    Ok(SignerIdentity {
        fingerprint: fingerprint(&sig.public_key),
        namespace: namespace.to_string(),
    })
}

/// Everything `ctm update` requires of a downloaded candidate, on every platform:
/// a signature by a pinned key, in the release namespace, over exactly these bytes.
pub fn verify_release_candidate(path: &Path, armored: &str) -> Result<SignerIdentity> {
    let bytes = std::fs::read(path)?;
    verify_with(&bytes, armored, &pinned_keys(), NAMESPACE)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/release-sig");
    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!("{DIR}/{name}")).unwrap()
    }
    fn fixture_text(name: &str) -> String {
        String::from_utf8(fixture(name)).unwrap()
    }
    fn throwaway() -> [u8; 32] {
        parse_public_key_line(&fixture_text("throwaway.pub")).unwrap()
    }

    #[test]
    fn the_pins_are_two_well_formed_keys_with_the_documented_fingerprints() {
        let keys = pinned_keys();
        assert_eq!(keys.len(), 2, "signing key and offline standby");
        assert_ne!(keys[0], keys[1]);
        assert_eq!(
            fingerprint(&keys[0]),
            "SHA256:0biQ8NOuSS0b7nEU/71bWgZ9yNDa3nf5QFXzJtv60ck"
        );
        assert_eq!(
            fingerprint(&keys[1]),
            "SHA256:yj06/vovmttxLO6PtKmRDsAf+2FcBuzqfpzQYUGy2/w"
        );
    }

    #[test]
    fn a_signature_made_by_ssh_keygen_verifies() {
        let id = verify_with(
            &fixture("blob.bin"),
            &fixture_text("blob.bin.sshsig"),
            &[throwaway()],
            NAMESPACE,
        )
        .unwrap();
        assert_eq!(
            id.fingerprint,
            "SHA256:fkbCNYMua39sKqu88t0DonRh2ekRoAoRDShQSZybgD4"
        );
        assert_eq!(id.namespace, NAMESPACE);
    }

    #[test]
    fn the_real_signing_key_verifies_against_the_pins() {
        // Signed once with the actual release key; proves the pin IS that key.
        let id = verify_with(
            &fixture("blob.bin"),
            &fixture_text("blob.bin.primary.sshsig"),
            &pinned_keys(),
            NAMESPACE,
        )
        .unwrap();
        assert_eq!(id.fingerprint, fingerprint(&pinned_keys()[0]));
    }

    #[test]
    fn every_required_property_is_load_bearing() {
        let blob = fixture("blob.bin");
        let sig = fixture_text("blob.bin.sshsig");
        let keys = [throwaway()];
        // Bytes changed.
        let mut tampered = blob.clone();
        tampered[10] ^= 0x01;
        assert!(
            verify_with(&tampered, &sig, &keys, NAMESPACE).is_err(),
            "tampered bytes"
        );
        // Right key, wrong namespace.
        assert!(
            verify_with(
                &blob,
                &fixture_text("blob.bin.other-namespace.sshsig"),
                &keys,
                NAMESPACE
            )
            .is_err(),
            "other namespace"
        );
        // Right namespace, unpinned key.
        assert!(
            verify_with(
                &blob,
                &fixture_text("blob.bin.other-key.sshsig"),
                &keys,
                NAMESPACE
            )
            .is_err(),
            "other key"
        );
        // Right everything, but the consumer pins a different namespace.
        assert!(
            verify_with(&blob, &sig, &keys, "file").is_err(),
            "consumer namespace"
        );
        // Signature text damaged.
        let mut flipped = sig.clone();
        let i = flipped.find("AAAA").unwrap() + 40;
        flipped.replace_range(i..i + 1, if &flipped[i..i + 1] == "A" { "B" } else { "A" });
        assert!(
            verify_with(&blob, &flipped, &keys, NAMESPACE).is_err(),
            "flipped base64"
        );
        // Truncated / oversized / not armored.
        assert!(parse_sshsig(&sig[..sig.len() / 2]).is_err(), "truncated");
        assert!(
            parse_sshsig(&"x".repeat(MAX_SIG_BYTES + 1)).is_err(),
            "oversized"
        );
        assert!(parse_sshsig("not a signature").is_err(), "not armored");
        assert!(
            parse_sshsig(&format!("{sig}\nextra")).is_err(),
            "trailing text"
        );
        // No key pinned at all.
        assert!(verify_with(&blob, &sig, &[], NAMESPACE).is_err(), "no pins");
    }

    #[test]
    fn a_signature_by_the_pinned_keys_is_not_accepted_for_a_throwaway_consumer() {
        // The other direction of the key check.
        assert!(verify_with(
            &fixture("blob.bin"),
            &fixture_text("blob.bin.primary.sshsig"),
            &[throwaway()],
            NAMESPACE
        )
        .is_err());
    }

    #[test]
    fn public_key_lines_are_parsed_strictly() {
        assert!(parse_public_key_line("ssh-rsa AAAAB3NzaC1yc2E=").is_err());
        assert!(parse_public_key_line("ssh-ed25519").is_err());
        assert!(parse_public_key_line("ssh-ed25519 not-base64!").is_err());
        assert!(parse_public_key_line(SIGNING_KEYS[0]).is_ok());
        // A trailing comment is fine, as in a real `.pub` file.
        assert!(parse_public_key_line(&format!("{} comment here", SIGNING_KEYS[0])).is_ok());
    }

    #[test]
    fn verify_release_candidate_reads_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ctm");
        std::fs::write(&p, fixture("blob.bin")).unwrap();
        assert!(verify_release_candidate(&p, &fixture_text("blob.bin.primary.sshsig")).is_ok());
        assert!(
            verify_release_candidate(&p, &fixture_text("blob.bin.sshsig")).is_err(),
            "throwaway is not pinned"
        );
    }
}
