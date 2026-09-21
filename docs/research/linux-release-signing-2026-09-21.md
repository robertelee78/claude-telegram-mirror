# Linux release signing for ctm — research report

**Date:** 2026-09-21
**Scope:** give `ctm-<linux-triple>` an independent root of trust, verified by `ctm update` and `install.sh`, of a strength equivalent to ADR-018's Developer ID + notarization design for darwin. Research only; no repository files were modified.
**Method:** local code read (`release.yml`, ADR-018, `apple_trust.rs`, `update.rs`, `install.sh`, `test-darwin-signing-contract.sh`, `cargo tree`), primary documentation fetched on 2026-09-21 (cited inline), and four executed spikes in the scratchpad (`spike/`). Anything not verified is marked **[unverified]**.

---

## 0. Summary of findings that drive the decision

1. **`ring` is already ctm's only crypto provider** (rustls → `ring 0.17.14`; no `aws-lc-rs`), and `ring::signature::ED25519` is in it. Raw Ed25519 verification in `ctm update` costs **zero new crates** (spike 4: a full sshsig verifier in 68 lines using only `ring`, `sha2`, `base64` — all three already in `cargo tree -p ctm`).
2. **Every Sigstore verification crate pulls in `aws-lc-rs` as a hard dependency** (`sigstore 0.14.0`: 23 required deps incl. `aws-lc-rs`, `tough`, `x509-cert`; the modular `sigstore-verify 0.11.0` → `sigstore-crypto` → `aws-lc-rs`, and `rustls-webpki[aws-lc-rs]`). That is a second TLS-grade crypto stack next to `ring`, a C/asm build dependency, and cross-compile friction for `aarch64-unknown-linux-gnu`.
3. **Sigstore's trust root is not under the maintainer's control and it rotates.** The public-good `trusted_root.json` shows a Fulcio CA rotation (2022), a CT-log rotation (2022), a TSA added 2025-07, and a new Rekor shard `log2025-1.rekor.sigstore.dev` added 2025-09-23. A trust root pinned into a 10 MB self-updater breaks on the next rotation, and the thing that would fix it (`ctm update`) is the thing that is broken — unless the updater grows a TUF client.
4. **A POSIX-sh consumer can verify exactly one thing without new tooling assumptions: an Ed25519 signature via `ssh-keygen -Y verify` (OpenSSH ≥ 8.1, Oct 2019).** `openssl pkeyutl -rawin` needs **OpenSSL ≥ 3.0** (1.1.1's `pkeyutl` documents "The Ed25519 and Ed448 signature algorithms are not supported by this utility"); macOS's `/usr/bin/openssl` is LibreSSL 3.3.6 and cannot even load an Ed25519 key (spike 1). Sigstore bundles and GitHub attestations cannot be verified in sh at all.
5. **GitHub changed the platform under us in 2025:** *immutable releases* (GA 2025-10-28) lock assets and the tag after publish, and every immutable release gets a GitHub-signed *release attestation*. Public-repo assets today carry **two** attestations (observed on `cargo-binstall` and `uv` via the unauthenticated attestations API): the workflow's SLSA provenance (public-good Fulcio, Rekor entry) and GitHub's release attestation (`O=GitHub, Inc., CN=Attester`, SAN `https://dotcom.releases.github.com`, GitHub TSA, no Rekor entry). ctm's repo has `immutable_releases: null` and v0.2.46 is `immutable: false`. Turning this on is a free, platform-level answer to the literal threat ("replace GitHub Release assets") — it is not a substitute for a consumer-verified signature, but it is the cheapest hardening available.
6. **The `apple-release` environment has no required reviewer** (`protection_rules: [branch_policy]` only). Any token with `contents: write` can push a `v*` tag and the Apple secrets are used unattended. Whatever Linux design is chosen, adding *required reviewers = owner* to the signing environment(s) is the single control that makes "attacker with a leaked write token" strictly weaker than "the owner at the keyboard".
7. **Prior art among shipped Rust CLIs is thin on in-updater verification.** Only `mise` verifies an Ed25519 signature inside its self-updater (zipsign, public key `include_bytes!`'d). `uv`'s self-update runs its installer script (sha256 pinned in the attested script). `cargo-binstall` verifies minisign for packages that declare it, with a just-in-time per-release key anchored in crates.io. `rustup` removed signature verification in 1.26.0 (2023-04-25) and still "does not yet validate signatures of downloads". `deno upgrade` only checks a checksum if you pass `--checksum`. `just`, `starship`, `zig` publish material for manual verification only.

**Recommendation (one line):** a long-lived Ed25519 key held in a reviewer-gated GitHub environment, signing the final bytes of **all four** release binaries in **OpenSSH signature format** (`PROTOCOL.sshsig`, namespace-bound), public key(s) pinned in `release_trust.rs` and `install.sh`, verified fail-closed by `ctm update` (ring, zero new crates) and by `install.sh` (`ssh-keygen -Y verify`), a **pre-pinned offline standby key** for rotation/leak recovery, plus the two free platform layers (immutable releases, `actions/attest-build-provenance`) for auditors. Full design in §7.

---

## 1. What the code does today (grounding)

| Piece | Fact (from source) |
|---|---|
| `release.yml` `build-linux` | unprivileged; strips, writes `ctm-<triple>`, `.sha256`, and `stable-<triple>.json` via `jq` from the bytes; uploads `release-<triple>`. Actions are pinned by **tag** (`actions/checkout@v4`, `dtolnay/rust-toolchain@stable`, `Swatinem/rust-cache@v2`, `softprops/action-gh-release@v2`), not by SHA. |
| `sign-darwin` | `environment: apple-release`, `persist-credentials: false`, verifies `git rev-parse HEAD == GITHUB_SHA`, verifies the `build.json` receipt, **greps the pins in `apple_trust.rs` and `install.sh` against the environment's variables before signing**, never executes the candidate. |
| `publish` | macOS; asserts tag == Cargo version, size/sha256 == record, frozen record key set, full `codesign` re-verification incl. online notarization; `contents: write`; creates the Release. |
| Record | `{"kind","schema_version","package","channel","target","version","size","sha256"}` — frozen (pre-0.2.46 clients `deny_unknown_fields`). |
| `update.rs` | reqwest (rustls, `ring` provider), `sha2`, `serde_json`, `semver`; fetches record via `latest/download` with cache-busting, asset from the versioned URL, origin-pinned to `github.com` / `release-assets.githubusercontent.com` / `objects.githubusercontent.com`, streams with size bound + sha256, `verify_apple_signature` (macOS only), atomic publish, re-digest. |
| `install.sh` | POSIX sh; `curl` + `sha256sum`/`shasum`; `--max-filesize`; origin check; on Darwin: full `codesign` pin check (team, identifier, Developer ID chain, hardened runtime, timestamp, online notarization). Linux: sha256 only. |
| `apple_trust.rs` | pins `TEAM_ID`, `IDENTIFIER`; pure parser with captured-output tests; `verify_release_candidate` = static verify → identity → online ticket. |
| Contract test | `scripts/test-darwin-signing-contract.sh`: pins agree across three files; no fallback strings; build job sees no secrets; sign job bound to environment; publish verifies online; signer refusal probes on macOS. |
| `cargo tree -p ctm` | 239 crates; crypto present: `ring 0.17.14`, `rustls 0.23.37`, `rustls-webpki 0.103.9`, `sha2 0.10.9`, `digest`, `base64 0.22.1`, `subtle`, `zeroize`. **No** `aws-lc-rs`, `openssl`, `ed25519-dalek`, `curve25519-dalek`. |
| GitHub state (API, 2026-09-21) | `visibility: public`, `immutable_releases: null`, latest `v0.2.46` `immutable: false`; release assets carry `digest` fields; **no attestations** for the Linux asset (HTTP 404); environment `apple-release`: deployment tags policy only, **0 reviewers**; no rulesets. |

Binary size reference: 10,605,840 bytes (v0.2.46 aarch64-apple-darwin, from `update.rs` test fixture).

---

## 2. Options survey

### 2a. Sigstore keyless (`cosign sign-blob` + GitHub OIDC → `.sigstore.json` bundle)

**What it proves.** "Bytes B were signed at time T by a job whose GitHub OIDC token said: workflow `https://github.com/robertelee78/claude-telegram-mirror/.github/workflows/release.yml@refs/tags/vX.Y.Z`, commit SHA, runner environment, event, repository id, and (since Fulcio added extension `1.3.6.1.4.1.57264.1.23`) the deployment environment." Fulcio issues a ~10-minute certificate binding the ephemeral key to that identity; the signature is logged in Rekor (transparency) and/or timestamped by the Sigstore TSA. Consumers verify against **Sigstore's trust root** (Fulcio chain, Rekor/CT log keys, TSA) and a **policy** (expected identity + issuer). Sources: Fulcio `docs/oid-info.md` and `config/identity/config.yaml` (`subject-alternative-name-template: "{{ .url }}/{{ .job_workflow_ref }}"`, `deployment-environment: "environment"`); Sigstore docs *Signing Blobs* ("The bundle contains verification metadata, including an artifact's signature, certificate and proof of transparency log inclusion"); cosign `doc/cosign_verify-blob.md`.

**Signing-time needs.** A job with `id-token: write`; `sigstore/cosign-installer`; `cosign sign-blob --yes --bundle ctm-<triple>.sigstore.json ctm-<triple>`. cosign v3.1.3 is current (2026-08-06); v3.0.0 (2025-10) made the protobuf bundle (`application/vnd.dev.sigstore.bundle.v0.3+json`) and `--trusted-root`/`--signing-config` the defaults ("Default to using the new protobuf format (#4318)"); PR #4959 (open) would make the new bundle format mandatory and require a trusted root for all verification.

**Consumer needs.** `cosign verify-blob --bundle … --certificate-identity <exact workflow URI> --certificate-oidc-issuer https://token.actions.githubusercontent.com [--trusted-root trusted_root.json]`. Verification is offline **given a current trusted root**; the root is normally fetched over TUF (cached under `~/.sigstore`). The identity pin is the workflow URI string; `--certificate-github-workflow-{ref,sha,repository,trigger,name}` exist; there is **no cosign flag** for the deployment-environment extension (a custom verifier could check it).

**Rust.** See §3. Short version: `sigstore 0.14.0` (2026-05-22, "experimental… API can change at any time") or the modular `sigstore-verify 0.11.0` (2026-07-08) — both require `aws-lc-rs`. `mise` wrote its own `mise-sigstore` crate on top of `sigstore-verify` + `rustls-webpki[aws-lc-rs]` + `x509-cert`.

**install.sh.** Impossible in POSIX sh (X.509 path validation, SCT/Rekor inclusion proofs, DSSE). Requires `cosign` (never preinstalled) or `gh`.

**Dependency weight.** CI: one action + one binary download (adds `sigstore/cosign-installer` to the release supply chain). Rust: ~40–60 crates incl. a second crypto library. Shell: hard dependency on a non-default tool.

**Maturity.** Production for OCI images and for GitHub/npm/Homebrew provenance; `sigstore-rs` still pre-1.0. Format churn is real (bundle v0.1→v0.3, cosign v2→v3 defaults, Rekor v2 shard 2025-09).

**Threat model coverage.** Asset replacement without running the workflow: **defended** (attacker cannot mint a Fulcio cert for the workflow identity). Attacker with `contents: write` who can push a tag: the workflow runs and **legitimately signs whatever that commit builds** — same as any scheme; gate it with an environment with required reviewers (the OIDC token is only minted for the job once the gate passes). Compromised build step/action: **not defended** (signature attests pipeline output, not pipeline honesty) — provenance helps forensics only. Full account compromise: **not defended** (identity is the account's). What keyless uniquely adds: **no long-lived secret to leak**, and **public transparency** (every signature under the identity is in Rekor; stealth signing is detectable). What it uniquely costs: the trust root's lifecycle belongs to Sigstore, not to ctm.

### 2b. GitHub Artifact Attestations (`actions/attest-build-provenance`)

**What it proves.** A SLSA v1 provenance statement (in-toto, DSSE) — "these subject digests were produced by workflow W at commit C on runner R" — signed keyless via the same Fulcio/Rekor path (public repos → public-good instance) and stored in GitHub's attestations API. Source: action README (needs `id-token: write`, `attestations: write`, `contents: read`; "As of version 4, this action wraps `actions/attest`"); GitHub docs *Artifact attestations* ("public repositories use the Sigstore Public Good Instance").

**Observed 2026-09-21** via `GET /repos/{owner}/{repo}/attestations/sha256:<digest>` — **unauthenticated, HTTP 200** for public repos (rate limit: 60 requests/hour/IP per GitHub REST docs):
- `cargo-binstall` and `uv` each return **two** bundles per asset: `[initiator=user]` SLSA provenance, issuer `O=sigstore.dev, CN=sigstore-intermediate`, SAN = workflow URI `…/.github/workflows/release.yml@refs/heads/main`, 1 Rekor entry; and `[initiator=github]` predicate `https://in-toto.io/attestation/release/v0.2`, issuer `O=GitHub, Inc., CN=Fulcio Intermediate l1`, SAN `https://dotcom.releases.github.com`, GitHub TSA timestamp, **no Rekor entry** — this is the immutable-release attestation (§2e).

**Consumer needs.** `gh attestation verify <file> -R owner/repo [--bundle x.jsonl --custom-trusted-root trusted_root.jsonl]` (GitHub docs *Verifying attestations offline*: "Import … GitHub CLI, your artifact, the bundle file, the trusted root file"; "generate a fresh trusted root file when importing new signed material… key rotation typically occurs a few times per year"). Without `gh`: cosign ≥ 2.4 can verify these bundles (Sigstore blog *cosign Verification of npm Provenance, GitHub Artifact Attestations…*), sigstore-python/-go, or a Rust verifier as in 2a. cargo-dist's docs state plainly: "Currently, verification of GitHub Artifact Attestations is only supported via GitHub CLI with `gh attestation verify`."

**install.sh.** Not feasible (same as 2a), and even fetching the bundle would spend the caller's 60/h unauthenticated budget.

**Threat model coverage.** Same as 2a for the workflow identity, plus SLSA provenance content (materials, builder, invocation) for audit. Adds nothing a consumer without `gh`/cosign can check.

**Verdict.** Excellent **auditor-facing** layer, ~5 lines of YAML, GitHub-maintained action, zero consumer cost. Not a consumer-verified root of trust for ctm's two consumers.

### 2c. Long-lived Ed25519 key (minisign / signify / raw / SSH signature)

**What it proves.** "Bytes B were signed by whoever holds private key K." The pin is K's public half in the installed binary and in `install.sh`. Nothing about *when* or *by which workflow*; everything about *who*. This is the same shape as ADR-018: the Developer ID private key is likewise a long-lived secret in a GitHub environment (`APPLE_DEVELOPER_ID_APPLICATION_P12_BASE64`), and the consumer pins the identity in code.

Four encodings of the same primitive, differing only in what the *shell* consumer can verify:

| Encoding | Signed message | Shell verifier | Rust verifier | Domain separation | Notes |
|---|---|---|---|---|---|
| **raw Ed25519** (`openssl pkeyutl`) | the file bytes | `openssl pkeyutl -verify -pubin -inkey pub.pem -rawin -in FILE -sigfile SIG` — **OpenSSL ≥ 3.0 only** (3.0 pod: "For EdDSA … this option is required"; 3.5+: implied; 1.1.1 pod: "Ed25519 and Ed448 … are not supported by this utility"). **LibreSSL 3.3.6 (macOS): "unable to load Public Key … unsupported algorithm"** (spike 1). | `ring::signature::ED25519` — 0 new crates | none (unless you sign a structured message) | Simplest, but the weakest shell story and no namespace. |
| **minisign** (`minisign -Vm`) | BLAKE2b-512(file) (pre-hashed mode, default since 0.8), plus a global signature over sig+trusted comment | `minisign` tool — **not preinstalled on any mainstream distro or macOS**. Without it: OpenSSL ≥ 3 + `b2sum` (coreutils ≥ 8.26) + DER-wrapping the key — feasible but two exotic tools. | `minisign-verify 0.2.5` (2026-03-03, **0 deps**, vendors its own Ed25519+BLAKE2b; used by cargo-binstall and mise) | key id only | Best Rust ergonomics if you accept a second, vendored Ed25519 implementation; worst shell ergonomics. Zig and mise publish minisign sigs. |
| **signify** (OpenBSD) | the file bytes (or a SHA256 list) | `signify` — only OpenBSD by default; `signify-openbsd` on Debian **[unverified: package presence per distro]** | `ring` (format is 2-byte alg + 8-byte keynum + 64-byte sig, base64) | key num only | Same as raw with a header; no advantage over sshsig on Linux. |
| **SSH signature** (`ssh-keygen -Y sign/verify`, `PROTOCOL.sshsig`) | `"SSHSIG" ‖ string namespace ‖ string reserved ‖ string "sha512" ‖ string SHA-512(file)` | **`ssh-keygen -Y verify`** — OpenSSH ≥ 8.1 (2019-10). Present on macOS 11+ (Big Sur ships OpenSSH 8.1p1; this machine: OpenSSH 10.3p1), Debian (`openssh-client` is Priority: standard → in the default "standard system utilities"), Ubuntu 22.04 (8.9p1), Fedora **[unverified: comps membership]**, Alpine (separate `openssh-keygen` package). **Not** on RHEL/Rocky/Alma 8 (`openssh-clients-8.0p1`, and its OpenSSL is 1.1.1k — so *neither* shell verifier works on EL8). Also verifiable with **OpenSSL ≥ 3 alone** (spike 2, 40-line POSIX script). | `ring` + `sha2::Sha512` + `base64` — 0 new crates, 68 LOC (spike 4); or `ssh-key 0.6.7` crate (`ed25519` feature → `ed25519-dalek` + `curve25519-dalek` etc., ~10 crates, second Ed25519 impl) | **namespace** (mandatory, non-empty; "prevents cross-protocol attacks caused by signatures intended for one intended domain being accepted in another") + principal in `allowed_signers` | Reference implementation ships in OpenSSH; the same format GitHub verifies for SSH-signed commits; key generation, encryption, agent support, fingerprints all standard. |

**Signing-time needs (all four).** The private key as an environment secret (base64 of the key file) + optional passphrase secret. Spike 3 confirmed `ssh-keygen -Y sign -f key -P '<passphrase>' -n <ns> FILE` works non-interactively with an **encrypted** key (wrong passphrase → exit 255 "incorrect passphrase supplied") — i.e. the two-secret pattern of the Apple P12 + password carries over unchanged. `ssh-keygen -Y sign` also reads stdin and writes the armored signature to stdout.

**Threat model coverage.** Asset replacement: **defended** (no key, no signature). Tag push by a leaked write token: the signing job runs only in the environment; with **required reviewers**, the secret is withheld until a human approves (the same gate protects the Apple key today — currently not configured). Compromised build step: **not defended** (as with every option; the signer signs what the receipt says). Full account compromise: **not defended** if the key is on GitHub; **defended** if signing (or at least a standby key) is offline — see §5. Transparency: none by itself; add 2b/2e for the public record.

**Verdict.** The only family both consumers can verify with what they already have. Among encodings, **sshsig** dominates: identical Rust cost to raw Ed25519, strictly better shell coverage (ssh-keygen *or* OpenSSL 3), built-in namespace and principal binding, and tooling every maintainer already knows.

### 2d. Other

- **OpenPGP / gpg — reject.** `gpg` is absent on macOS and minimal Linux; keyring/trust-model UX defeats fail-closed installers; the format is large and verification in Rust means `sequoia-openpgp` or `pgp` (dozens of crates). The Rust project removed rustup's experimental GPG verification in 1.26.0: "validating the integrity of downloaded binaries did not rely on it, and there was no option to abort the installation if a signature mismatch happened. Multiple problems with its implementation were discovered." `mise` still publishes GPG signatures but its own installer does not verify them (`# TODO: verify with minisign or gpg if available`).
- **SSH signatures — taken seriously above (2c).** Evaluated as the recommended encoding.
- **FIDO/hardware-backed SSH keys (`sk-ssh-ed25519@openssh.com`).** `ssh-keygen -Y sign` supports them, but the signature blob and signed data differ (application hash, flags, counter), which would complicate the Rust verifier. Recommended only for the *offline standby* if ever wanted, and then the verifier must grow the sk- variant. Default: plain `ssh-ed25519`, verifier refuses every other key type.

### 2e. GitHub immutable releases + release attestation (platform layer, not a signature you pin)

GitHub changelog 2025-10-28 (GA): "Once you publish a release as immutable, its assets can't be added, modified, or deleted. Tags for new immutable releases are protected and can't be deleted or moved." Docs: title/notes/latest-flag remain editable; deleting an immutable release is possible but "you cannot reuse the same tag name"; drafts are exempt until published (recommended flow: draft → attach → publish). Every immutable release gets a release attestation (`in-toto release/v0.2`, signed by GitHub's own Fulcio/TSA) verifiable with `gh release verify TAG` / `gh release verify-asset TAG FILE`. Enable per repository or organization. **This directly removes the "replace assets on an existing Release" move at the platform level** and leaves the attacker only "publish a new release", which the pinned signature then refuses. Trust root is GitHub itself, so it is not independent of a GitHub compromise — which is exactly why it complements, rather than replaces, the pinned key.

Note for `softprops/action-gh-release@v2`: it creates a non-draft release with `files:` in one step. With immutability on, the release must be complete at publish time; the current single-step upload is compatible, but a *re-run* of `publish` on the same tag would fail rather than overwrite — desirable.

---

## 3. Rust consumer side (`ctm update`)

| Option | Crates | New crates vs. today | Second crypto stack? | Network at verify time | Binary size delta (estimate) | Maturity / notes |
|---|---|---|---|---|---|---|
| sshsig via `ring` (hand-rolled parser) | `ring` (direct dep, already linked), `sha2` (add `Sha512`), `base64` (promote transitive → direct) | **0** | no | none | tens of KB (Ed25519 verify + SHA-512 already-compiled paths) | Spike 4: 68 LOC, four negative cases correct; fixture generated by `ssh-keygen`. `ring 0.17.14` last released 2025-03-11; if reqwest ever moves to `aws-lc-rs` (reqwest 0.13 has an `__rustls-aws-lc-rs` feature), `aws_lc_rs::signature::ED25519` has the same API — a one-line change. |
| sshsig via `ssh-key 0.6.7` | `ssh-key[ed25519]` → `ed25519-dalek`, `curve25519-dalek`, `signature`, `ssh-encoding`, `ssh-cipher`, … | ~8–12 | second Ed25519 implementation (dalek beside ring) | none | ~200–400 KB **[estimate]** | Mature (RustCrypto); `SshSig` type does the envelope for you; 0.7.0-rc.11 in flight. Not worth 10 crates to save 40 lines. |
| raw Ed25519 via `ring` | as row 1 minus `Sha512` | 0 | no | none | negligible | Trivial; but loses namespace and the `ssh-keygen` shell verifier. |
| minisign via `minisign-verify 0.2.5` | 1 crate, **0 deps** (vendored Ed25519 + BLAKE2b) | 1 | yes (a third Ed25519 implementation in the process, unaudited relative to ring) | none | ~50 KB **[estimate]** | Used by cargo-binstall (`binstalk-fetchers`) and mise. Fine in Rust; no shell verifier. |
| Sigstore bundle via `sigstore 0.14.0` | 23 required deps incl. `aws-lc-rs`, `tough` (TUF), `x509-cert`, `rustls-webpki`, `tokio`, `pem`, `pkcs8`, … | ~40–60 **[estimate]** | **yes — `aws-lc-rs` is non-optional**; C/asm build, cmake/bindgen risk on the cross-built aarch64 job | none *if* a trust root is pinned; TUF fetch otherwise | multiple MB **[estimate]** | "experimental… API can change at any time"; attestation verification "not yet implemented" per README. |
| Sigstore bundle via `sigstore-verify 0.11.0` (modular rewrite, what `mise-sigstore` builds on) | 22 required deps incl. `sigstore-crypto` (→ `aws-lc-rs`), `rustls-webpki[aws-lc-rs]`, `x509-cert[builder,sct]`, `cms`, `sigstore-rekor`, `sigstore-tsa`, `sigstore-trust-root`, `jiff` | ~40 **[estimate]** | **yes** | same as above | same | Released 2026-07-08 alongside `sigstore-bundle 0.11.0` (bundle v0.1–v0.3 parsing). Supports an embedded trusted root for an "offline path", per its README. |
| GitHub attestation bundle | same as Sigstore rows + DSSE/in-toto parsing + `reqwest` call to `api.github.com/repos/…/attestations/sha256:…` (unauthenticated OK, 60/h/IP) | same | same | **one extra API call** (or publish the bundle as a release asset) | same | The release-attestation bundle (GitHub Fulcio) needs GitHub's trusted root, which differs from the public-good one — two roots to pin. |

**The trust-root problem, concretely.** For Sigstore options the verifier needs Fulcio's chain and the log/TSA keys with validity windows. Pinning today's `trusted_root.json` (6,787 bytes from `sigstore/root-signing`) into ctm means: the day Sigstore adds a shard/CA/TSA (it did in 2022, 2025-07, 2025-09), signatures made with the new material fail in every installed ctm, and `ctm update` — the fix — is what fails. The only fixes are a TUF client in the updater (network + `tough`/`sigstore-tuf`, more crates) or a "fetch the root from a URL" step that reintroduces the trust-on-download problem. A maintainer-owned Ed25519 pin has no such external clock: rotation happens when ctm decides, through ctm's own release.

Nothing in the recommended path pulls OpenSSL, LibreSSL, or a second TLS stack.

---

## 4. `install.sh` consumer side (POSIX sh, curl)

| Option | Tool needed | Present by default? | Honest statement when absent |
|---|---|---|---|
| sshsig via `ssh-keygen -Y verify` | OpenSSH ≥ 8.1 | macOS 11+: yes. Debian/Ubuntu default installs: yes (Priority: standard; Ubuntu 22.04 = 8.9p1). Alpine: `apk add openssh-keygen`. **EL8: no** (8.0p1). Minimal containers (`debian:*-slim`, `ubuntu`, `alpine`): no — but they also lack `curl` until someone installs packages. | "ctm install: cannot verify the release signature: `ssh-keygen` (OpenSSH ≥ 8.1) not found. Install `openssh-client` (Debian/Ubuntu), `openssh-clients` (Fedora/RHEL 9+), or `openssh-keygen` (Alpine) and re-run." **Fail closed.** |
| sshsig via OpenSSL ≥ 3 (spike 2 script) | `openssl` 3.x + `dd`/`od`/`awk` (POSIX) | Ubuntu 22.04+ (3.0.2), Debian 12+, Fedora 36+, RHEL 9+, Alpine 3.17+ **[versions from memory except Ubuntu 22.04 and Rocky 8, which were checked]**; macOS: **no** (LibreSSL). | Second verifier for the *same* asset; adds ~40 lines of byte-slicing to a security-critical script. Optional. Covers "has openssl 3 but removed openssh-client" — a small population. |
| raw Ed25519 via OpenSSL ≥ 3 | as above | as above | Same coverage as the row above but *worse* than sshsig because it cannot use ssh-keygen at all. Dominated. |
| minisign | `minisign` | never | Would need "please install minisign", a tool nobody has. |
| Sigstore / GitHub attestations | `cosign` or `gh` | never (`gh` is common on developer machines, absent on servers) | Cannot be fail-closed without making `gh` a hard prerequisite of a `curl | sh` installer. |
| gpg | `gpg` | Linux desktops usually; macOS never | Rejected in §2d. |

**Is fail-closed viable for Linux installs?** Yes, with the sshsig choice. The ADR-018 stance works on macOS because `codesign` is always there; on Linux the analogous "always there" tool does not exist, but `ssh-keygen` is the closest thing (it ships with the SSH client every server operator has). The UX cost is one package install on EL8 or a slim container — the same class of message the script already prints for a missing `curl` or `sha256sum`. There must be **no `CTM_INSTALL_SKIP_SIGNATURE`** escape hatch (the contract test should grep for its absence, as the darwin test greps for `--sign -`). Pure-sh Ed25519 without any tool is not realistic and is not proposed.

Both consumers must fetch the signature **from the versioned URL** (`releases/download/v<version>/ctm-<triple>.sshsig`), bounded (`--max-filesize 4096`; Rust `MAX_SIG_BYTES`), origin-checked, and verify **before** `chmod`/`publish`. The record stays byte-identical to today.

---

## 5. Prior art in shipped CLIs (verified 2026-09-21 against the repositories)

| Project | Signs Linux binaries how | Self-updater verifies? | Installer verifies? | Source |
|---|---|---|---|---|
| **uv / ruff** (astral) | cargo-dist 0.32.0 with `github-attestations = true` → `actions/attest@v4` on `*.json *.sh *.ps1 *.zip *.tar.gz` (announce phase). macOS/Windows signed via Azure Key Vault + `rcodesign` in a `release` environment. | `uv self update` uses `axoupdater`, which runs the dist installer → **sha256 only** (no attestation check in-process). | `install.sh` carries per-target **sha256 pinned in the script** (the script itself is attested and served from astral.sh, a second origin). | `dist-workspace.toml`, `.github/workflows/{release,sign-release-binaries}.yml`, `crates/uv/src/commands/self_update.rs`, `https://astral.sh/uv/install.sh` |
| **mise** | Publishes `minisign` + GPG signatures, `zipsign` (Ed25519) on archives, `actions/attest@v4` provenance, a `packslip.sigstore.json` inventory, macOS Developer ID signing. | **Yes**: `self_update` crate with `.verifying_keys([*include_bytes!("../../zipsign.pub")])` (Ed25519, key embedded in the binary); on macOS additionally `codesign --verify … -R=identifier "dev.jdx.mise"` (but "skipping" with a warning if `codesign` missing — weaker than ctm). For *tools it installs*: native Rust cosign/SLSA/attestation/minisign verification (`mise-sigstore`, `minisign-verify`). | `https://mise.run`: **sha256 only**; literally `# TODO: verify with minisign or gpg if available`. | `src/cli/self_update.rs`, `.github/workflows/release.yml`, `SECURITY.md`, `Cargo.toml` |
| **cargo-binstall** | **Just-in-time minisign**: `.github/scripts/ephemeral-gen.sh` creates a per-release keypair, `ephemeral-sign.sh` signs, the public key is written into `Cargo.toml` and published to crates.io (immutable → the anchor), plus `actions/attest@v4.2.2`. | Binstall verifies minisign for any crate declaring `[package.metadata.binstall.signing]` (`minisign-verify 0.2.1`); "The legacy signature format is not supported." | `install-from-binstall-release.sh`: **no verification** (TLS only). | `SIGNING.md`, `.github/workflows/release-cli.yml`, `Cargo.lock` |
| **rustup** | Nothing. Docs: "rustup performs all downloads over HTTPS, but does not yet validate signatures of downloads." Experimental GPG validation **removed in 1.26.0 (2023-04-25)**. | No. | `rustup-init.sh`: no checksum. | `doc/user-guide/src/security.md`, blog *Announcing Rustup 1.26.0* |
| **deno** | Nothing. | `deno upgrade`: `verify_checksum` only when the user passes `--checksum`. | `install.sh`: nothing. | `cli/tools/upgrade.rs` |
| **zig** (not Rust) | Publishes minisign signatures; public key `RWSGOq2NVecA2UPNdBUZykf1CCb147pkmdtYxgb3Ti+JO/wCYvhbAb/U` on the download page. | No self-updater. | Manual. | `https://ziglang.org/download/` |
| **just** | `SHA256SUMS` on the release. | No self-updater. | `https://just.systems/install.sh`: no verification. | `.github/workflows/release.yaml` |
| **starship** | Windows: SignPath; macOS: Developer ID + notarization + pkg. Linux: nothing. | No self-updater. | `install.sh`: no checksum. | `.github/workflows/release.yml` |

Takeaways: (1) in-updater signature verification with an embedded public key is shipped and proven (mise, via `self_update`/`zipsign`); (2) nobody verifies Sigstore inside a self-updater or a `curl | sh` script; (3) attestations are universally treated as an auditor layer; (4) uv's trick — sha256 pinned inside an installer served from a *different origin* than the binaries — is worth noting but does not apply to ctm (install.sh and the assets are both under the same GitHub account).

---

## 6. Key management for the long-lived key

**Generation** (on the maintainer's machine, never in CI):
```
ssh-keygen -t ed25519 -a 100 -C "ctm release signing 2026" -f ctm-release-signing-ed25519      # primary, passphrase-protected
ssh-keygen -t ed25519 -a 100 -C "ctm release signing standby 2026" -f ctm-release-standby-ed25519
```
Record both fingerprints (`ssh-keygen -lf …pub`, e.g. `SHA256:o35NoWq8…`). Pin **both** public keys from the first signed release.

**Storage.** Environment `release-signing` (deployment tags `v*`, **required reviewer: owner**, "prevent self-review" not applicable to a solo owner but enable "disallow bypassing"): secrets `CTM_RELEASE_SIGNING_KEY_BASE64` (`base64 < key`), `CTM_RELEASE_SIGNING_KEY_PASSPHRASE`; variable `CTM_RELEASE_SIGNING_PUBLIC_KEY` (the `ssh-ed25519 AAAA…` line, used only for the pre-sign pin check like `APPLE_TEAM_ID` today). Environment secrets "are only available to workflow jobs that use the environment… can only access these secrets after any configured rules (for example, required reviewers) pass" (GitHub docs). The **standby private key never touches GitHub**: encrypted at rest in two places the owner controls (e.g. `~/.private_keys/` as with the Apple material, plus an offline copy).

**Rotation (planned).** Release N: add key C's public half to the pin sets, still sign with A. Release N+1…: sign with C; pins `[C, B(standby)]`. Clients that skipped release N (they only knew A) fail closed at N+1 and recover by re-running `install.sh` — the same recovery ADR-018 documented for 0.2.44→0.2.45. Keep A pinned for a documented window (e.g. two minor releases or 90 days) before dropping it; nothing in the record changes.

**Rotation (emergency / leak).** Cut a release **immediately**, signed with the offline standby B; pins `[B, D(new standby)]`; remove A. Every installed client already trusts B, so nobody is stranded — this is the whole reason for pre-pinning a standby. Releases signed by A before the leak remain valid to old clients that only trust A (a downgrade/freeze residual shared by every pinned-key scheme, including Apple's outside of revocation); immutable releases prevent the attacker from *editing* those old releases. Also rotate the environment secrets and audit the Actions run log for the window in which A was reachable.

**Comparison with Sigstore short-lived certs.** Sigstore removes the key-custody problem entirely (nothing to leak; identity = workflow) and gives transparency, at the price of external trust-root rotation, a second crypto stack, and no shell verifier. ADR-018 already accepted long-lived custody in a protected environment for Apple; the Ed25519 key is the same risk class, and the offline standby gives the recovery Apple's revocation would give.

**Certificate/key expiry.** Ed25519 keys do not expire; `allowed_signers` supports `valid-after=`/`valid-before=` but those are evaluated against *verification* time (signatures carry no trusted timestamp), so they must not be used for the pin — rotation is by release, as above.

---

## 7. Recommendation — one concrete design

### 7.1 Decision

Sign the **final bytes** of every release binary (both Linux and both darwin targets, the darwin ones *after* Apple signing) with a long-lived **Ed25519** key in **OpenSSH signature format** (`PROTOCOL.sshsig`, key type `ssh-ed25519`, hash `sha512`, namespace pinned), published as `ctm-<triple>.sshsig`. Pin the public key(s) in `rust-crates/ctm/src/release_trust.rs` and `install.sh`. Verify fail-closed in `ctm update` (ring) on every target and in `install.sh` (`ssh-keygen -Y verify`) on every target. Enable **immutable releases**. Add **`actions/attest-build-provenance`** for auditors. Keep `apple_trust.rs` exactly as is — on macOS a candidate must satisfy Apple *and* the Ed25519 pin.

Suggested pin strings (choose once; the contract test pins them across three files): namespace `ctm.release` (ssh-keygen recommends a `NAME@DOMAIN` style, e.g. `release@ctm.cli` — either works; must be non-empty and never reused for another purpose), principal `release@ctm.cli`.

### 7.2 Pipeline (`release.yml`)

- **New job `sign-release`**: `needs: [build-linux, sign-darwin]`, `runs-on: ubuntu-latest`, `environment: release-signing`, `timeout-minutes: 15`, `actions/checkout` with `persist-credentials: false`, then:
  1. `test "$(git rev-parse HEAD)" = "$GITHUB_SHA"`; `git diff --exit-code`.
  2. Download `release-*` artifacts (the four final binaries + records).
  3. For each target: assert the record is `ctm.standalone-release` for this version/target, `size` and `sha256` match the bytes, key set is the frozen eight. (The unsigned Linux candidate is data: never executed; the contract test greps for that as the darwin test does.)
  4. Materialize the key into `$RUNNER_TEMP/ctm-release-signing-secrets/key` (0600), derive its public half with `ssh-keygen -y -f key -P "$PASS"` and **assert it equals** `vars.CTM_RELEASE_SIGNING_PUBLIC_KEY` **and** appears verbatim in `release_trust.rs` and `install.sh` (mirrors the `grep -q "pub const TEAM_ID…"` step in `sign-darwin`). Refuse otherwise — "or every consumer would refuse the release we are about to cut."
  5. `ssh-keygen -Y sign -f key -P "$PASS" -n ctm.release "ctm-$TARGET"` → `ctm-$TARGET.sshsig` (rename from the default `.sig`). Then self-verify with an `allowed_signers` built **from the pin in install.sh**, not from the key: `ssh-keygen -Y verify -f allowed -I release@ctm.cli -n ctm.release -s ctm-$TARGET.sshsig < ctm-$TARGET`.
  6. Write `sigproof-<target>.json` (`kind: ctm.release-signature-proof`, `schema_version: 1`, `source_sha`, `version`, `target`, `asset: {size, sha256}`, `signature: {format: "sshsig", namespace, key_type: "ssh-ed25519", fingerprint, hash: "sha512"}`, `verification: {ssh_keygen_output}`) — the audit trail, like `proof-<target>.json`. Shred the secret directory.
  7. Upload `signed-<target>` artifacts (binary, record, `.sshsig`, sigproof).
  Put the signing logic in `scripts/sign-release.sh INPUT OUT VERSION SHA TARGET NAMESPACE PRINCIPAL` so it has a contract test and refusals (no credentials → refuse by name before touching anything; non-canonical namespace → refuse; public key ≠ pin → refuse).
- **`publish`**: `needs: [sign-release]`, download `signed-*` (pattern change so unsigned artifacts cannot be picked up), keep every existing check, and add for all four targets: `ssh-keygen -Y verify` against an `allowed_signers` regenerated from the **install.sh** pin (independent of `sign-release`), `ssh-keygen -Y find-principals` == the principal, sigproof `asset.sha256 == record.sha256`, `source_sha == GITHUB_SHA`. Upload `ctm-<triple>.sshsig` and `sigproof-<triple>.json` alongside the existing assets. Add `actions/attest-build-provenance@v4` with `subject-path: release/ctm-*` (permissions `id-token: write`, `attestations: write`) — auditors get `gh attestation verify ctm-x86_64-unknown-linux-gnu -R robertelee78/claude-telegram-mirror`.
- **Repository setting**: enable immutable releases before the first signed release; from then on `gh release verify v0.2.47` also works.
- **Environment `release-signing`** as in §6; and add the same **required reviewer** to `apple-release` (finding 6).
- While there: pin third-party actions by SHA (`actions/checkout@<sha>`, `Swatinem/rust-cache@<sha>`, `dtolnay/rust-toolchain@<sha>`, `softprops/action-gh-release@<sha>`) — the compromised-build-step threat is the one no signature addresses, and mutable tags are the cheapest way in.

### 7.3 Rust consumer (`rust-crates/ctm`)

- `Cargo.toml`: `ring = "0.17"` (already compiled), `base64 = "0.22"` (already compiled). No other change.
- New `src/release_trust.rs` (sibling of `apple_trust.rs`, < 300 lines with tests):
  - `pub const SIGNING_KEYS: &[&str] = &["ssh-ed25519 AAAA… (primary)", "ssh-ed25519 AAAA… (standby)"];` `pub const NAMESPACE: &str = "ctm.release";` `const MAX_SIG_BYTES: usize = 4096;`
  - `pub fn parse_sshsig(armored: &str) -> Result<SshSig>` — strict: header/footer, base64, `SSHSIG`, version 1 only, key type `ssh-ed25519` only, 32-byte key, non-empty namespace, `sha512`, 64-byte sig, no trailing bytes (spike 4 is the reference).
  - `pub fn verify_release_candidate(path: &Path, sig: &[u8]) -> Result<SignerIdentity>` — SHA-512 of the file, rebuild the signed blob, `ring::signature::UnparsedPublicKey::new(&ED25519, pk).verify(...)`, key must equal one of `SIGNING_KEYS`, namespace must equal `NAMESPACE`. Returns the fingerprint (`SHA256:` base64 of SHA-256 of the key blob) for the "verified: release key SHA256:…" line.
  - Tests: a fixture triple (`tests/fixtures/release-sig/{blob,blob.sshsig,key.pub}`) produced by `ssh-keygen`, plus every load-bearing property dropped/altered (tampered bytes, other namespace, other key, version 2, `sk-ssh-ed25519@openssh.com`, `sha256` hash alg, trailing bytes, truncated) — the `every_required_property_is_load_bearing` pattern from `apple_trust.rs`.
- `update.rs`: `sig_url(version, target)`; `download_signature` (bounded, origin-checked); in `run_update`, after `download_asset` and **before** `verify_apple_signature`: `verify_release_signature(&candidate, &sig)` on every OS, removing the candidate on failure. Optionally store the signature beside the binary (`.ctm-signature`) so `ctm doctor` can re-verify the running binary offline on Linux (today check 13 has nothing to say off macOS).
- Old clients (≤ 0.2.46) ignore the new assets; the record is unchanged, so the first signed release remains installable by every client in the field — the lesson of the 0.2.45 amendment.

### 7.4 `install.sh`

- Pins: `RELEASE_SIGNING_KEYS="ssh-ed25519 AAAA… ssh-ed25519 AAAA…"` (space-separated), `RELEASE_NAMESPACE="ctm.release"`, `RELEASE_PRINCIPAL="release@ctm.cli"`.
- Tools block: `command -v ssh-keygen || fail "…install openssh-client / openssh-clients / openssh-keygen…"`; also require `ssh-keygen -Y` support by probing `ssh-keygen -Y check-novalidate` usage (exit status/usage text) so an OpenSSH 8.0 host fails with a clear message instead of a parse error.
- After the sha256 check: `curl … --max-filesize 4096 -o "$tmp/$ASSET.sshsig" "${BASE}/download/v${version}/${ASSET}.sshsig"`, origin check, write `allowed_signers` (one line per pinned key: `$RELEASE_PRINCIPAL namespaces="$RELEASE_NAMESPACE" $key`), then `ssh-keygen -Y verify -f "$tmp/allowed_signers" -I "$RELEASE_PRINCIPAL" -n "$RELEASE_NAMESPACE" -s "$tmp/$ASSET.sshsig" < "$tmp/$ASSET" || fail "release signature verification failed"`; print `verified: release key SHA256:… (namespace ctm.release)`. Runs on **both** OSes, before the Darwin block. No skip variable.
- Optional (phase 2, only if EL8-with-OpenSSL-3-but-no-ssh telemetry ever justifies it): the spike-2 OpenSSL path as a second verifier of the same `.sshsig`.

### 7.5 Contract test — `scripts/test-release-signing-contract.sh` (mirrors the darwin one)

Static, runs on Linux and macOS in CI:
- `sh -n install.sh`, `bash -n scripts/sign-release.sh`.
- Pins agree: every `ssh-ed25519` line in `release_trust.rs::SIGNING_KEYS` appears in `install.sh`'s `RELEASE_SIGNING_KEYS` and vice-versa; namespace and principal identical; `release.yml` greps each pin before signing (`grep -q … release_trust.rs`, `… install.sh`).
- No fallback: `! grep -q CTM_INSTALL_SKIP_SIGNATURE install.sh`; `! grep -q continue-on-error release.yml`; `! grep -qi 'skip.*signature' update.rs`.
- Jobs: `build-linux` references no `secrets.` and no `environment:`; `sign-release` has `environment: release-signing`, `persist-credentials: false`, the source-identity check, references `scripts/sign-release.sh`, `secrets.CTM_RELEASE_SIGNING_KEY_BASE64`, `secrets.CTM_RELEASE_SIGNING_KEY_PASSPHRASE`, `vars.CTM_RELEASE_SIGNING_PUBLIC_KEY`, and never executes a candidate (`"$candidate"` as a command / in `$(…)`); `publish` `needs: [sign-release]`, `pattern: signed-*`, contains `ssh-keygen -Y verify`, `find-principals`, the frozen-record-keys assertion, and the attest step.
- `install.sh` contains `ssh-keygen -Y verify`, `namespaces=`, `--max-filesize 4096` for the signature, verifies before the Darwin block.
Functional (anywhere with ssh-keygen; no secrets):
- Throwaway key → sign a blob → `scripts/sign-release.sh` refuses: no credentials (by name, before creating any directory), wrong target, key ≠ pin; a verify function extracted from `install.sh` (or `install.sh` against a loopback stand-in, as ADR-018's proof did) accepts the good triple and refuses tampered bytes, wrong namespace, wrong key; `cargo test -p ctm release_trust` passes with the fixtures.

### 7.6 Also sign macOS? — Yes

Reasons: one contract for every asset (every `ctm-<triple>` has a `.sshsig`; one verifier in Rust, one in sh; one proof shape); the darwin binaries gain a signature Apple cannot revoke or refuse (a CA-side incident, a notary outage, or a future Apple policy change no longer blocks `ctm update` *verification* — though ADR-018's online notarization check would still fail closed on its own); and the signing job becomes the natural single place after `sign-darwin`. Cost: macOS updates require both checks — a key-rotation mistake would break macOS too. That is acceptable because the contract test pins the key in three files and the signing job refuses to sign with a key the consumers do not pin. **`apple_trust.rs` and the Apple pins do not change.** Order on macOS: sha256 → Ed25519 → Apple → publish.

### 7.7 What was rejected, and why

| Rejected | Why |
|---|---|
| Sigstore keyless as the consumer-verified layer | second crypto stack (`aws-lc-rs`) in a 10 MB binary; external trust-root rotation breaks the self-updater; impossible in POSIX sh; identity is rooted in the GitHub account anyway; pre-1.0 Rust crates; cosign/gh required for humans. **Adopted instead** as an auditor layer via `actions/attest-build-provenance` (same Fulcio/Rekor evidence, no consumer cost). |
| GitHub attestations as the consumer-verified layer | same as above plus `gh` dependency and a 60/h unauthenticated API budget for `install.sh`. |
| minisign | best Rust ergonomics, but no shell verifier without a tool nobody has; a third Ed25519 implementation in-process. |
| raw Ed25519 via `openssl pkeyutl` | OpenSSL ≥ 3.0 only; LibreSSL/macOS cannot; no namespace; strictly dominated by sshsig, which OpenSSL 3 can *also* verify (spike 2). |
| OpenPGP | tooling absent on macOS/minimal Linux, keyring UX, heavy Rust deps; the Rust project removed it from rustup in 2023 for exactly these reasons. |
| Just-in-time (per-release) keys à la cargo-binstall | needs an immutable third-party anchor (crates.io) for the public key; ctm retired npm and has no such registry; the installed binary's pin *is* ctm's anchor, which requires a stable key. |
| Signing the record instead of the binary | equivalent security, but the binary signature is what `ssh-keygen -Y verify < file` and every auditor expects, and it keeps darwin (whose bytes are fixed by Apple's signature) symmetric. |
| Hardware-backed (sk-) signing key in CI | not possible in a hosted runner; kept as a possible future for the offline standby only. |

### 7.8 Residual risks (stated, not solved here)

- **Compromised build step / third-party action**: the signer signs pipeline output. Mitigate with SHA-pinned actions, `permissions: {}` at workflow level, and (later) a second independent build whose sha256 must agree before signing.
- **Freeze / downgrade**: an attacker who can only *publish* could re-serve an older validly-signed release as `latest`; `ctm update` refuses downgrades by SemVer, `install.sh` would install the old version. Immutable releases + a future signed `not_after` in a new asset would close it.
- **Full owner-account compromise**: every option that keeps signing on GitHub fails; only the offline standby (and any offline-signed release) survives. That is the strongest honest statement available for a solo maintainer, and it is strictly better than the darwin scheme today, whose only key is on GitHub.

---

## Appendix A — Spikes (all in `scratchpad/spike/`, executed 2026-09-21 on macOS 27 / OpenSSH 10.3p1 / LibreSSL 3.3.6 / Homebrew OpenSSL 3.6.4 / dash)

1. **OpenSSL vs LibreSSL raw Ed25519.** OpenSSL 3.6.4: `pkeyutl -sign/-verify … -rawin` OK; tampered → "Signature Verification Failure", exit 1. `/usr/bin/openssl` (LibreSSL 3.3.6): `genpkey -algorithm ed25519` → "Algorithm ed25519 not found"; `pkeyutl -verify -pubin` → "unable to load Public Key … unsupported algorithm"; no `-rawin` in its usage.
2. **sshsig verified by POSIX sh + OpenSSL 3 only** (`verify-sshsig-openssl.sh`, 40 lines: `sed`/`tr`/`dd`/`od`/`awk`/`printf`, `openssl base64|dgst -sha512|pkeyutl -verify -keyform DER -rawin`, DER prefix `302a300506032b6570032100`): good → exit 0; tampered → exit 1; wrong namespace → "namespace mismatch"; wrong key → "signature is by a different key"; runs under `dash`.
3. **`ssh-keygen -Y`**: sign/verify/find-principals/check-novalidate all as expected; tampered → "incorrect signature" exit 255; wrong namespace → "namespace does not match"; wrong principal → refused; **encrypted key + `-P '<passphrase>'` signs non-interactively**; wrong passphrase → "incorrect passphrase supplied", exit 255; sign from stdin to stdout works.
4. **Rust sshsig verifier with `ring 0.17` + `sha2 0.10` + `base64 0.22` only** (`sshsig-rs/src/main.rs`, 68 lines): good/tampered/wrong-ns/wrong-key all correct; `cargo tree` = 15 crates, every one already in ctm's tree.

## Appendix B — Sources (fetched 2026-09-21)

- ctm: `.github/workflows/release.yml`, `docs/adr/ADR-018-…md`, `rust-crates/ctm/src/{apple_trust,update}.rs`, `install.sh`, `scripts/test-darwin-signing-contract.sh`, `cargo tree -p ctm`; GitHub API: `repos/robertelee78/claude-telegram-mirror` (`immutable_releases: null`), `/releases/latest` (`immutable: false`, asset digests), `/environments` (`apple-release`: 0 reviewers), `/attestations/sha256:…` (404).
- OpenSSH: `PROTOCOL.sshsig` (rev 1.4, 2020-08-31) — https://raw.githubusercontent.com/openssh/openssh-portable/master/PROTOCOL.sshsig ; `ssh-keygen(1)` local man page (`-Y sign/verify`, `hashalg` default sha512, ALLOWED SIGNERS `namespaces=`, `valid-after/before`).
- OpenSSL: `doc/man1/openssl-pkeyutl.pod.in` (master; HISTORY: "-rawin … no longer required [since 3.5] … Ed25519"), `openssl-3.0` branch ("For EdDSA … this option is required"), `OpenSSL_1_1_1-stable` `pkeyutl.pod` ("Ed25519 and Ed448 … not supported by this utility").
- Sigstore: https://docs.sigstore.dev/cosign/signing/signing_with_blobs/ ; https://docs.sigstore.dev/cosign/verifying/verify/ ; cosign `CHANGELOG.md` v3.0.0; cosign `doc/cosign_verify-blob.md`; cosign releases (v3.1.3 2026-08-06); cosign PR #4959 (open); Fulcio `docs/oid-info.md` (1.3.6.1.4.1.57264.1.8–.24 incl. .23 Deployment Environment) and `config/identity/config.yaml`; `sigstore/root-signing` `targets/trusted_root.json`; blog *Verifying Sigstore Bundles as an End User* (2025-05-02); blog *cosign Verification of npm Provenance, GitHub Artifact Attestations…*.
- GitHub: *Artifact attestations* concept page; *Verifying attestations offline*; `actions/attest-build-provenance` README; REST *Attestations* and *Rate limits* ("60 requests per hour" unauthenticated); *Manage environments* (secrets gated by rules; free plan = public repos only); changelog *Immutable releases are now generally available* (2025-10-28); docs *Immutable releases* and *Verifying the integrity of a release* (`gh release verify`, `gh release verify-asset`); community discussion #171210; cargo-dist book *GitHub Artifact Attestations* ("only supported via GitHub CLI").
- crates.io API (versions/dates/deps): `sigstore 0.14.0` (2026-05-22), `sigstore-verification 0.2.8`, `sigstore-bundle 0.11.0`, `sigstore-verify 0.11.0`, `sigstore-crypto 0.11.0`, `sigstore-trust-root 0.11.0` (2026-07-08), `ssh-key 0.6.7` / `0.7.0-rc.11`, `minisign-verify 0.2.5` (2026-03-03), `ed25519-dalek 3.0.0`, `ring 0.17.14` (2025-03-11), `zipsign-api 0.2.1`, `self_update 1.3.0`, `reqwest 0.13.5`; local `reqwest-0.12.28/Cargo.toml` (`rustls-tls` → `__rustls-ring`).
- Prior art: astral-sh/uv (`dist-workspace.toml`, `release.yml`, `sign-release-binaries.yml`, `self_update.rs`, `https://astral.sh/uv/install.sh`); jdx/mise (`src/cli/self_update.rs`, `release.yml`, `SECURITY.md`, `Cargo.toml`, `crates/mise-sigstore/Cargo.toml`, `https://mise.run`); cargo-bins/cargo-binstall (`SIGNING.md`, `release-cli.yml`, `Cargo.lock`, `install-from-binstall-release.sh`); rust-lang/rustup (`doc/user-guide/src/security.md`, blog 2023-04-25 *Announcing Rustup 1.26.0*, `https://sh.rustup.rs`); denoland/deno `cli/tools/upgrade.rs`, `https://deno.land/install.sh`; casey/just `release.yaml`, `https://just.systems/install.sh`; starship `release.yml`, `https://starship.rs/install.sh`; `https://ziglang.org/download/`.
- Distributions: Debian `openssh` source `debian/control` (Priority: standard); Rocky 8 BaseOS index (`openssh-clients-8.0p1-24.el8`, `openssl-1.1.1k-12.el8_9`); packages.ubuntu.com jammy (`openssl 3.0.2`, `openssh-client 8.9p1`); pkgs.alpinelinux.org `openssh-keygen`; macOS Big Sur = OpenSSH 8.1p1 (web search; consistent with ctm's `minos 11.0`).
