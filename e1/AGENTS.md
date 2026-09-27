# AGENTS.md

Guidance for AI coding agents (and humans) working in the `e1/` subtree of
Ginka's monorepo. The root `AGENTS.md` also applies.

## What this project is

**e1 is a native GitHub client, written in Rust on GPUI, built both as a standalone app and as Ginka's Inbox.**

It reads the things a person checks on GitHub between commits — the inbox, the pull requests waiting on them, the issues assigned to them, any repository's open work — and shows them in one window with the same layout, tokens and toolkit as Ginka's agent workstation. Ginka mounts the same views as its Inbox.

**Read [`docs/roadmap.md`](docs/roadmap.md) before starting any non-trivial work.** It holds the architecture, the crate layout, the data model, the embedding contract, the milestone plan and the decision log. This file is the short version; the roadmap is authoritative.

**For anything that renders, read [`docs/ui.md`](docs/ui.md) too.** It holds the layout, the design tokens and the region-by-region breakdown.

## Current state

**M0 and M1 have landed; M2 is under way.** The window opens frameless over a blurred desktop, with the navigation sidebar, the centre list and the right detail panel as resizable columns whose arrangement persists. The GitHub client reads the viewer, the inbox, repositories, pull requests, issues, comments and a pull's files with their diffs over REST, with a scripted fake standing in for it in tests and in `E1_DEMO=1`. A pull's detail has a Files tab whose diffs are one virtualized list across every file. A repository has a file finder (the whole tree in one request, matched locally with `nucleo`) that reads files into the right panel, and the centre strip has a search box over GitHub's issue search. Two caches make it fast: answers are kept on disk with their `ETag`s and revalidated with `If-None-Match`, and the store's memory is written as a snapshot the next launch opens on. Avatars are fetched once into `~/.e1/cache/avatars/` and drawn from there. An item can be commented on, closed, reopened and merged from its detail. Signing in is GitHub's device flow from the window, with the token kept in the macOS keychain. See `docs/roadmap.md` §5 for what each milestone still owes.

## Commands

```bash
cargo run -p e1 --release                   # the standalone desktop app
cargo run -p e1                             # the same in the dev profile
E1_DEMO=1 cargo run -p e1                   # scripted data, no network
E1_DEMO=1 E1_DEMO_OPEN='bokuweb/ginka#12:src/shell.rs' cargo run -p e1
e1/scripts/bundle-macos.sh debug            # ad-hoc signed app, ZIP and DMG in root target/dist
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo test -p e1-github -p e1-ui            # the fast loop: no GPUI build
```

Run these commands from the Ginka repository root. On a volume without native extended attributes macOS drops `._*` sidecar files next to every file written; `rust-i18n` reads every file in `locales/`, so remove sidecars before a build that fails on `locales/._app.yml`.

The token is looked for in this order: `E1_GITHUB_TOKEN`, `GITHUB_TOKEN`, `GH_TOKEN`, the keychain entry the window wrote when the reader signed in, then whatever `gh auth token` prints. Without one the window opens on the sign-in screen, which runs GitHub's device flow as the `e1` OAuth app (`e1_github::auth::DEFAULT_CLIENT_ID`, public by design); a fork with its own app sets `E1_GITHUB_CLIENT_ID` at build time or at run time. `E1_HOME` overrides `~/.e1`; `E1_LOG` sets the tracing filter. `~/.e1/cache/` is safe to delete at any time.

## Layout

```
e1/
├─ Cargo.toml           # workspace member; the `e1` binary lives here and is deliberately thin
├─ src/main.rs          # opens the window, mounts e1-views::Shell
├─ crates/
│  ├─ e1-github/        # domain: the data model, the `GitHub` trait, the REST client,
│  │                    # token discovery, and the scripted fake. No GPUI.
│  ├─ e1-updater-macos/ # standalone-only safe API over the contained Sparkle FFI
│  ├─ e1-ui/            # design tokens, assets, settings, layout, view models --
│  │                    # everything UI-side that is testable without a window
│  └─ e1-views/         # the GPUI views, as a library a host window can mount
├─ locales/app.yml      # every user-visible string, en and ja side by side
├─ assets/themes/       # design tokens (dark.json, light.json), Ginka's schema
├─ assets/icons/        # app-owned icons, layered over the toolkit's set
├─ assets/macos/        # the application icon installed in e1.app
├─ resources/macos/     # the generated bundle's Info.plist template
├─ scripts/             # explicit macOS bundle and appcast construction
└─ docs/
```

## Architectural rules

These are load-bearing. Each one keeps the standalone app and Ginka Inbox using the same views.

1. **The views are a library.** Everything that draws lives in `e1-views`, and `src/main.rs` only opens a window and hands it a `Shell`. A view that only exists in the binary cannot appear in Ginka.
2. **GitHub is reached through the `GitHub` trait, never directly.** Views hold an `Arc<dyn GitHub>` and nothing else knows about HTTP. Ginka's daemon owns all state in that app (its rule 1), so when embedded the implementation it supplies will proxy through the daemon — which is only possible if no view has a private path to the network.
3. **No second reactor.** HTTP is blocking (`ureq`) and runs on GPUI's background executor. Ginka runs on `smol` and forbids a second async runtime in its process; an async HTTP client here would bring tokio along.
4. **One toolkit, at Ginka's rev.** `gpui-component` is the only linked UI library and it owns the `gpui` rev; the root `Cargo.lock` pins both. Two revs of `gpui` are two unrelated sets of types, and a view built against the wrong one cannot be mounted at all. Never pin `gpui` directly; bump the toolkit as its own change.
5. **Tokens by name, and the same names as Ginka.** No view hardcodes a colour, radius or duration; everything resolves through `e1_ui::Tokens`, whose JSON schema is Ginka's `assets/themes/*.json`. That is what lets the host swap its own tokens in (roadmap §4.3).
6. **Domain logic belongs in `e1-github` or `e1-ui`, not in `e1-views`.** If it can be tested without a window, it must live where it can be tested without a window. This is also a compiler constraint: `rustc` overflows its stack expanding `#[test]` in a crate that also holds the toolkit's builder chains, so `e1-views` carries no tests at all.
7. **Streaming and long lists are virtualized from the first commit.** The centre list, the file finder, a pull's diffs and a file's lines are each one `uniform_list`; a repository with four thousand issues or a diff with four thousand lines must not cost four thousand elements.
8. **Local-first, and the token never touches a plain file.** No feature may require anything but a GitHub token. A token the window obtained goes to the platform keychain and nowhere else; one from the environment or from `gh` is read on every launch and never copied.

## UI stack

- **Linked:** [`gpui-component`](https://github.com/longbridge/gpui-component) — resizable panels, virtualized lists, markdown, tooltips, inputs, and (through its `tree-sitter-languages` feature) the grammars and highlight queries the file view colours code with. That feature compiles some thirty grammars, so the first build after a `cargo clean` is long; nothing else about it is felt.
- **Reference, not a dependency:** Ginka's own `src/` for how the three-column frameless window is assembled, and `bezel` for the glass theme. Both are read for the mechanism and rebuilt here against our own types; nothing is copied in.
- Before writing a widget, check `gpui-component`'s gallery for an existing one.

## Conventions

- **Rust edition 2024.** `cargo fmt` and `cargo clippy -D warnings` must pass; CI enforces both.
- **Errors:** `anyhow` at binary boundaries, typed errors (`thiserror`) inside `e1-github`.
- **Tests:** test-first for anything with a decision in it — the wire mapping (a merged pull is `closed` with `merged_at` set), the token precedence, the `Link` header, relative time. Network behaviour is tested against `e1_github::Scripted`, never against GitHub.
- **i18n:** user-visible strings go through `rust-i18n`. `en` and `ja` are both maintained.
- **a11y is a rule, not a polish pass.** Every control reachable by mouse is reachable by keyboard with visible focus; nothing encodes meaning in colour alone (a state is an icon *and* a colour).
- **English in the repository.** Code, comments, docs, commit messages and pull requests are written in English, no matter what language the conversation that produced them was in.
- **Commits and pull requests:** imperative subject, explain *why* in the body. Reference the roadmap milestone when the change advances one.
- **Comments are rustdoc.** Every public item carries a `///` comment; every crate and module root carries a `//!` header saying what lives there and what it owns. Document what a caller must know — invariants, errors, the constraint that made the code look the way it does — not what the signature already says.

## Working agreements for agents

- When a change alters architecture, data model, or scope, **update `docs/roadmap.md` in the same change**, including the decision log at the bottom. When it alters layout, tokens or component choices, update `docs/ui.md`.
- Do not silently expand scope. The milestone ordering in the roadmap is deliberate.
- When something here diverges from how Ginka does the same thing, say why in the decision log — divergence is what embedding pays for.
- Keep this file and `CLAUDE.md` truthful. If you add commands, add them here once they actually work.
