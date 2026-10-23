# Versioning and releasing

How tile-rs versions are numbered, how changes are recorded, and how a release is made.

## Version numbers

tile-rs follows [Semantic Versioning](https://semver.org): `MAJOR.MINOR.PATCH`.

While the major version is `0` (the current phase) the rules are:

| Bump | When |
|---|---|
| `0.MINOR.0` | a new feature, **or** a change that could affect existing users (removed/renamed setting, changed default hotkey, different behaviour) |
| `0.x.PATCH` | bug fixes, documentation, performance, internal clean-up: nothing a user has to notice or adapt to |
| `1.0.0` | when the config format and hotkey names are considered stable |

From `1.0.0` on: `MAJOR` = breaking change, `MINOR` = backwards-compatible feature, `PATCH` = fix.

Pre-releases use a suffix: `0.3.0-rc.1`, `0.3.0-rc.2` ... They are published as GitHub
*pre-releases* and never replace the latest stable release.

### Where the version lives

**`version` in `Cargo.toml` is the single source of truth.** It appears in the tray menu and in
`tile-rs.exe --version`. The git tag must be exactly `v` + that version (`v0.3.0`); the release
workflow refuses to publish if they differ.

## Compatibility promise (config files)

Users keep one `config.toml` for years, so:

1. A config that works in version N **must keep working** in N+1. Do not remove or rename a key;
   if a rename is unavoidable, accept both names for at least one minor release and say so in the changelog.
2. New settings are **optional** and have a default.
3. A new setting or default hotkey that users should get automatically is added to
   `migrate()` in `src/config/mod.rs` (it appends missing keys to existing files, never changes
   existing lines) **and** to `default_config()`, with a test in `config::tests`.
4. A change of meaning of an existing setting counts as breaking: bump `MINOR` (while `0.x`) or
   `MAJOR` (from 1.0) and describe the migration in the changelog under **Changed**.

## Changelog

[`CHANGELOG.md`](../CHANGELOG.md) follows [Keep a Changelog](https://keepachangelog.com).

- Every pull request that changes behaviour adds a line under `## [Unreleased]`, in one of:
  **Added**, **Changed**, **Deprecated**, **Removed**, **Fixed**, **Security**.
- Write for users, not for developers ("Alt+= now repeats while held", not "refactored resize_focused").
- At release time `[Unreleased]` becomes `[X.Y.Z] - YYYY-MM-DD` and a fresh empty `[Unreleased]` is added on top.
- The text of that section is used as the GitHub release notes.

## Branches and commits

- `main` always builds, passes all checks and could be released.
- Work happens on short-lived branches: `feat/<name>`, `fix/<name>`, `docs/<name>`, `release/vX.Y.Z`.
- Changes reach `main` through a pull request. CI must be green (format, clippy, tests, release build).
- Commit messages: a short imperative subject (`Add focus border`, `Fix crash when a window closes mid-drag`),
  a blank line and, if useful, an explanation of *why*.

## Making a release

Prerequisites: everything you want in the release is merged into `main` and CI is green.

1. **Branch**: `git switch main && git pull && git switch -c release/vX.Y.Z`
2. **Bump** `version` in `Cargo.toml`; run `cargo build` so `Cargo.lock` follows.
3. **Changelog**: rename `[Unreleased]` to `[X.Y.Z] - date`, add a new empty `[Unreleased]`,
   update the compare links at the bottom of the file.
4. **Check**: `./scripts/check.ps1` (format, clippy, tests, release build) and, if you like,
   `./scripts/package.ps1` to inspect the zip locally.
5. **Pull request** `release/vX.Y.Z` → `main`; merge when CI is green.
6. **Tag** the merge commit on `main` and push the tag:
   ```powershell
   git switch main; git pull
   git tag -a vX.Y.Z -m "tile-rs X.Y.Z"
   git push origin vX.Y.Z
   ```
7. The **Release workflow** (`.github/workflows/release.yml`) now: verifies that the tag matches
   `Cargo.toml`, runs the tests, builds `tile-rs-vX.Y.Z-windows-x64.zip` (exe, README, LICENSE,
   CHANGELOG, configuration reference) plus its `.sha256`, builds the Windows installer
   `tile-rs-X.Y.Z-setup.exe` (Inno Setup, see `installer/tile-rs.iss`) plus its `.sha256` and Ed25519 `.sha256.sig`, and publishes a GitHub
   Release whose notes come from the changelog.
8. **Verify**: download the installer from the release page and run it on a clean profile (it
   installs, opens the launcher and starts tile-rs); also check the portable zip and its SHA-256
   (`tile-rs.exe --version` shows the new version).

### Hotfix

Branch from the tag that needs fixing (`git switch -c fix/<name> vX.Y.Z`), fix, merge to `main`
through a pull request, then release `X.Y.(Z+1)` as above.

### Withdrawing a bad release

Do not move or delete tags. Mark the release as a pre-release or edit its notes with a warning,
and publish a fixed `PATCH` version.

## Code signing (optional)

`scripts/sign.ps1` code-signs a file with `signtool.exe` (SHA-256, DigiCert RFC 3161 timestamp).
It is called from `scripts/package.ps1` (the zipped exe) and `scripts/build-installer.ps1` (the
`dist\tile-rs.exe` and the installer), and it is a **no-op unless a certificate is configured**, so
local builds and unsigned releases keep working.

To sign a release, add these repository secrets (Settings → Secrets and variables → Actions) and
they are passed to the packaging steps automatically:

| Secret | Purpose |
|---|---|
| `WINDOWS_CERT_PFX_BASE64` | a code-signing `.pfx`, base64-encoded (`[Convert]::ToBase64String([IO.File]::ReadAllBytes('cert.pfx'))`) |
| `WINDOWS_CERT_PASSWORD` | the `.pfx` password |
| `WINDOWS_CERT_THUMBPRINT` | alternative: the SHA-1 thumbprint of a certificate already installed in the runner's user store |

`WINDOWS_CERT_PFX_BASE64` takes precedence; set only one of the two. The signing machine needs the
Windows SDK (for `signtool.exe`); the GitHub `windows-latest` runner has it.

## Binaries and trust

Release binaries are built by GitHub Actions from the tagged commit; the zip's SHA-256 is
published next to it. The executable is **not code-signed** until a certificate is configured as
above (Windows SmartScreen shows a warning on the first start until then).

## Updates in installed applications

The manager checks the latest stable GitHub Release 15 seconds after startup and then every six
hours. A tray notification opens the launcher. In **Updates**, users can check manually, read
release notes, and explicitly install an available update. Publishing source commits alone does
not notify users: push a version tag and let the release workflow publish its installer and
`tile-rs-X.Y.Z-setup.exe.sha256` and `.sha256.sig` together. Drafts and prereleases are excluded.

Downloads are limited to the repository's HTTPS release assets. The exact checksum asset bytes are authenticated with the embedded Ed25519 public key first.
The signed filename must match the requested release, preventing reuse of an older signed
installer under a newer version. The installer size and SHA-256 are then checked before a helper starts it. The helper waits for the launcher and manager to close,
updates the current executable's folder, then starts tile-rs again. Existing user configuration
is preserved. Windows may request elevation for a protected installation directory. Cancelling
or failing installation shows an error and attempts to restart the existing executable.

The update button uses the Windows installer, including for copies originally extracted from a
portable ZIP; the updated folder becomes a registered Windows installation. Keep using manual
ZIP downloads if an unregistered portable copy is required. An older release without an installer
checksum and signature cannot be installed through this updater. Versions shipped before this update feature
must first be upgraded manually. Ed25519 authenticates updates independently of Authenticode; configure code signing above for
Windows publisher identity verification. Older installed clients that only check SHA-256 ignore
the extra signature asset and can still update to signed releases. The first release containing this updater must be tested on
Windows with two real release versions (notification, check, install, restart and config retention).


## Provisioning the update signing key

The checksum signature is a raw 64-byte Ed25519 signature over the **exact bytes** of
`tile-rs-X.Y.Z-setup.exe.sha256`, including its filename and newline. It is published as
`tile-rs-X.Y.Z-setup.exe.sha256.sig`. `scripts/sign-update.ps1` uses OpenSSL 3 (`pkeyutl -sign -rawin`),
checks that the private key matches the embedded public key, and verifies the output before publication.

No production signing key is included. `UPDATE_VERIFYING_KEY` in `src/platform/update.rs` is
explicitly `None`: ordinary builds and CI remain green, but in-app installation fails closed with
a configuration error. The release workflow refuses to publish until both the public key and the
`UPDATE_SIGNING_KEY` secret are configured. This avoids an intentionally failing reminder test
while ensuring an unsigned update is never installed or published by this workflow.

Before the first release with signature verification:

1. On a trusted machine, in a private directory **outside the repository**, generate the key pair:
   ```powershell
   openssl genpkey -algorithm ED25519 -out update-signing.pem
   openssl pkey -in update-signing.pem -pubout -outform DER -out update-public.der
   $der = [IO.File]::ReadAllBytes((Resolve-Path update-public.der))
   $publicHex = ($der[($der.Length - 32)..($der.Length - 1)] | ForEach-Object { $_.ToString('x2') }) -join ''
   $publicHex
   ```
2. Replace the constant with `const UPDATE_VERIFYING_KEY: Option<&str> = Some("PUBLIC_HEX");`,
   substituting the 64 public hex characters. Commit **only the public key**. Keep the private
   PEM in a secure backup; never add it to the checkout or release assets.
3. Set repository Actions secret `UPDATE_SIGNING_KEY` to the complete contents of
   `update-signing.pem`, including the BEGIN/END lines. The release step receives it through an
   environment variable, writes it only to a temporary file, and removes that file in `finally`.
4. Run `./scripts/check.ps1`, then publish through the release workflow. Missing secrets,
   mismatched keys, signing errors, and invalid output signatures fail the release.

Existing verifier-enabled installations trust their embedded key. Preserve the pair for future
releases; rotating it requires an explicit migration release trusted by the previous key.
Production secrets and release publication remain maintainer actions.
