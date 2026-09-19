# ADR-017: GitHub Releases distribution, one-line install, and `ctm update`

> **DO NOT BE LAZY. We have plenty of time to do it right.**
> No shortcuts. Never make assumptions.
> Always dive deep and ensure you know the problem you're solving.
> Make use of search as needed.
> Measure 3x, cut once.
> No fallback. No stub (todo later) code.
> Just pure excellence, done the right way the entire time.
> Chesterton's fence: always understand the current implementation fully before changing it.

**Status:** Implemented (2026-09-19) — first published release on this channel is 0.2.30 (0.2.29's darwin-x64 packaging step failed on a `sha256sum` shim scoped to the wrong subshell; fixed by using `shasum -a 256` on macOS). Amended the same day with §Shell integration.
**Date:** 2026-09-19
**Authors:** Robert, Claude
**Tags:** distribution, install, update, github-releases, supersedes-npm
**Related:** hf2q ADR-045 (the design this borrows from), ADR-008 §linux-arm64 CI, `ctm-macos-notarization-gap` (memory)

## Context

ctm shipped as an npm package wrapping four platform-specific optional packages
(`@agidreams/ctm-<os>-<arch>`), with a Node shim resolving the native binary at
runtime. That model was chosen for reach and has cost more than it delivered:

- **It requires Node to install a Rust binary.** `postinstall.cjs`, `ctm-wrapper.cjs`
  and `resolve-binary.cjs` exist only to work around npm's hoisting/global-install
  layout variance (ADR-008 R2-B5, the `resolve-binary` fallback chain).
- **Publishing is a five-package, non-atomic dance** with a registry-propagation
  poll and no rollback (release.yml `publish` job). The 0.2.28 release proved the
  fragility: all four builds succeeded and the publish failed with npm's
  `404 Not Found - PUT` — an expired `NPM_TOKEN` — leaving a tag with no
  installable artifact.
- **The GitHub Release object already existed** as an afterthought (the npm
  tarballs were uploaded to it after publishing). The artifacts users need were
  one step from being directly installable the whole time.
- **`ctm` has no self-update.** Users ran `npm i -g claude-telegram-mirror@latest`,
  which re-resolved five packages and could not restart the service or reconcile
  hook paths.

hf2q solved the same problem for a Rust CLI (hf2q ADR-045, `src/distribution/`):
one small **release record** per target, one native executable, strict
origin-pinned bounded download with size+sha256 verification, an **atomic
rename swap** in the install directory under a lock with one `.previous` kept
for rollback, and a channel-ownership marker so the updater never overwrites a
binary it did not install. That design is correct and this ADR adopts it, with
one difference: hf2q publishes its record on a vanity domain; ctm has none and
uses GitHub's `releases/latest/download/<fixed-name>` redirect instead.

## Spikes (Kata step 2 — executed 2026-09-19)

| # | Question | Result |
|---|---|---|
| 1 | Does `github.com/<owner>/<repo>/releases/latest/download/<fixed-asset-name>` resolve? | **Yes** — `302` to the newest release's asset (probed against hf2q as control). No API call, no auth, no rate-limit exposure. |
| 2 | Does a `curl`-fetched, ad-hoc-signed arm64 binary run on macOS? | **Yes** — no quarantine xattr (only Finder/browsers set it), `Signature=adhoc` intact, executes. Also confirms `APPLE_*` secrets are absent in CI: every darwin release is ad-hoc-signed. |
| 3 | Can the binary be swapped while the launchd daemon runs, and does the service pick it up? | **Yes** — rename-over succeeds with the old process live; `.previous` retained; rollback works. The service plist and the `settings.json` hooks store an absolute path from `current_exe()`, so a swap at a stable path needs no re-registration; `launchctl kickstart -k` restarts onto the new file. |

## Decision

**GitHub Releases is the sole distribution channel. npm is retired.**

1. **Install** is one line, served from the repository:
   ```sh
   curl -fsSL https://raw.githubusercontent.com/robertelee78/claude-telegram-mirror/master/install.sh | sh
   ```
   `install.sh` detects the target triple, fetches `stable-<triple>.json` via the
   `latest/download` redirect, downloads `ctm-<triple>` from the same release,
   verifies size and sha256, installs to `~/.local/bin/ctm` (mode 0755) with a
   `.ctm-channel` marker, and prints the next step (`ctm setup` for new installs;
   `ctm doctor --fix` for existing ones, which reconciles service and hook paths).
   It never touches `PATH` silently — it prints the export line if needed.

2. **`ctm update [--check] [--rollback]`** (`src/update.rs`) implements the
   hf2q model natively:
   - channel detection from the running executable: *standalone* (marker present
     in its directory), *npm* (path contains `node_modules/@agidreams/`), *source*
     (`target/` in path). Only standalone updates in place. npm performs a
     **migration**: standalone install to `~/.local/bin`, then service + hooks
     re-registered to the new path, then the user is told to
     `npm uninstall -g claude-telegram-mirror`. Source refuses with guidance.
   - record fetch (bounded 4 KiB, strict schema, `deny_unknown_fields`), SemVer
     compare against `CARGO_PKG_VERSION`; `--check` stops here.
   - asset download streamed into `<dir>/.ctm-candidate.partial` with the final
     URL restricted to `github.com` / `release-assets.githubusercontent.com`,
     byte count bounded by the record, sha256 verified before anything is renamed.
   - publish: copy active → `.ctm-previous`, `rename(candidate → ctm)`, fsync,
     re-verify the active file's digest. `--rollback` swaps `.previous` back.
   - if a service unit is installed, restart it so the daemon runs the new binary;
     otherwise print that a running foreground daemon must be restarted.
   - The record is NOT required to be byte-canonical (hf2q's extra check guards a
     mutable vanity-domain URL; a GitHub Release asset is immutable per tag and
     its bytes are covered by the asset sha256 the record itself carries).

3. **Release workflow** (`release.yml`): the four builds are unchanged; the
   `publish` job now uploads `ctm-<triple>`, `ctm-<triple>.sha256`,
   `stable-<triple>.json` and `install.sh` to the GitHub Release. All npm steps,
   `NPM_TOKEN`, and `id-token: write` are removed.

4. **Repository:** `npm-packages/`, `package.json`, `package-lock.json`,
   `.npmignore`, `postinstall.cjs`, `scripts/ctm-wrapper.cjs`,
   `scripts/resolve-binary.cjs` are deleted; `scripts/bump-version.sh` updates
   `Cargo.toml` only. History retains them.

5. **`ctm doctor`** gains an "Update" check: channel, running version vs latest
   record, and service/hook path drift (a service or hook pointing at a binary
   other than the running one), fixable with `--fix`.

## Consequences

- Node.js is no longer required for anything.
- One artifact per target, immutable per tag, verifiable by anyone with `sha256sum`.
- Darwin binaries remain ad-hoc-signed (no `APPLE_*` secrets). This is fine for
  `curl` installs and `ctm update` (no quarantine); it remains a gap only for
  binaries that arrive via Finder/browser, which this channel never produces.
- Existing npm users migrate with `ctm update` (or the install line); their
  config, sessions DB, and Telegram topics are untouched — only the binary path
  and the service/hook registrations change.
- `bump-version.sh` shrinks to one file; the six-file lockstep problem disappears.
- The 0.2.28 tag stands (builds succeeded, feature is real) but has no
  user-installable artifact; 0.2.29 is the first release on this channel.

## Shell integration (amendment, 2026-09-19)

The operator's requirement: PATH and tab completion must be **automatic** — no
printed `export PATH=…` line to paste. hf2q prints the line and provisions
completions through clap's unstable dynamic completer with a ~1,100-line
reconcile-on-every-startup subsystem; ctm gets the same user outcome with a smaller,
deterministic mechanism (`src/shell.rs`):

- **`ctm completions <bash|zsh|fish>`** prints a static clap completion script.
- **`ctm shell-setup [--remove]`**, run by `install.sh` and after every `ctm update`
  (by the *new* binary, so the recorded path is right):
  1. writes the completion file to each shell's per-user autoload dir —
     `~/.local/share/bash-completion/completions/ctm`,
     `~/.local/share/zsh/site-functions/_ctm`, `~/.config/fish/completions/ctm.fish`;
  2. maintains ONE idempotent, marker-delimited block (`# >>> ctm >>> … # <<< ctm <<<`)
     **appended at the end** of the shell's rc — `~/.zshrc`; `~/.bashrc` and
     `~/.bash_profile` when present; `~/.config/fish/conf.d/ctm.fish` — that prepends
     the install dir to `PATH` and, for zsh, adds the completion dir to `fpath` and
     runs `compinit` only if the user's config has not. Appending at the end is the
     whole trick: version managers (fnm, nvm, …) prepend their shim dirs earlier in
     the same file, so a block that runs last wins — verified in a fresh zsh whose
     rc deliberately prepended a shim after `~/.local/bin`.
  3. touches only the login shell's rc (creating it if missing) plus any other
     supported shell whose config already exists; never litters. `--remove` deletes
     the completion files and removes the block exactly (rc restored byte-for-byte
     in the test). `CTM_NO_SHELL_SETUP=1` opts out.

