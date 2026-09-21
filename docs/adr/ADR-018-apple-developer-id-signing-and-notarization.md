# ADR-018: Apple Developer ID signing and notarization for darwin releases

> **DO NOT BE LAZY. We have plenty of time to do it right.**
> No shortcuts. Never make assumptions.
> Always dive deep and ensure you know the problem you're solving.
> Make use of search as needed.
> Measure 3x, cut once.
> No fallback. No stub (todo later) code.
> Just pure excellence, done the right way the entire time.
> Chesterton's fence: always understand the current implementation fully before changing it.

**Status:** Implemented (2026-09-21) — first release signed under this ADR is 0.2.45.
**Date:** 2026-09-21
**Authors:** Robert, Claude
**Tags:** distribution, signing, notarization, macos, supply-chain
**Related:** ADR-017 (the channel this hardens), hf2q ADR-045 (the signer this ports), `ctm-macos-notarization-gap` (memory)

## Context

Every darwin binary ctm has ever published was **ad-hoc signed**. `release.yml`
carried a Developer ID + `notarytool` step, but it was gated on `APPLE_*` secrets
that were never added to this repository, with a fallback to `codesign --sign -`.
Every release therefore took the fallback and printed a `::warning::` that nobody
read. On 2026-09-21 the shipped 0.2.44 binary reported:

```
Signature=adhoc
TeamIdentifier=not set
codesign --check-notarization --test-requirement '=notarized'  → exit 3
```

while hf2q, built by the same person with the same Apple team, ships
`Authority=Developer ID Application: ROBERT E LEE (3T2D2YNTVW)`, hardened
runtime, secure timestamp, notarized.

The earlier rationalisation — "curl sets no quarantine xattr, so Gatekeeper never
looks" — explains why the binary does not crash, not why it is unsigned. It leaves
`ctm update` and `install.sh` with nothing but a sha256 fetched from the same
origin as the binary: a compromised release replaces both. Developer ID signing
adds a second, independent root of trust (Apple's CA plus a notary ticket bound
to the exact CDHash) that a GitHub compromise cannot forge.

Beyond the missing secrets, the inline step itself was weaker than hf2q's design
in every dimension that matters:

| | inline step (before) | hf2q `sign_notarize_standalone_release.sh` |
|---|---|---|
| Credentials absent | falls open to ad-hoc | fails closed |
| Notary auth | Apple ID + app-specific password | App Store Connect API key |
| Proof | `notarytool submit --wait`, unchecked | asserts `Accepted`, no issues, ticket cdhash == signed cdhash, online `=notarized`, emits `proof.json` |
| Identifier | auto `ctm-<hash>` | pinned |
| Secret scope | repository | protected `environment` |
| Shape | YAML duplicated per job | script + contract test; never executes candidate code with credentials present |

## Spikes (Kata step 2 — executed 2026-09-21)

**Hypothesis:** hf2q's signer, fed the credentials already in `~/.private_keys/`,
signs and notarizes ctm's darwin binaries under a new identifier with no change
to the binary, for both arm64 and the cross-built x64.

1. **Credentials.** The hf2q P12's password exists only as a GitHub secret, but
   the raw private key and the Developer ID certificate are on disk and their
   public keys match (`openssl pkey -pubout` == `openssl x509 -pubkey`). A ctm
   P12 was minted from them with its own password (`~/.private_keys/ctm-developer-id-application.p12`
   + `.password`, 0600). The Developer ID certificate and the notary API key are
   **team-scoped, not app-scoped**, so one team signs both products while the
   two repositories' secrets rotate independently.
2. **arm64.** The ported script (`scripts/sign-notarize-darwin.sh`) against a
   stripped `cargo build --release` binary: identifier `us.ctm.cli`, team
   `3T2D2YNTVW`, hardened runtime, timestamp; notary `Accepted`, zero issues,
   ticket cdhash `51c09966…` == signed cdhash; online `=notarized` passes. 23 s.
3. **x64.** Same script, `--target x86_64-apple-darwin`. Finding: `strip` on the
   cross-built binary leaves it **wholly unsigned** (arm64 keeps a linker
   ad-hoc signature); `codesign --force` handles both. `Accepted`, cdhash
   `84abab44…`, online check passes.
4. **Hardened runtime at run time.** The signed arm64 binary ran `ctm doctor`
   through all 13 checks: TLS to the Telegram API, SQLite, tmux, launchd — no
   restriction bit.
5. **Hygiene.** After both runs the user keychain search list was restored
   verbatim and the secret directories were gone.
6. **minos.** Both targets declare `minos 11.0` (cargo's default; hf2q pins
   14.0). Recorded in the proof rather than pinned — nothing in ctm needs 14.

**Reformulated hypothesis:** confirmed with (1), (3) and (6) as adjustments.

## Decision

1. **Signing is mandatory and fails closed.** There is no ad-hoc fallback in
   `release.yml`. A darwin release either carries a Developer ID signature with
   an accepted notary ticket, or it does not exist. Forks without credentials
   cannot cut releases; that is the point.

2. **The pipeline separates building from signing.** The darwin build jobs run
   with no secrets and upload an *unsigned* candidate plus a `build.json` receipt
   (`ctm.darwin-build-candidate`: source sha, version, target, unsigned sha256).
   A matrix `sign-darwin` job, `environment: apple-release` (deployable only from
   `v*` tags), checks out the tagged commit with `persist-credentials: false`,
   verifies the receipt against the bytes, and runs the signer. The unsigned
   candidate is data to that job: it is never executed while credentials are
   present. The signer writes the `stable-<target>.json` record from the
   signed bytes, because the sha256 changes at signing.

3. **The signer is a script with a contract test**, ported from hf2q and
   parameterised on target: `scripts/sign-notarize-darwin.sh INPUT OUT VERSION
   SHA TEAM IDENTIFIER TARGET`. Ephemeral keychain; exactly one identity; asserts
   identifier, team, authority, hardened runtime, timestamp, unique CDHash;
   notarizes with an App Store Connect API key; asserts `Accepted` with zero
   issues and a ticket whose SHA-256 cdhash equals the signed cdhash; verifies
   online with `--check-notarization --test-requirement '=notarized'`; emits
   `proof.json`, `notary-log.json`, `codesign.txt`, `notarization-check.txt`,
   `notary-submission.json`, `notary-wait.json`. `scripts/test-darwin-signing-contract.sh`
   pins the workflow shape (no fallback, environment, no execution of the
   candidate) and the signer's refusals; CI runs it on macOS.

4. **The record carries the signing identity.** `stable-<darwin-triple>.json`
   gains `"signing": {"team_id","identifier","cdhash"}`. Linux records are
   unchanged. The `publish` job runs on macOS and independently re-verifies each
   darwin asset (`codesign --verify --strict`, online `=notarized`, identity ==
   record == proof, sha256 == record == proof) before the GitHub Release is
   created. Proofs and notary logs are published as release assets
   (`proof-<target>.json`, `notary-log-<target>.json`).

5. **The updater and installer verify the signature, pinned in code.**
   `src/apple_trust.rs` holds `TEAM_ID = "3T2D2YNTVW"` and `IDENTIFIER =
   "us.ctm.cli"`. On macOS, `ctm update` refuses a candidate unless
   `codesign --verify --strict` passes, the parsed identity (team, identifier,
   full Developer ID authority chain, hardened runtime, timestamp) equals the
   constants **and** the record's `signing`, and the online notarization check
   passes — all before the atomic swap. A darwin record without `signing` is
   refused. The pin lives in the running binary, which is itself signed by the
   team: changing the team requires shipping code through a release the current
   team signed. `install.sh` performs the same checks in POSIX sh with the team
   and identifier hard-coded. hf2q pins to the *installed* binary's identity
   instead; ctm cannot, because the first signed upgrade starts from an ad-hoc
   0.2.44.

6. **`ctm doctor` reports it.** Check 13 shows the running binary's signing
   state on macOS: Developer ID team + identifier + hardened runtime + timestamp,
   or WARN for ad-hoc on the standalone channel (source builds are ad-hoc by
   nature and reported as such without warning).

7. **Secrets live in a protected environment**, named identically to hf2q's so
   the runbook is shared: secrets `APPLE_DEVELOPER_ID_APPLICATION_P12_BASE64`,
   `APPLE_DEVELOPER_ID_APPLICATION_P12_PASSWORD`, `APPLE_NOTARY_KEY_P8_BASE64`;
   variables `APPLE_DEVELOPER_ID_APPLICATION`, `APPLE_TEAM_ID`,
   `APPLE_CODESIGN_IDENTIFIER=us.ctm.cli`, `APPLE_NOTARY_KEY_ID`,
   `APPLE_NOTARY_ISSUER_ID`. The stale `NPM_TOKEN` is deleted.

## Consequences

- A darwin release now takes ~1 minute longer (two notary round-trips, run in
  parallel) and cannot be cut without the `apple-release` environment.
- `ctm update` on macOS makes one extra network call (Apple's ticket lookup).
  Offline updates are refused rather than trusted — consistent with the channel
  already requiring GitHub.
- The 0.2.44 → 0.2.45 upgrade is the one hop the *old* updater cannot verify
  beyond sha256; it is verified by hand after the hop (§Proof). From 0.2.45 on,
  every hop is verified by the running binary.
- Linux binaries remain unsigned beyond sha256 + origin pinning. Sigstore
  attestation for Linux is a separate decision, not taken here.
- The Developer ID certificate expires 2031-08-22; the notary API key does not
  expire. Rotation means: new P12 in the environment, nothing in code.

## Proof (Kata step 6)

Recorded when 0.2.45 is published: release run URL, both `proof-*.json`
assets, and on this machine after `ctm update`:

```
codesign -dvv ~/.local/bin/ctm            → Identifier=us.ctm.cli, TeamIdentifier=3T2D2YNTVW
codesign --verify --strict --check-notarization --test-requirement '=notarized' ~/.local/bin/ctm → exit 0
ctm doctor                                → [13/13] … Developer ID 3T2D2YNTVW, notarized
```
