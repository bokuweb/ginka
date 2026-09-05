# CLAUDE.md

**This project's agent instructions live in [`AGENTS.md`](AGENTS.md). Read it first, then [`docs/roadmap.md`](docs/roadmap.md) — [`docs/ui.md`](docs/ui.md) for anything that renders, and [`docs/connectors.md`](docs/connectors.md) for anything that talks to Slack.**

Quick orientation:

- **Ginka** is an IDE-agnostic coding-agent orchestrator written in Rust on **GPUI**. It targets the feature set of [band-app/band](https://github.com/band-app/band) as a native single binary, borrowing interaction ideas from [Orca](https://www.onorca.dev/). Everything in it is written here: no code is copied or ported in from another project.
- **State of play:** the daemon, the wire protocol, the agent drivers, session persistence and git checkpoints are in, and both the CLI and the app are clients of the daemon. What is missing is the chat surface that renders a transcript, and the terminal and diff work in M3. `AGENTS.md` § Current state is the short version; `docs/roadmap.md` §5 is authoritative.
- Architecture in one line: a **GPUI app** and a **`ginka` CLI** both talk to a background **`ginka-daemon`** over authenticated WebSocket RPC; the daemon owns SQLite, agent processes, PTYs and git.
- UI: a three-column agent workstation (session sidebar / transcript + composer + terminal dock / right-panel surfaces) on a dark glass surface, specified in `docs/ui.md`. Built on **`gpui-component`**, with **`bezel`** as a design reference we rebuild from, never copy — the two link incompatible `gpui` forks and must never both appear in `Cargo.toml`.
- The seventeen requirements that shape the interfaces (steering, absorbable option changes, three-ref checkpoints, externalised attachments, the daemon-host path rule, accounts per provider, …) are `docs/roadmap.md` §3.3, as N1–N17. Accounts and rate-limit headroom have their own design in `docs/accounts.md`.
- Everything written into this repository is **English** — code, rustdoc comments (`///` on every public item), docs, commit messages and PR titles/descriptions.
- The non-negotiable rules (daemon owns state, domain logic in `ginka-core`, CLI and UI share one protocol, workspace id derived from an immutable worktree `name`, virtualized streaming surfaces, one normalized `AgentEvent` stream, local-first) are spelled out in `AGENTS.md` § Architectural rules.

When a change alters architecture, data model or scope, update `docs/roadmap.md` — including its decision log — in the same change.
