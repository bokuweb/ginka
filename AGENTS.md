# AGENTS.md

Guidance for AI coding agents (and humans) working in this repository.

## What this project is

**Ginka is an IDE-agnostic coding-agent orchestrator built in Rust on GPUI.**

It is a native reimplementation of what [band-app/band](https://github.com/band-app/band) does — manage many coding agents across many projects and git worktrees, with chat, terminal, diff review and status in one window — replacing Band's Electron + local Node server with a single Rust binary plus a background daemon.

[Orca](https://www.onorca.dev/) informs the interaction design: prompt fan-out across worktrees, diff-line comments batched back to the agent, click-an-element design mode.

**Ginka is written here.** Other clients in this space are worth reading for behaviour and for the edge cases they have already hit, and worth looking at for how they render. No code from any of them is copied, ported or paraphrased into this repository — see the roadmap's R9 and its decision log.

**Read [`docs/roadmap.md`](docs/roadmap.md) before starting any non-trivial work.** It holds the architecture, the crate layout, the data model, the milestone plan and the open decisions. This file is the short version; the roadmap is authoritative.

**For anything that renders, read [`docs/ui.md`](docs/ui.md) too.** It holds the layout spec, the design tokens, the region-by-region breakdown, and the mapping from each region to the component that draws it.

## Current state

**Milestone 0 is largely landed** (see `docs/roadmap.md` §5 for what remains: the glass window layer, motion helpers, type export, settings hot-reload, crash handler). What works today:

- The three-column shell renders with resizable sidebar and right panel, a composer, a context bar, a terminal dock and the surface chooser — all against sample data. **Not yet visually signed off** (see roadmap M0).
- Design tokens load from `assets/themes/` and are bridged onto the toolkit's theme; the appearance follows the system unless overridden.
- Storage boots: SQLite migrations, settings, logging. `ginka doctor` verifies it.

Everything in the sidebar and the transcript is **placeholder content** (`src/workspace.rs::SessionRow::samples`). M1 replaces it with real projects and worktrees.

**The domain layer for N1–N14 has landed ahead of its milestones** (roadmap §3.3), test-first and with no UI on top of it yet:

| Module | What it decides |
| --- | --- |
| `ginka-protocol::provider` | provider kinds, access modes, the model/effort/tier vocabulary, and when an option change forces a restart |
| `ginka-protocol::session` | the two title fields and their precedence |
| `ginka-core::driver` | the session traits, the steer-or-queue policy, and `apply_session_options` |
| `ginka-core::composer` | completion triggers, provider+disk command merge, the bounded `@file` index |
| `ginka-core::checkpoint` | three refs per turn, turn diffs, rewind |
| `ginka-core::review` | diffs by source, including "this turn" |
| `ginka-core::commit` | the cheap-tier policy, the prompt, the commit path |
| `ginka-core::transcript` | sessions, messages and daemon-side search |
| `ginka-core::skills`, `::blob`, `::attachment`, `::usage` | the skill library, image externalisation, uploads, cost and plan headroom |

`ginka-core::driver::testing::ScriptedSession` is how session behaviour is tested — never a live vendor CLI.

## Commands

```bash
cargo run                  # the desktop app
cargo run -p ginka-cli -- doctor   # where state lives and whether it is healthy
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

`cargo clippy`/`cargo test` on the whole workspace also builds the GPUI app; the domain crates alone are
`cargo test -p ginka-core -p ginka-protocol`, which is the fast loop.

`GINKA_HOME` overrides `~/.ginka`; point it at a temp directory rather than testing against your real state. `GINKA_LOG` sets the tracing filter.

## Layout

```
ginka/
├─ Cargo.toml           # workspace root; the GPUI binary lives here
├─ src/                 # GPUI app: views only -- shell, sidebar, surfaces
├─ crates/
│  ├─ ginka-protocol/   # serde wire types shared by every process; ts-rs export
│  ├─ ginka-core/       # domain: projects, worktrees, sessions, drivers, git, cron
│  ├─ ginka-daemon/     # binary: WebSocket RPC server, owns SQLite + processes
│  ├─ ginka-client/     # async RPC client used by the app and the CLI
│  └─ ginka-cli/        # binary: the `ginka` command
├─ db/migrations/       # SQL migrations, embedded at compile time
├─ locales/             # rust-i18n yml (en, ja) -- planned; does not exist yet (M0)
├─ assets/themes/       # design tokens (dark.json, light.json)
├─ assets/icons/        # app-owned icons, layered over the toolkit's set
└─ docs/
```

## Architectural rules

These are load-bearing. Violating them creates work that has to be undone.

1. **The daemon owns all state.** SQLite, agent processes, PTYs and git operations live in `ginka-daemon`. The GPUI app holds view state only and must be killable at any moment without losing anything.
2. **Domain logic belongs in `ginka-core`, not in `src/`.** If it can be tested without a window, it must live where it can be tested without a window.
3. **The CLI and the UI speak the same protocol.** Never add a capability to the UI through a private path. If the UI can do it, `ginka-cli` and the MCP server can do it too — this is what lets agents drive the app.
4. **A workspace is a git worktree, keyed by an immutable `name`, never by the live branch.** An agent switching branches inside a worktree must not re-key anything.
5. **Streaming surfaces are virtualized from the first commit.** Chat transcripts and terminal output are the two places the performance budget (roadmap §6.2) gets lost.
6. **Every driver normalizes into one `AgentEvent` stream.** Vendor-specific shapes stop at the driver boundary; nothing above `ginka-core::driver` knows whether it is talking to Claude Code or Codex.
7. **Local-first.** No feature may require an account or a remote service.
8. **Testable UI code goes in `ginka-ui`, never in `src/`.** `rustc` overflows its stack expanding `#[test]` in a crate that also holds the toolkit's builder chains, so a test next to a view does not merely offend rule 2 — it fails to compile.
9. **`gpui-component` is the only linked UI toolkit, and it owns the `gpui` rev.** Never add a second GPUI component library and never pin `gpui` directly — `bezel` and `gpui-component` link incompatible forks of GPUI (`bezel-gpui 0.3.8+zed.82aeef` vs `gpui 0.2.2` from zed git), so mixing them does not compile. See roadmap §4.6.
10. **No view hardcodes a colour, radius or duration.** Everything resolves through the theme tokens in `docs/ui.md` §2.

## UI stack

- **Linked:** [`gpui-component`](https://github.com/longbridge/gpui-component) — dock layout (resizable panels, draggable tabs), virtualized list/table, `CodeEditor` with tree-sitter + LSP, markdown, charts, sidebar, webview.
- **Design reference, not a dependency:** [`bezel`](https://bezel.gallery/) ([crabtalk/bezel](https://github.com/crabtalk/bezel)). Where it is genuinely ahead — the glass/vibrancy theme, animated agent status orbs, composer slash-command and link handling, motion helpers — **look at it and build ours**. Its licence would permit copying; we still do not, and its code targets a different `gpui` fork anyway. Do not add it to `Cargo.toml`; see rule 9.
- **Ours by design:** agent status glyphs, the glass theme layer, transcript event views (tool cards, reasoning, plan approval, ask-user, diff sidecars), and the terminal view. `docs/ui.md` §5.
- Before writing any widget, check `gpui-component`'s gallery for an existing one.

## Conventions

- **Rust edition 2024.** `cargo fmt` and `cargo clippy -D warnings` must pass; CI enforces both.
- **`gpui` comes in transitively via `gpui-component`.** Bump the toolkit, never GPUI directly, and do it as its own PR.
- **Errors:** `anyhow` at binary boundaries, typed errors (`thiserror`) inside `ginka-core` and `ginka-protocol`.
- **Async:** `smol` and GPUI's executor. Do not introduce a second reactor without a note in the roadmap's decision log.
- **Tests:** test-first for anything with a decision in it — the test says what the rule is, and the awkward cases (a refused steer, a hand edit between turns, an unpriced model) are the point. Every bug fix lands with a regression test. Agent-session behaviour is tested against `ScriptedSession`, never a live vendor CLI; git behaviour against a real repository in a temp directory (`crates/ginka-core/tests/support`).
- **i18n:** user-visible strings go through `rust-i18n`, with `en` and `ja` both maintained. **Not wired up yet** — it is an open M0 item, and every string added before it lands is one to retrofit.
- **a11y is a rule, not a polish pass.** Every control reachable by mouse is reachable by keyboard with visible focus; decorative animation honours the system reduce-motion setting; nothing encodes meaning in colour, hover or motion alone. Roadmap §6.4.
- **Commits:** imperative subject, explain *why* in the body. Reference the roadmap milestone when the change advances one.
- **Comments:** explain non-obvious constraints (why the workspace id is derived from `name`, why a poll is throttled), not what the code plainly says.

## Working agreements for agents

- When a change alters architecture, data model, or scope, **update `docs/roadmap.md` in the same change** — including the decision log at the bottom. When it alters layout, tokens or component choices, update `docs/ui.md`.
- Do not silently expand scope. The roadmap's §3.2 non-goals and the milestone ordering are deliberate; if something seems missing, it is probably deferred on purpose.
- Prefer reading prior art for behaviour before designing from scratch — Band and the other open agent clients have already hit the edge cases (worktree sync, terminal reattach, session resume). **Read them; write our own.** Take the requirement away from the reading and implement it here; do not copy, port or paraphrase a file with it open. Most of that prior art is GPL-3.0, so copying would settle the licence question (roadmap Q3) by accident — but the rule holds for permissively licensed code too, because an implementation we did not write is one we cannot debug.
- The requirements that constrain interfaces live in roadmap §3.3 as N1–N16. If you are about to design something that sounds like one of them — steering, session options, checkpoints, attachments, titles, updates — read that row first.
- Keep this file and `CLAUDE.md` truthful. If you add commands, add them here once they actually work.
