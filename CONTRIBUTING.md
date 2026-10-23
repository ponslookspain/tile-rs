# Contributing to tile-rs

Thanks for helping! Bug reports, ideas and pull requests are all welcome.

## Reporting a problem

Please include: your Windows version, the tile-rs version (`tile-rs.exe --version`), what you
did, what you expected and what happened. The log (tray menu → *Open log*) and the output of
`tile-rs.exe list` are often exactly what is needed. Do not paste anything private: window
titles can contain it.

## Building

See [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) for the full run, test, packaging and release flow.

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
  animation math) lives in `src/layout/geometry.rs` and `src/config/` and must be covered by unit
  tests. Code that moves windows is checked by hand: say in the pull request how you tried it.
- **Config compatibility**: an existing `config.toml` must keep working. New settings are
  optional, get a default and are added to `migrate()`; see [docs/RELEASING.md](docs/RELEASING.md).
- **Changelog**: add a user-facing line to `## [Unreleased]` in `CHANGELOG.md`.
- **Docs**: update `README.md` and `docs/CONFIGURATION.md` when behaviour or settings change.
- **Style**: `cargo fmt`; no new clippy warnings; keep `unsafe` blocks small, next to the Windows call they wrap.

## Project layout

See the [source map in the development guide](docs/DEVELOPMENT.md#структура-исходного-кода).

Releases are made by the maintainers, see [docs/RELEASING.md](docs/RELEASING.md).
