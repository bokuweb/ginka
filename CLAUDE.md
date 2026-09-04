# CLAUDE.md

**This project's agent instructions live in [`AGENTS.md`](AGENTS.md). Read it first, then [`docs/roadmap.md`](docs/roadmap.md) — and [`docs/ui.md`](docs/ui.md) for anything that renders.**

Quick orientation:

- **Ginka** is an IDE-agnostic coding-agent orchestrator written in Rust on **GPUI**. It targets the feature set of [band-app/band](https://github.com/band-app/band) as a native single binary, borrowing interaction ideas from [Orca](https://www.onorca.dev/). Everything in it is written here: no code is copied or ported in from another project.
- Milestone 0 is **largely landed** (shell, theme tokens, settings, SQLite migrations, logging, CI); `docs/roadmap.md` §5 lists what is left. Everything the UI shows is still placeholder data.
- Architecture in one line: a **GPUI app** and a **`ginka` CLI** both talk to a background **`ginka-daemon`** over authenticated WebSocket RPC; the daemon owns SQLite, agent processes, PTYs and git.
- UI: a three-column agent workstation (session sidebar / transcript + composer + terminal dock / right-panel surfaces) on a dark glass surface, specified in `docs/ui.md`. Built on **`gpui-component`**, with **`bezel`** as a design reference we rebuild from, never copy — the two link incompatible `gpui` forks and must never both appear in `Cargo.toml`.
- The sixteen requirements that shape the interfaces (steering, absorbable option changes, three-ref checkpoints, externalised attachments, the daemon-host path rule, …) are `docs/roadmap.md` §3.3, as N1–N16.
- The non-negotiable rules (daemon owns state, domain logic in `ginka-core`, CLI and UI share one protocol, workspace id derived from an immutable worktree `name`, virtualized streaming surfaces, one normalized `AgentEvent` stream, local-first) are spelled out in `AGENTS.md` § Architectural rules.

When a change alters architecture, data model or scope, update `docs/roadmap.md` — including its decision log — in the same change.
