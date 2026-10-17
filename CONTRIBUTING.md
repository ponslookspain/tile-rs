# Contributing to tile-rs

Thanks for helping! Bug reports, ideas and pull requests are all welcome.

## Reporting a problem

Please include: your Windows version, the tile-rs version (`tile-rs.exe --version`), what you
did, what you expected and what happened. The log (tray menu → *Open log*) and the output of
`tile-rs.exe list` are often exactly what is needed. Do not paste anything private: window
titles can contain it.

## Building

You need [Rust](https://rustup.rs) (stable) and the C++ build tools of Visual Studio
(*Desktop development with C++*).

```powershell
git clone https://github.com/ponslookspain/tile-rs.git
cd tile-rs
cargo run             # debug build (has a console window)
cargo build --release # target\release\tile-rs.exe
```

## Before you open a pull request

```powershell
./scripts/check.ps1
```

It runs exactly what CI runs: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`
and a release build.

## Rules of the road

- **Branch** from `main`: `feat/...`, `fix/...`, `docs/...`. One topic per pull request.
- **Tests**: logic that does not call Windows (layout geometry, config parsing and migration,
  animation math) lives in `src/geometry.rs` and `src/config.rs` and must be covered by unit
  tests. Code that moves windows is checked by hand: say in the pull request how you tried it.
- **Config compatibility**: an existing `config.toml` must keep working. New settings are
  optional, get a default and are added to `migrate()`; see [docs/RELEASING.md](docs/RELEASING.md).
- **Changelog**: add a user-facing line to `## [Unreleased]` in `CHANGELOG.md`.
- **Docs**: update `README.md`, `README.ru.md` and `docs/CONFIGURATION.md` when behaviour or settings change.
- **Style**: `cargo fmt`; no new clippy warnings; keep `unsafe` blocks small, next to the Windows call they wrap.

## Project layout

```
src/main.rs        event handling, layout, workspaces, animations, start-up
src/window.rs      window discovery and classification (tiled / fullscreen / skipped)
src/geometry.rs    pure layout maths (spiral, resize ratios, neighbours, easing) + unit tests
src/config.rs      config file: parsing, defaults, migration, hotkey syntax + unit tests
src/launcher.rs    the settings launcher window
src/overlay.rs     the focus border and the workspace indicator (drawn by tile-rs itself)
src/tray.rs        tray icon and menu
src/prof.rs        opt-in profiling (TILE_RS_PROFILE)
docs/              configuration reference, release process
scripts/           check.ps1 (CI checks), package.ps1 (release zip), build-installer.ps1,
                   release-notes.ps1, make-icon.ps1
.github/workflows/ CI and release automation
```

Releases are made by the maintainers, see [docs/RELEASING.md](docs/RELEASING.md).
