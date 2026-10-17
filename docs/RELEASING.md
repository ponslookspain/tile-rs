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
   `migrate()` in `src/config.rs` (it appends missing keys to existing files, never changes
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
   `tile-rs-X.Y.Z-setup.exe` (Inno Setup, see `installer/tile-rs.iss`), and publishes a GitHub
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

## Binaries and trust

Release binaries are built by GitHub Actions from the tagged commit; the zip's SHA-256 is
published next to it. The executable is currently **not code-signed** (Windows SmartScreen shows
a warning on the first start).
