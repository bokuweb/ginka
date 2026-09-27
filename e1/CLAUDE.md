# CLAUDE.md

**This project's agent instructions live in [`AGENTS.md`](AGENTS.md). Read it first, then [`docs/roadmap.md`](docs/roadmap.md) — and [`docs/ui.md`](docs/ui.md) for anything that renders.**

Quick orientation:

- **e1** is a native GitHub client written in Rust on **GPUI**, built as a standalone app and mounted as Ginka's Inbox from the same workspace: the views are a library, the GitHub access is behind a trait, and the toolkit rev is Ginka's.
- Architecture in one line: a thin **`e1` binary** owns standalone lifecycle integration, opens a frameless glass window and mounts **`e1-views`**, which draws what **`e1-github`** fetches; **`e1-ui`** holds the tokens, settings and view models, and **`e1-updater-macos`** contains the standalone Sparkle FFI.
- UI: the same three-column workstation as Ginka — navigation on the left, the list in the centre, the item on the right — on a dark glass surface, specified in `docs/ui.md`. Built on **`gpui-component`** at the rev Ginka's lock file pins.
- The rules that make embedding possible (views in a library, the API behind a trait, no second reactor, one toolkit rev, tokens by name) are `AGENTS.md` § Architectural rules; the embedding contract itself is `docs/roadmap.md` §4.3.
- Everything written into this repository is **English** — code, rustdoc comments (`///` on every public item), docs, commit messages and PR titles/descriptions.

When a change alters architecture, data model or scope, update `docs/roadmap.md` — including its decision log — in the same change.
