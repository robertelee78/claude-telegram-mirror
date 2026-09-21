# ADR-020: An Ed25519 release signature on every binary, verified by every consumer

> **DO NOT BE LAZY. We have plenty of time to do it right.**
> No shortcuts. Never make assumptions.
> Always dive deep and ensure you know the problem you're solving.
> Make use of search as needed.
> Measure 3x, cut once.
> No fallback. No stub (todo later) code.
> Just pure excellence, done the right way the entire time.
> Chesterton's fence: always understand the current implementation fully before changing it.

**Status:** Implemented (2026-09-21) — first release signed under this ADR is 0.2.48; 0.2.49 the same day fixed the installer's exit status and taught `doctor --fix` to fetch a missing signature. Proof below.
**Date:** 2026-09-21
**Authors:** Robert, Claude
**Tags:** distribution, signing, supply-chain, linux, macos
**Related:** ADR-018 (Apple signing; this is its platform-independent sibling), ADR-017 (the channel), `docs/research/linux-release-signing-2026-09-21.md` (the research this decides on)

## Context

ADR-018 gave the macOS binaries a second root of trust: Apple's CA and notary
ticket, pinned to a team and identifier in the consumer. The Linux binaries still
had only a sha256 fetched from the same origin as the bytes — a replaced GitHub
Release replaces both — and the macOS binaries had no signature Apple could not
revoke or refuse.

The research (`docs/research/linux-release-signing-2026-09-21.md`, 2026-09-21,
sourced and spiked) surveyed Sigstore keyless, GitHub artifact attestations,
minisign/signify/raw Ed25519, SSH signatures and OpenPGP, against ctm's actual
consumers: a POSIX `install.sh` with `curl`, and a 10 MB Rust self-updater built
on `reqwest`/rustls/`ring`. The findings that decide it:

1. **`ring` is already ctm's only crypto provider** and contains Ed25519; a
   verifier for OpenSSH's signature format costs **zero new crates** (spiked: 68
   lines with `ring`, `sha2`, `base64`, all already in the tree).
2. **Every Sigstore verification crate hard-depends on `aws-lc-rs`** — a second
   TLS-grade crypto stack with a C/asm build — and **Sigstore's trust root is not
   ours and rotates** (Fulcio 2022, CT 2022, TSA 2025-07, Rekor shard 2025-09-23).
   A root pinned into the self-updater breaks on the next rotation, and the fix
   is the thing that is broken.
3. **The only signature a POSIX shell can verify with tools that are actually
   present is an SSH signature** via `ssh-keygen -Y verify` (OpenSSH ≥ 8.1,
   2019). `openssl pkeyutl -rawin` needs OpenSSL ≥ 3.0 and macOS ships LibreSSL,
   which cannot load an Ed25519 key at all. Sigstore bundles and GitHub
   attestations cannot be verified in sh.
4. Prior art is thin: of the shipped Rust CLIs surveyed only `mise` verifies a
   signature inside its self-updater; `rustup` removed verification in 2023.
5. The `apple-release` environment had **no required reviewer**: a leaked token
   with `contents: write` could push a `v*` tag and have the Apple key used
   unattended.

## Decision

1. **One long-lived Ed25519 key signs the final bytes of all four release
   binaries** (the darwin ones *after* Apple signing) in OpenSSH signature format
   (`PROTOCOL.sshsig`, `ssh-ed25519`, `sha512`, namespace `ctm.release`),
   published as `ctm-<triple>.sshsig` beside each binary. The record
   `stable-<triple>.json` does not change (its field set is frozen; ADR-018).
2. **Two public keys are pinned** — the signing key and an **offline standby** —
   in `rust-crates/ctm/src/release_trust.rs` and `install.sh`, and asserted equal
   by the contract test and by the signing job before it signs. The standby's
   private half never touches GitHub; it exists so that a leak of the signing key
   is answered by cutting a release signed with a key every installed client
   already trusts, rather than by stranding them.
3. **Consumers verify fail-closed, on every platform, before the swap.**
   `ctm update` (`ring`, no network beyond the download) and `install.sh`
   (`ssh-keygen -Y verify`) require a signature by a pinned key, in the pinned
   namespace, over exactly the downloaded bytes. Order: sha256 → Ed25519 → (macOS)
   Apple → publish. Both consumers keep the signature beside the installed binary
   (`.ctm-signature`) so `ctm doctor` can re-verify the running binary offline on
   Linux as well as macOS.
4. **Signing is a separate, reviewer-gated job.** `sign-release` runs under the
   environment `release-signing` (tags `v*` only, **required reviewer: the
   owner**) after every build and after Apple signing, with
   `persist-credentials: false`, never executes a candidate, asserts the key's
   public half equals the pins in both consumers, signs with
   `scripts/sign-release.sh`, self-verifies with an `allowed_signers` built from
   the *install.sh* pin, and emits `sigproof-<triple>.json`. The same required
   reviewer is added to `apple-release`. A release now needs an explicit approval
   after the tag push.
5. **`publish` re-verifies every signature independently** (`ssh-keygen -Y
   verify` against the install.sh pin, `find-principals`, sigproof ↔ record
   agreement) before the Release exists, then runs `actions/attest-build-provenance`
   so auditors get SLSA provenance via `gh attestation verify`. The repository's
   **immutable releases** setting is enabled: published assets and tags cannot be
   changed afterwards. Third-party actions are pinned by commit SHA.
6. **Proof is as a user**, in CI and by hand: a `verify-install` job runs the
   *published* `install.sh` on ubuntu and macOS after every release; `ctm update`
   on this machine prints the release-key verification before the Apple one.

## Rotation

- *Planned*: release N adds the next key's public half to both pin sets while
  still signing with the current key; release N+1 signs with the new key. A
  client that skipped N fails closed at N+1 and recovers with `install.sh`.
- *Leak*: cut a release immediately, signed with the offline standby; pin
  `[standby, new-standby]`; rotate the environment secret; audit the run log.
  No installed client is stranded, because the standby was pinned in advance.
- Keys do not expire; `allowed_signers` validity windows are evaluated at
  verification time and are therefore **not** used for the pin.

## Rejected

Sigstore keyless / GitHub attestations as the *consumer* layer (second crypto
stack, external rotating root, unverifiable in sh — adopted only as the auditor
layer); minisign (no shell verifier); raw Ed25519 via OpenSSL (≥ 3.0 only, no
namespace, dominated by sshsig which OpenSSL 3 can also verify); OpenPGP;
per-release keys (need a third-party immutable anchor ctm no longer has);
signing the record instead of the binary (equivalent, but asymmetric with
Apple's bytes and not what `ssh-keygen -Y verify < file` expects).

## Consequences

- `install.sh` now requires `ssh-keygen` with `-Y` (OpenSSH ≥ 8.1) on every
  platform, and says so when it is missing. No skip variable.
- macOS updates must satisfy both the release key and Apple; a rotation mistake
  would break both platforms, which is why the contract test pins the key in
  three files and the signing job refuses a key the consumers do not pin.
- Residual, stated: a compromised build step signs what it built (mitigated by
  SHA-pinned actions and provenance, not eliminated); a publisher who can only
  re-serve an old signed release as `latest` is refused by `ctm update`'s SemVer
  check but not by `install.sh`; full owner-account compromise defeats every
  on-GitHub option — only the offline standby survives it.

## Proof (Kata step 6 — recorded 2026-09-21)

- Release runs: `v0.2.48` 35599051380 (publish verified all four signatures; its
  `verify-install` then exposed the installer's exit-status bug), `v0.2.49`
  35600096095 — **every job green**, including `verify-install` running the
  *published* `install.sh` as a user: ubuntu `verified: release signature —
  SHA256:0biQ8NOu…`, `installed: … (ctm 0.2.49)`; macOS the same plus
  `verified: Developer ID 3T2D2YNTVW as us.ctm.cli, notarized`.
- Both gated environments required an explicit approval after the tag push
  (approved by the owner via `gh api …/pending_deployments`).
- Provenance: `gh attestation verify ctm-x86_64-unknown-linux-gnu -R
  robertelee78/claude-telegram-mirror` → built by
  `.github/workflows/release.yml @ refs/tags/v0.2.48`.
- On this Mac, as a user, through the first updater that verifies the signature:

```
$ ctm update
downloading ctm 0.2.49 for aarch64-apple-darwin …
verified: release signature by key SHA256:0biQ8NOuSS0b7nEU/71bWgZ9yNDa3nf5QFXzJtv60ck (namespace ctm.release)
verified: Developer ID 3T2D2YNTVW as us.ctm.cli, notarized
installed ctm 0.2.49 at /Users/robert.lee/.local/bin/ctm
$ ctm doctor
[13/13]   OK: Update: standalone, 0.2.49
release signature: verified, key SHA256:0biQ8NOuSS0b7nEU/71bWgZ9yNDa3nf5QFXzJtv60ck (ctm.release)
$ ssh-keygen -Y verify -f allowed_signers -I release@ctm.cli -n ctm.release -s ctm-aarch64-apple-darwin.sshsig < ~/.local/bin/ctm
Good "ctm.release" signature for release@ctm.cli with ED25519 key SHA256:0biQ8NOu…
```

- Negative cases, `install.sh` against a stand-in serving the real asset: a
  signature by another key → `release signature verification failed`; no
  signature → `could not fetch the release signature`; nothing installed either
  time. Rust: a one-byte change → `release signature does not match the bytes`.
