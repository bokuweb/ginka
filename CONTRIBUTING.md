# Contributing to Ginka

Read [`AGENTS.md`](AGENTS.md) first — it holds the architectural rules every
change is checked against — then [`docs/roadmap.md`](docs/roadmap.md) for
where the work is, and [`docs/ui.md`](docs/ui.md) for anything that renders.

## Building

- The toolchain is pinned in `rust-toolchain.toml` (Rust 1.97.1, with
  `rustfmt` and `clippy`); `rustup` picks it up.
- macOS and Linux are built in CI. Linux needs the packages the CI workflow
  installs (`.github/workflows/ci.yml`).
- `cargo run` starts the window, which starts a daemon when there is none.
  `cargo run -p ginka-cli -- <command>` is the CLI; `GINKA_HOME=<dir>` points
  both at a throwaway state directory instead of `~/.ginka`.
- The e1 GitHub client lives in this workspace. `cargo run -p e1` starts its
  standalone app; `cargo run --features github` mounts its views in Ginka.
- On an external disk that is not APFS, macOS writes `._*` AppleDouble
  sidecars beside files. Delete them before building; `locales/._app.yml`
  alone breaks the translation macro.

## Before a pull request

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --all-targets --features github -- -D warnings
cargo test --workspace
```

CI runs the first, the second and the last on macOS and Linux.

## How changes are written

- **Test first.** Behaviour lands with the test that pins it, written before
  the code and seen failing. Logic belongs where it can be tested without a
  window: domain rules in `ginka-core`, view models in `ginka-ui`; the views
  in `src/` only draw.
- **One protocol.** A capability is a `Request` the daemon answers, so the
  window, the CLI and MCP reach it the same way; a change to the wire bumps
  `PROTOCOL_VERSION` and adds a wire-format test.
- **English** in code, rustdoc (`///` on every public item), docs, commit
  messages and pull requests. User-visible strings go in `locales/app.yml`
  in both English and Japanese.
- **Nothing copied.** Other projects are read for behaviour, never copied
  or ported from (roadmap R9).
- **Docs move with the code.** A change to architecture, data model or scope
  updates `docs/roadmap.md`, its checklist and its decision log, in the same
  change.
- Commit subjects are an imperative sentence about what changes for the
  reader — "Open the window where it was left" — with a body that says why.

## Licence

The repository has no licence file yet. Until it does, ask before
contributing code you want to keep rights in.
