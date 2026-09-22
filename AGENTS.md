# AGENTS.md

Guidance for AI coding agents (and humans) working in this repository.

## What this project is

**Ginka is an IDE-agnostic coding-agent orchestrator built in Rust on GPUI.**

It is a native reimplementation of what [band-app/band](https://github.com/band-app/band) does — manage many coding agents across many projects and git worktrees, with chat, terminal, diff review and status in one window — replacing Band's Electron + local Node server with a single Rust binary plus a background daemon.

[Orca](https://www.onorca.dev/) informs the interaction design: prompt fan-out across worktrees, diff-line comments batched back to the agent, click-an-element design mode.

**Ginka is written here.** Other clients in this space are worth reading for behaviour and for the edge cases they have already hit, and worth looking at for how they render. No code from any of them is copied, ported or paraphrased into this repository — see the roadmap's R9 and its decision log.

**Read [`docs/roadmap.md`](docs/roadmap.md) before starting any non-trivial work.** It holds the architecture, the crate layout, the data model, the milestone plan and the open decisions. This file is the short version; the roadmap is authoritative.

**For anything that renders, read [`docs/ui.md`](docs/ui.md) too.** It holds the layout spec, the design tokens, the region-by-region breakdown, and the mapping from each region to the component that draws it.

**For anything that talks to a chat platform, read [`docs/connectors.md`](docs/connectors.md).** It is the design for the Slack connector — where it lives, what a message becomes, what the thread sees, and the security rules — and it is not implemented yet.

**For anything touching logins, `CLAUDE_CONFIG_DIR` / `CODEX_HOME`, or rate-limit headroom, read [`docs/accounts.md`](docs/accounts.md).** It is the design for several accounts per provider (N17), what each vendor can report about its windows, and why switching between accounts is manual.

## Current state

**M0 and M1 are largely landed, and M2's process split is in.** See `docs/roadmap.md` §5 for what each milestone still owes. What works today:

- A **daemon** owns the state: SQLite, git and agent processes. It binds loopback, publishes its port and a bearer token to `~/.ginka/daemon.json`, and pushes every mutation to connected clients with a sequence number and a bounded replay window.
- The **CLI and the app are both clients of it.** `ginka` registers projects, creates and removes worktrees, starts agents, reads transcripts, lists and restores checkpoints — and starts the daemon when there is none.
- **Agents run.** `claude` and `codex` drivers normalize their output into one `AgentEvent` stream; a turn is one process, follow-ups resume the vendor's session, and cancelling signals the process group. Transcripts are persisted as events.
- **Claude delegated agents are visible.** One parent row owns a bounded trail of the child's reasoning, messages and tool lifecycle, and is settled if the turn ends before a final report. Codex child-thread reporting waits on its app-server transport rather than guessing at `exec --json` output.
- **Checkpoints**: the worktree is snapshotted before the first turn and at every turn boundary, as a commit on no branch, and can be restored.
- The shell renders a project rail, a session list that opens for the selected project, conversation and resizable right panel, with a composer, context bar, terminal dock and surface chooser. Right-panel and terminal visibility/dimensions plus the active surface persist per workspace; sidebar navigation remains global. Projects are added through one name-and-source-folder modal. Its palette and type bases share e1's token contract for future integration. **Not yet visually signed off** (see roadmap M0).

- **The window is live.** The centre column draws the selected workspace's transcript, folded from the daemon's events and followed off its push stream; readable messages can be copied or quoted into the durable draft. Claude and Codex task updates share one bounded live Tasks card, remain searchable after persistence and carry into cross-provider handoffs. Each turn boundary retains the provider, model, reasoning effort and service tier that actually started it. The composer starts an agent or sends a follow-up, picks which agent and model answer, and stops one that is working. Codex context readings expose an idle-only manual compaction control; unsupported providers never receive a guessed command. A turn boundary is where a checkpoint was taken, so it is also the way back to it.
- **Scratch workspaces**: `ginka workspace scratch` makes somewhere to work with no repository at all, and a plain folder is its own workspace.

- **English and Japanese.** Every user-visible string is in `locales/app.yml` in both; the language follows `app.json`, then the environment, then English.

- **The review loop.** The right panel separates unstaged and staged diffs, marks replacements word by word, supports file- and hunk-level staging, a file revert, recent commit history, and comments anchored to lines that go back to the agent as one message. Pull is deliberately clean and fast-forward-only so divergence remains an explicit decision. `ginka review`, `stage`, `stage-hunk`, `revert`, `commit`, `history`, `pull`, `push`.
- **Terminals.** A strip of shells per workspace, owned by the daemon, with a bounded scrollback replayed to a window that comes back to them.
- **Finding and editing things.** A files surface shows a bounded, expandable file tree and searches the worktree by path (`nucleo`) and by content (`git grep`) from the same box. Its scope chips can search every active worktree in the selected project and open a hit by switching to its owning workspace. It keeps several independently editable `CodeEditor` tabs with back/forward visit history, previews the live Markdown buffer, finds/replaces, and saves through the daemon without overwriting a newer revision. Installed local language servers provide hover, workspace-local definition jumps across tabs and diagnostics for Rust, TypeScript/JavaScript, Python and Go; an absent server falls back to syntax-only editing. A saved selection can be added to chat as an exact file-line mention; dirty tabs cannot be closed silently. Markdown preview never loads images named by repository text. `ginka files [--limit]`, `ginka project search`, `ginka save` and the MCP file tools expose the same operations. ⌘K reaches every action, panel, surface and workspace by name; the title-bar arrows and ⌘[ / ⌘] traverse bounded project/session history; ⌘1–9 selects a visible session in the current project; ⌘⌥←/→ cycles surfaces.
- **MCP.** `ginka mcp` serves the same requests to an agent over stdio, so rule 3's third client is real: what a person can do, an agent can — including a **fan-out**, which asks one question in a worktree per attempt.
- **Slack.** A bound channel starts or continues a `claude` or `codex` turn on this machine over Socket Mode, and the answer goes back to the thread, with a 👀 while it works and a progress line edited in place. Sessions carry an `origin`; the daemon hosts the connector as a fourth client of the protocol; `ginka slack status` says whether it is connected and why not. `docs/connectors.md` is the design and §12 there is where the code departs from it.
- **Moving a conversation.** `ginka session fork --agent codex` (or `--account`) copies the record onto another agent and hands it a bounded digest of the transcript with its first prompt, because the vendor's thread cannot follow (`ginka-core::handoff`, roadmap §4.4). The window offers ready alternative agents at every completed turn and forks through that exact transcript position.
- **Branches.** `ginka workspace branches|checkout [--create]` and the same over MCP; the context-bar branch picker uses those requests to search, switch or create. A checkout changes the branch column and nothing else (rule 4).
- **Semantic indexing.** `ginka workspace index`, the palette, and the context-bar index status use the same daemon operation. `WorkspaceSummary.indexed` is the daemon-host filesystem truth; the command runs in a visible daemon terminal.
- **Conversation search.** `ginka session search`, the MCP request, ⌘F and the command palette use the same daemon-side transcript search. The window restricts results to the open session, moves through them in transcript order and jumps to the folded block that owns each persisted sequence.
- **Commit messages (N9).** `ginka commit <workspace> --generate` has an agent write the subject on its cheap tier; the daemon pushes it as `CommitMessageGenerated` and the CLI commits with it.
- **Access mode (N2).** `session start --access read-only|ask|auto`, the same over MCP, and a chip in the composer. Stored on the session; a follow-up runs under the mode the conversation began in. Claude gets `--permission-mode`, Codex `--sandbox read-only` / `--full-auto`.
- **Interactive responses.** Ask-user, plan and permission events pause a session; answers from the cards or composer, `session respond`, MCP, and Slack are validated against the open request and delivered into the live transport. A scripted driver pins the loop; the shipped headless drivers do not raise native requests yet.
- **Skills.** `ginka skills list|enable|disable`, `ginka_skills` over MCP, and the **Skills** surface manage the agents' own skills across every ecosystem's roots, under the home and under each project (N11). Duplicate installs are one row and its toggle changes every copy without deleting any; the window searches names, descriptions, roots and paths, filters by scope/state, and copies daemon-host paths.
- **Accounts.** Several logins per provider (`docs/accounts.md`, N17): `ginka account add|list|login|remove|refresh`, `session start --account`, an *Add a login…* row in the pickers, and an account chip in the composer when a provider has more than one. A separate usage chip stays visible with one login too, combining the active session's live token count, provider-reported current context occupancy and the tightest rate-limit window. A session records the login it ran on, usage is filed by it, and each login's rate-limit windows — Codex's through its app server, Claude's from a refused turn — are shown beside the choice, in the sidebar footer and on the **Reports** surface.

What is *not* there yet: dockable/persisted centre and right surfaces (M4), a virtualized transcript, split diffs, terminal splits and scrollback search, a shipped interactive transport for plan/ask-user/permission requests, and the drivers beyond `claude` and `codex` (M5).

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
| `ginka-core::handoff` | the digest a conversation is moved to another agent with, and what it keeps when it does not fit |
| `ginka-core::tools` | which MCP servers an agent is handed, and how each driver puts them on its command line |

`ginka-core::driver::testing::ScriptedSession` is how session behaviour is tested — never a live vendor CLI.

## Commands

```bash
cargo run                                   # the desktop app
cargo test --workspace                      # run this rather than `-p`: the CLI's
                                            # tests start the daemon binary next to it
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all

cargo run -p ginka-cli -- doctor            # where state lives and whether it is healthy
cargo run -p ginka-cli -- daemon status
cargo run -p ginka-cli -- project add .
cargo run -p ginka-cli -- workspace new <project> <branch>
cargo run -p ginka-cli -- workspace archive <workspace> [--restore]
cargo run -p ginka-cli -- session start <workspace> "<prompt>" --model <model> --reasoning-effort high --service-tier priority --access auto
cargo run -p ginka-cli -- session options <session> --model <model> --reasoning-effort high --service-tier priority
cargo run -p ginka-cli -- session respond <session> <request-id> "<answer>"
cargo run -p ginka-cli -- account add codex-work --provider codex --label Work
cargo run -p ginka-cli -- account login codex-work   # the vendor's sign-in, here
cargo run -p ginka-cli -- account list               # signed in, and headroom
cargo run -p ginka-cli -- session log <session>
cargo run -p ginka-cli -- session compact <session>
cargo run -p ginka-cli -- checkpoint list <workspace>
cargo run -p ginka-cli -- --json project list   # the protocol's own shapes, for agents
cargo run -p ginka-cli -- mcp                   # serve those shapes to an agent over MCP
cargo run -p ginka-cli -- slack status          # the Slack connector, and why it is not running
cargo run -p ginka-cli -- slack allow U01ABC2   # let one more member speak to it
cargo run -p ginka-cli -- session fork <session> --agent codex   # move a conversation to another agent
cargo run -p ginka-cli -- skills list           # the agents' own skills, and whether each is on
cargo run -p ginka-cli -- workspace branches <workspace>          # and `checkout <branch> --create`
cargo run -p ginka-cli -- commit <workspace> --generate           # an agent writes the message
cargo run -p ginka-cli -- pull <workspace>                        # clean fast-forward only
cargo run -p ginka-cli -- history <workspace>                     # bounded recent commits
cargo run -p ginka-cli -- stage-hunk <workspace> <path> '<header>' # exact partial stage
cargo run -p ginka-cli -- workspace index <workspace>            # zg index, so agents get semantic search
cargo run -p ginka-cli -- project search <project> <query>       # search every active worktree
cargo run -p ginka-cli -- --json show <workspace> <path>         # includes the revision required to save
cargo run -p ginka-cli -- save <workspace> <path> --expected-revision <revision> < replacement

cargo run -p ginka-protocol --features export --bin export-types   # TypeScript bindings
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
│  ├─ ginka-core/       # domain: projects, worktrees, sessions, drivers, git,
│  │                    # checkpoints, and the daemon's request handling
│  ├─ ginka-daemon/     # binary: WebSocket RPC server, owns SQLite + processes,
│  │                    # hosts the Slack connector
│  ├─ ginka-client/     # async RPC client used by the app and the CLI
│  └─ ginka-cli/        # binary: the `ginka` command
├─ db/migrations/       # SQL migrations, embedded at compile time
├─ locales/app.yml      # every user-visible string, en and ja side by side
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
- **i18n:** user-visible strings go through `rust-i18n`. `en` and `ja` are both maintained.
- **a11y is a rule, not a polish pass.** Every control reachable by mouse is reachable by keyboard with visible focus; decorative animation honours the system reduce-motion setting; nothing encodes meaning in colour, hover or motion alone. Roadmap §6.4.
- **English in the repository.** Code, comments, docs, commit messages and pull requests are written in English, no matter what language the conversation that produced them was in.
- **Commits and pull requests:** imperative subject, explain *why* in the body. Reference the roadmap milestone when the change advances one. A PR description says what changed and what it is for, in the same voice as the commit.
- **Comments are rustdoc.** Every public item — module, type, trait, function, field, variant — carries a `///` comment; every crate and module root carries a `//!` header saying what lives there and what it owns. Document what a caller must know: invariants, panics, errors, units, and the constraint that made the code look the way it does (why the workspace id is derived from `name`, why a poll is throttled). Do not restate what the signature already says, and do not leave a public item undocumented because it looks obvious.

## Working agreements for agents

- When a change alters architecture, data model, or scope, **update `docs/roadmap.md` in the same change** — including the decision log at the bottom. When it alters layout, tokens or component choices, update `docs/ui.md`.
- Do not silently expand scope. The roadmap's §3.2 non-goals and the milestone ordering are deliberate; if something seems missing, it is probably deferred on purpose.
- Prefer reading prior art for behaviour before designing from scratch — Band and the other open agent clients have already hit the edge cases (worktree sync, terminal reattach, session resume). **Read them; write our own.** Take the requirement away from the reading and implement it here; do not copy, port or paraphrase a file with it open. Most of that prior art is GPL-3.0, so copying would settle the licence question (roadmap Q3) by accident — but the rule holds for permissively licensed code too, because an implementation we did not write is one we cannot debug.
- The requirements that constrain interfaces live in roadmap §3.3 as N1–N17. If you are about to design something that sounds like one of them — steering, session options, checkpoints, attachments, titles, updates — read that row first.
- Keep this file and `CLAUDE.md` truthful. If you add commands, add them here once they actually work.
