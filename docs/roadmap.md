# Ginka Roadmap

> Status: **M0 landed except the items still unchecked in §5**. This document is the plan the milestones execute against.
> Last updated: 2026-09-04

## 1. Vision

**Ginka is an IDE-agnostic coding-agent orchestrator, written in Rust on top of [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui).**

The product target is functional parity with [band-app/band](https://github.com/band-app/band) — run many coding agents across many projects and git worktrees, watch their status in one place, and review/steer their work without leaving the app — but delivered as a single native binary instead of an Electron shell in front of a local Node server.

Three properties drive every decision below:

1. **Native and fast.** One process tree, no bundled Node runtime, no browser engine for the app chrome. Cold start and pane switching should feel like a terminal, not a web app.
2. **Local-first.** All state lives on the user's disk (`~/.ginka/`). No account, no remote service required for any core feature.
3. **Worktree-per-task.** A "workspace" is a git worktree. Isolation is the unit of parallelism, and every agent, terminal, diff and browser pane is scoped to one.

## 2. What we take from each reference

### 2.1 [Band](https://github.com/band-app/band) — the feature target

Band is the shape we are reproducing. Its surface, derived from its `apps/web` routers, services and docs:

| Area | Band capability | Ginka intent |
| --- | --- | --- |
| Projects | Register git repos *and* plain folders (`kind: git \| plain`), default branch, labels, sort order | Same model, same discriminator |
| Workspaces | Worktree per workspace, immutable `name` identity separate from live `branch`, pin, setup runner that copies untracked files into new worktrees | Same, including the name/branch split |
| Agent status | Per-workspace agent status + summary + last activity, "needs attention" detection, status event bus | Same, pushed over the daemon event stream |
| Branch status | dirty / conflict / ahead / behind / sync state, CI state + URL via GitHub GraphQL, polled with throttling | Same, background poller in the daemon |
| Chat | Multi-agent chat panes per workspace, streaming events, queued follow-ups, plan approval, ask-user-question, tool-call rendering, slash commands, `@file` mentions, session history/resume | Same event model, GPUI-rendered transcript |
| Terminal | PTY pool, tabs, parking/reattach, replay + width sync, file links, selection → chat | Same, `alacritty_terminal` + `portable-pty` |
| Changes | Changes file tree, diff view, revert file, commit dialog, promote-to-git | Same |
| Code | File browser, quick open, content search (ripgrep), editor with LSP, file tabs, markdown/image/PDF preview | Same, minus PDF initially |
| Layout | Dockview-style split layout persisted per workspace, panel visibility, maximize | Own GPUI dock implementation, same persistence semantics |
| Cronjobs | Cron-scheduled prompts, scoped to project or workspace, dispatched `via: chat \| terminal`, overlap-skip | Same |
| Reports | Token/cost usage events, on-disk session scanner with per-(workspace, agent) watermark, 30-day retention | Same |
| Browser | Embedded browser pane per workspace, history with frecency, CDP proxy, screencast | Deferred to M5, macOS first |
| Remote | Tunnel + QR for phone access, mobile layout | Deferred; see §7 open questions |
| CLI | Rust CLI driving the same backend, plus agent skills that call it | First-class; the daemon RPC is the CLI's API too |
| MCP | MCP server exposing app control to agents | Same, on the daemon |

### 2.2 [Orca](https://www.onorca.dev/) — the interaction ideas

Orca is the sharpest expression of "the IDE is for agents, not for humans typing". Ideas we adopt:

- **Fan-out.** Send one prompt to N agents in N worktrees, then compare and merge the winner. Band has multi-agent chat; Orca makes the *comparison* first-class. This becomes Ginka's `fan-out` command in M4.
- **Diff review as agent input.** Drop markdown comments on diff lines, batch them, and send the batch back to the agent as a single message. This is a better feedback loop than re-prompting from scratch, and it is cheap on top of our diff view.
- **Design mode.** Click an element in the embedded browser to hand its HTML/CSS/screenshot to the agent. Deferred with the browser pane (M5).
- **Split-pane arrangement that matches task complexity** and cross-worktree native search.
- **Explicitly rejected for v1:** SSH worktrees and the mobile companion apps. Both are large surface areas that do not serve the core loop.

## 3. Scope

### 3.1 v1.0 definition of done

A user can: register a project → create a worktree workspace → start a Claude Code (or Codex) session in it → watch status across all workspaces → read the diff → comment on the diff and send it back → run terminals → commit → and drive all of the above from `ginka` on the command line. On macOS, signed and auto-updating.

### 3.2 Non-goals for v1

- Being a general-purpose IDE. The editor exists to read code and make small corrections, not to replace Zed/VS Code.
- A hosted/multi-user service, accounts, or team sync.
- Windows/Linux parity at v1 (they are supported targets, but macOS ships first).
- Mobile clients, SSH worktrees, Linear/Jira integration.

### 3.3 Requirements that shape the architecture

Sixteen requirements that are cheap to design in and expensive to add afterwards. They are listed here rather than left inside a milestone because each one constrains an interface — a trait signature, the wire protocol, the git ref layout, the update contract — and a milestone list is where that gets forgotten. Every one is scheduled in §5, and the numbers (N1…) are what the milestones reference.

| # | Requirement | Why it has to be decided now | Lands in |
| --- | --- | --- | --- |
| N1 | **Steer a running turn.** Where the transport can inject a user message into the turn that is already in flight, a follow-up goes straight in and its outcome comes back asynchronously. The queue is what we fall back to, not what we build first | "Stop, retype, resend" is exactly the friction this app exists to remove. Whether a driver can steer is a capability on the trait, so retrofitting it means changing every driver at once | M2 |
| N2 | **A driver decides whether an option change is absorbable.** Model, reasoning effort and service tier should ride on the next turn wherever the transport allows it; changing the access mode or the provider always restarts the session | If the UI assumes one answer, every model switch either kills a session that did not need killing or silently keeps a setting the agent never received. The matrix differs per transport, so the decision belongs behind the trait | M2 |
| N3 | **The model catalogue is asked of the CLI**, with a static table as the fallback when the probe fails or times out | A hardcoded model list is wrong the week after a vendor ships. The fallback is what keeps the picker usable offline and on an unknown CLI version | M2 |
| N4 | **Slash commands come from the provider as well as from disk**, and the `@file` index is bounded | A provider defines half its own commands; only reading disk shows the user a lie. An unbounded index is a hang on a generated monorepo, and the bound has to exist before the index does | M2 |
| N5 | **Two title fields: one the user sets, one the agent supplies.** The user's always wins; the agent's replaces a first-prompt placeholder silently, and how fast it lands is the thing to optimise | Most agents already name their own sessions on a cheap model as part of work they are doing anyway. Paying a second model for a title we could have read is waste, and a title that arrives after the turn ends is indistinguishable from no title at all | M2 |
| N6 | **Attachments and agent-emitted images live outside the transcript.** Uploads are daemon-owned and referenced by URI; images above a small threshold go to a content-addressed store and are referenced, never inlined | Inline base64 inflates the payload by a third, is re-decoded on every render, and turns one screenshot-heavy session into megabytes of state. This is a storage-shape decision, and reshaping stored transcripts later is a migration | M2/M3 |
| N7 | **Diffs are addressable by source**, including *this turn* alongside uncommitted / unstaged / staged / committed / branch | "What did this turn change" is the review loop's first question and cannot be answered from `git status`. It also only works if the turn's base ref was captured while the turn ran (N8) | M3 |
| N8 | **A checkpoint is three refs per turn**, not one: the state accepted for the turn before the agent ran, the state it ended at, and the base its diff is taken against | A branch switch or a hand edit made in the terminal between turns must not be attributed to the agent. One ref cannot tell them apart, and the refs have to be written at the time — they cannot be reconstructed later | M3 |
| N9 | **Commit-message generation runs on a cheap tier at the lowest effort it accepts**, independent of the session's model, over a size-capped diff | The subject line is a fixed classification over a diff that is already in the prompt. Running it on the session's frontier model is pure cost, and the cap is what stops a large refactor from failing the request | M3 |
| N10 | **Transcript search is answered by the daemon**, not by the client | Sessions run for hours and the client does not hold the messages — scrollback is not a search index. Client-side search would force the transcript into memory, which contradicts §6.2 | M4 |
| N11 | **The agents' own skills are manageable from the app**: discover them across every ecosystem's roots, collapse duplicate installs of the same skill, enable and disable without deleting anything | This is a different feature from the skills we ship for agents to drive Ginka (M4), and both are wanted. Users already do this by hand; doing it by hand is what makes it error-prone | M4 |
| N12 | **Usage shows rate-limit headroom, not only totals** — how much of the current window is spent and when it resets | Mid-task the question is "how close am I to the wall", and token totals do not answer it | M5 |
| N13 | **Costs are priced from a public rate table**, cached locally, with the page stating its own confidence when the table is stale or a model is unknown | Hardcoded prices are wrong within weeks, and a usage page that quietly reports a wrong number is worse than one that says "at least this" | M5 |
| N14 | **Providers are configurable: disabled individually, and their binary path overridable** | Agent CLIs are installed through version managers, nix profiles and plain checkouts. Without an override, a failed autodetect is a dead end with no way out from inside the app | M2 |
| N15 | **One signed update contract across all three platforms**, and an install that a package manager owns updates through that manager rather than in place | Two update mechanisms means two trust roots and twice the release tooling. Overwriting package-manager-owned files is the failure that loses a user's trust exactly once | M6 |
| N16 | **The daemon ships and is signed with the app**, and development uses a separately named daemon build | Two binaries in one bundle is a signing and a discovery problem, and it is far cheaper to answer before notarisation than during it. The dev split is what keeps a daemon-side edit from forcing an app relaunch | M6 |

## 4. Architecture

### 4.1 Process model

```
┌─────────────────────────────┐        ┌──────────────────────────────────────┐
│  ginka (GPUI app)           │  WS    │  ginka-daemon                        │
│  - windows, panes, input    │◄──────►│  - SQLite (~/.ginka/ginka.db)        │
│  - view state only          │  RPC   │  - agent process supervision         │
└─────────────────────────────┘        │  - PTY pool                          │
┌─────────────────────────────┐        │  - git / worktree ops                │
│  ginka (CLI)                │◄──────►│  - watchers, pollers, cron, MCP      │
└─────────────────────────────┘        └──────────────────────────────────────┘
```

- One daemon per user, auto-spawned by the app or the CLI, discovered via `~/.ginka/daemon.json` (port + token). Loopback only, bearer-token authenticated.
- The daemon survives the UI closing, so agents keep running.
- Everything the UI can do, the CLI can do, because they speak the same protocol. This is what makes agent-driven self-control (Band's `skills/band-*`) possible.
- **The protocol is versioned and bounded.** A `PROTOCOL_VERSION` constant is exchanged in the handshake and a mismatch fails loudly rather than half-working; a maximum wire-message size is enforced on both ends, because attachments and screenshots travel over this socket. `GINKA_DAEMON_ADDRESS` and `GINKA_DAEMON_TOKEN` override discovery, which is what makes a daemon outside the app testable at all.
- **Every path the daemon returns refers to the daemon's host, not the client's.** Today they are always the same machine, but the moment a client interprets a daemon path as local, remote access (Q1) stops being an additive change. The rule costs nothing now: paths cross the wire as daemon-host paths, and anything that needs a local path — the folder picker, opening a file in an external editor — asks whether the daemon is the local child process first.

### 4.2 Crate layout

```
ginka/
├─ Cargo.toml                # workspace root; the GPUI binary lives here
├─ crates/
│  ├─ ginka-protocol/        # serde wire types, RPC envelopes, ts-rs export bin
│  ├─ ginka-core/            # domain logic: projects, worktrees, sessions, git,
│  │                         # drivers/, terminal, cron, usage, checkpoints
│  ├─ ginka-ui/              # design tokens, assets, view models -- everything
│  │                         # UI-side that is testable without a window
│  ├─ ginka-daemon/          # binary: WS server + supervision + MCP endpoint
│  ├─ ginka-client/          # async RPC client used by app and CLI
│  └─ ginka-cli/             # binary: `ginka` command
├─ src/                      # GPUI app: views only (shell, sidebar, surfaces)
├─ assets/                   # icons, fonts, themes
├─ db/migrations/            # SQL migrations
├─ locales/                  # rust-i18n yml (en, ja)
└─ docs/
```

### 4.3 Key dependencies (pinned in M0)

| Concern | Choice | Note |
| --- | --- | --- |
| UI framework | `gpui` — whichever rev our component library pins (see §4.6) | Not on crates.io. We do **not** choose this independently; the component library's pin wins. |
| UI components | `gpui-component` (git, `longbridge/gpui-component`) | Dock layout, virtualized list/table, code editor with tree-sitter + LSP, markdown, charts, webview. See §4.6. |
| Design reference | `bezel` (MIT, `crabtalk/bezel`) | **Not linked, and not copied** — see §4.6 and risk R7. A look reference for the glass theme, the status glyphs and the composer affordances, all of which we implement ourselves. |
| Async | `smol` + GPUI's executor | Matches GPUI's model; avoid dragging in a second reactor. Tokio only where a dependency demands it. |
| DB | `rusqlite` + `refinery` (or `sqlx` w/ offline mode) | Decision due in M0. Bundled SQLite either way. |
| Terminal | `alacritty_terminal` + `portable-pty` | The stack Zed runs on. |
| Git | `gix` for reads (status, log, diff), `git` subprocess for worktree/commit/push | `gix` is fast and pure-Rust; worktree plumbing is safer via the real binary. |
| Diff | `similar` | Word-level intra-line diff. |
| Search | `grep` crates (ripgrep as a library) | Avoids shipping a second binary. |
| Fuzzy | `nucleo-matcher` | Quick-open, command palette, file mentions. |
| Buffers | `ropey` | |
| Syntax / LSP | via `gpui-component`'s `CodeEditor` (tree-sitter + LSP built in) | Removes most of the M4 editor work; see §4.6. |
| i18n | `rust-i18n` | en + ja from the start. |
| Serialization | `serde`, `serde_json`, `ts-rs` | |

### 4.4 Data model

SQLite, migrations under `db/migrations/`. Tables (Band's schema is a good starting point and we deviate only where noted):

- `projects` — `name` (PK), `path`, `default_branch`, `label`, `sort_order`, `kind` (`git` \| `plain`), `has_origin`.
- `worktrees` — `id`, `project_name` (FK, cascade), `name` (immutable workspace identity, slugified from the creating branch), `branch` (live, reconciled against git), `path`, `head`, `pinned`. **The workspace id derives from `name`, never from `branch`** — switching branches inside a worktree must not re-key everything.
- `workspace_statuses` — agent name/status/summary/last-activity per workspace.
- `branch_statuses` — dirty, conflict, ahead, behind, sync state, CI state + URL.
- `tasks` — one agent invocation: prompt, status, session id, model, mode, agent id, chat id, timestamps.
- `chats` / `messages` — transcript persistence, chunked for pagination.
- `panel_states` — per-workspace, per-panel JSON blob + free-form `labels` map (the `ginka:` key prefix is reserved for internal dispatch keys).
- `cronjobs` — cron expression, scope (project \| workspace), `via` (chat \| terminal), enabled, last run + status, last terminal id for overlap-skip.
- `usage_events` — input/output/cache/reasoning tokens + cost, with `external_key` unique index for the disk scanner's dedup; `usage_scan_state` holds the per-(workspace, agent) watermark. 30-day retention sweep.
- `sessions` / `messages` — transcript persistence. Sessions carry the two title columns below; messages carry a per-session `seq` so a transcript pages without depending on global insert order, and search runs here (§3.3 N10).
- `checkpoints` — **three refs per turn**, not one: the state accepted for the turn *before* the provider ran, the ending checkpoint, and the base the turn's diff is taken against. Refs live under a private `refs/ginka/` namespace keyed by session and turn. One ref per turn cannot tell an agent's edits apart from a branch switch or a hand edit made in the terminal between turns, and would attribute both to the agent.
- `browser_history` — (workspace, url) unique, `visit_count` / `last_visited_at` for frecency. M5.
- `attachments` — daemon-owned uploads referenced from a message by a `ginka-attachment:` URI, bounded per file and per directory upload. The daemon stores the bytes; the transcript stores the reference.
- Provider-emitted images (`data:` URLs from screenshots and tool output) never land in the transcript inline. Above a small size threshold they are externalised into a content-addressed blob directory and referenced as `ginka-blob:`. Inline base64 inflates the payload by a third and is re-decoded on every render — this is the single change that keeps a computer-use-style session's state readable.
- Sessions carry **two title fields**: a user-set `title` and a provider-set `auto_title`, resolved title → auto-title → default, so a provider's title can never overwrite a name the user typed. The first prompt's opening words fill `auto_title` as a placeholder until the provider's own title arrives.

### 4.5 Agent driver abstraction

```rust
trait AgentDriver {
    fn id(&self) -> &'static str;                    // "claude" | "codex" | ...
    fn probe(&self) -> Result<ProbeResult>;          // installed? authed? version?
    fn models(&self) -> Result<Vec<ProviderModel>>;  // asked of the CLI, static fallback
    fn start(&self, spec: SessionSpec) -> Result<Box<dyn AgentSession>>;
    fn resume(&self, cursor: ResumeCursor) -> Result<Box<dyn AgentSession>>;
}

trait AgentSession {
    /// Can a user message be injected into the turn that is already running?
    fn supports_steer(&self) -> bool;
    fn steer(&mut self, message: &str) -> Result<()>;   // answered by an event
    /// Did the transport absorb the change, or must the session be restarted?
    fn apply_options(&mut self, options: &SessionOptions) -> Result<OptionOutcome>;
    fn cancel(&mut self) -> Result<()>;
}
```

The trait is synchronous. A driver owns a child process and a reader thread and hands events to the daemon over a channel; the calls above are short and the waiting happens on the thread, so `async` here would buy nothing and cost a second reactor. This matches the same reasoning that made the database synchronous (§8, Q2).

`SessionSpec` carries the binary, cwd, access mode, model, reasoning effort and service tier, plus the provider's own resume cursor. Model, effort and tier are per-provider vocabulary discovered from the CLI where it exposes one (a static table is the fallback), never a hardcoded list.

Two rules fall out of the transports, and both are cheap now and expensive later:

- **Steering is the feature; the follow-up queue is the fallback.** Where the transport can inject into a live turn, a follow-up goes straight in and the outcome arrives asynchronously. Where it cannot — or the session is still connecting, or the provider refuses — the message stays visible above the composer and opens a fresh turn once the current one settles.
- **`apply_options` decides in-session vs restart, per transport.** Most transports carry model, effort and tier on the next turn; access mode and a provider change always restart. Loosening what a *running* agent may touch deserves a fresh session even where the transport would accept it.

Every driver normalizes into one `AgentEvent` stream: `Connected`, `AvailableCommands` (the provider's own slash commands), `TurnStarted`, `TextDelta`, `Reasoning`, `ToolCall{..}`, `ToolResult`, `AskUser`, `PlanProposal`, `Permission`, `SteerAccepted`, `SteerRejected`, `AutoTitle`, `Usage`, `TurnEnd`, `SessionResult`, `ProcessExited`. Tool events normalize further into one activity shape (`Reasoning | Command | FileChange | Search | Plan | Tool`) so the transcript renders provider-agnostic rows.

Sessions are long-lived and per session, not per view: switching workspaces never touches a running one. A session's process is torn down when the user stops a turn on a transport with no interrupt, when an option changes that the transport cannot apply, on delete, on `ProcessExited`, and on an idle sweep — which skips any session with a turn in flight, so an unanswered approval is never reaped out from under the user. Finishing a turn is deliberately *not* on that list.

Two implementation paths:

1. **ACP** where the vendor speaks it — one adapter covers many agents.
2. **JSONL/stdio adapters** per vendor otherwise (Claude Code's stream-json, Codex, OpenCode, Gemini, Cursor, Amp).

Driver order of work: `claude` → `codex` → `acp` (covers several) → the rest.

### 4.6 UI stack

The UI is built on [`gpui-component`](https://github.com/longbridge/gpui-component) (Longbridge, production-tested in Longbridge Pro), with [`bezel`](https://bezel.gallery/) as a design reference we look at and rebuild from rather than copy. The visual specification lives in [`docs/ui.md`](ui.md).

**Why `gpui-component` is the linked dependency.** Four of its subsystems are things this app would otherwise have to build, and each is worth weeks:

- **Dock layout** — resizable panels, draggable tabs, persisted arrangement. This is the right panel, the terminal dock and the split panes. It deletes the "own GPUI dock implementation" line item from M4.
- **`CodeEditor`** — tree-sitter highlighting *and* LSP already wired. M4's editor and LSP work collapses into integration.
- **Virtualized `List` and `Table`** — the session sidebar and the Reports tables, meeting the performance budget without custom work.
- **`webview` crate** — the M5 embedded browser pane, which is otherwise the single hardest item on the roadmap (see R3).

Plus markdown rendering, charts, themes, CJK text handling, and a `Sidebar` component that matches the reference layout.

**Why `bezel` is *not* linked.** Bezel is a much closer fit in spirit — its crates are `theme` (including `glass.rs`), `motion`, `agent` (animated status orbs and avatars), `ui`, `syntax`, `markdown`, `editor` (with slash-command and link handling), and `terminal` (an `alacritty_terminal` view). That is almost a description of this app. But:

> **The two libraries link different, incompatible `gpui` crates.** `bezel` depends on `bezel-gpui 0.3.8+zed.82aeef` — a *republished fork* of Zed's GPUI under a different package name. `gpui-component` depends on `gpui 0.2.2` from `zed-industries/zed` git. Two package names means two distinct crates in the dependency graph, so `App`, `Window`, `Element` and every other core type are unrelated at the type level. They cannot be mixed in one binary.

So this is an either/or, not a "use both". We pick `gpui-component` because dock + editor/LSP + virtualized lists + webview are strictly more expensive to rebuild than bezel's aesthetics are to reproduce.

Bezel's licence would permit copying; we still do not. Its code targets a different GPUI fork, so nothing could be dropped in unmodified anyway, and a half-adapted copy is harder to own, debug and upgrade than an implementation we wrote against our own tokens and our own `gpui` rev. So bezel is treated the way we would treat any app whose look we admire — read how it renders, then write our own:

| Capability bezel demonstrates | We build ours in | Milestone |
| --- | --- | --- |
| Translucent window with platform vibrancy | our theme layer | M0 |
| Animated agent status glyphs | `ginka-ui` | M2 |
| Composer slash commands and file links | composer | M2 |
| Phase-based animation helpers | our motion layer | M0 |

**Revisit condition:** if bezel moves onto upstream `gpui` (or gpui-component onto bezel's fork), re-evaluate. Until then, one linked toolkit.

**Why Zed's own `editor` crate is not embedded.** Our `gpui` comes from `zed-industries/zed` at rev `ef07591` (via `gpui-component`), so depending on `editor` from that same rev would *not* hit the two-incompatible-`gpui`-crates problem that rules bezel out. It is rejected for two other reasons:

- **License.** `gpui`, `gpui_platform`, `sum_tree` and `util` are Apache-2.0, but `editor` — and `rope`, `text`, `language`, `multi_buffer`, `settings`, `theme`, `ui`, `workspace`, `project`, `client` with it — is `GPL-3.0-or-later`. Linking it makes the whole `ginka` binary GPL-3.0, which pre-empts Q3 rather than deciding it.
- **It is not a library.** `editor` declares 73 in-workspace dependencies, pulling in most of Zed's 245 crates (`client`, `project`, `workspace`, `settings`, `theme`, `ui`, `dap`, `telemetry`, `db`, `rpc`). Zed's whole workspace is `publish = false`, so there is no semver and no embedding API: `settings`, `client` and `theme` are initialized as globals, and `workspace` brings Zed's own Pane/Item/Dock model, which would compete with `gpui-component`'s `DockArea` and our theme tokens (`docs/ui.md` §2). Every rev bump would be a porting exercise.

`gpui-component`'s `CodeEditor` (Apache-2.0) covers what M4 needs — `input/editor.rs` plus completion, hover, diagnostic and code-action popovers over `lsp-types` — and that is bounded by R5's read-focused cap. Driving an external editor (`zed <file>:<line>`, `$EDITOR`, `code`, `cursor`) stays the escape hatch for real editing work; it fits the IDE-agnostic premise better than embedding one.

**Rules that follow from this:**

- The `gpui` rev is transitively owned by `gpui-component`. Never pin it independently; upgrade the component library and take its rev.
- No view hardcodes a colour, radius or duration. Everything comes from the tokens in `docs/ui.md` §2.
- Before writing a widget, check `gpui-component`'s gallery (`cargo run` in that repo) for an existing one. The four things we deliberately build ourselves are listed in `docs/ui.md` §5.

## 5. Milestones

Each milestone ends with something demoable. Checkboxes are the working task list.

---

### M0 — Foundations (target: 2 weeks)

Goal: `cargo run` opens a GPUI window with the app's chrome, and CI is green on macOS + Linux.

- [x] Cargo workspace with the five crates from §4.2, `rust-toolchain.toml`
- [x] Add `gpui-component`; take its `gpui` rev transitively. Bump procedure in `docs/gpui-upgrades.md`
- [x] App shell matching `docs/ui.md`: custom title bar, three resizable columns, composer, context bar, terminal dock, right-panel surface chooser
- [x] Design tokens: `assets/themes/{dark,light}.json` implementing `docs/ui.md` §2, installed globally and bridged onto the toolkit's `Theme` **and its derived semantic tokens**
- [x] Title bar in three regions, agent glyphs per session row, app-owned icon set
- [x] VS Code-style panel toggles: sidebar, right panel and terminal dock open and close independently by keyboard or title-bar control, keep their size while closed, and persist across restarts
- [x] Appearance follows the system setting, and an explicit choice overrides it
- [x] App-owned icon asset source layered over the toolkit's
- [x] Settings: `~/.ginka/app.json` (UI) and `~/.ginka/settings.json` (daemon), atomic writes, corrupt files fall back without being overwritten
- [x] SQLite connection + migration runner + the `projects` / `worktrees` tables
- [x] `ginka-protocol` skeleton; `ginka doctor` verifies the storage layer end to end
- [x] Logging (`tracing`) to `~/.ginka/logs/` with daily rotation
- [x] CI: fmt, clippy, test on macOS + Linux
- [x] `AGENTS.md` / `CLAUDE.md` conventions kept in sync with reality
- [ ] **Visual sign-off against `docs/ui.md` (R8).** Blocked: this environment has neither screen-recording nor accessibility permission, so the window cannot be captured or measured here. Run `cargo run` and look.
- [x] Glass window: `WindowBackgroundAppearance::Blurred` plus alpha carried in the theme's `bg.window`. Bezel's `glass.rs` turned out to be unnecessary — GPUI provides the backdrop directly
- [ ] Motion helpers of our own; 260 ms list reordering
- [ ] `export_types` binary for the protocol crate
- [ ] Hot-reload settings on change
- [ ] Crash handler
- [ ] `rust-i18n` wired up with `locales/{en,ja}.yml` — §6.4 assumes it and nothing is wired yet; every string added before it lands is one to retrofit

**Exit criteria:** window opens in <300 ms warm; the three-column shell with a resizable sidebar and right panel renders against `docs/ui.md`; theme switches; migrations run on a fresh `~/.ginka`.

---

### M1 — Projects, worktrees, dashboard (target: 3 weeks)

Goal: the workspace list from Band's dashboard, fully working, with no agents yet.

- [ ] Project registry: add/remove/rename, `git` vs `plain` kind detection, default-branch probe, labels, reordering
- [ ] Worktree lifecycle: create (branch from base), remove (incl. detached/locked handling), pin, prune
- [ ] Setup runner: `.ginka/config.json` per project — copy untracked files (`.env`, etc.) and run setup commands on worktree creation
- [ ] `syncWorktrees` equivalent: reconcile DB against `git worktree list` on a tick, updating `branch` / `head` / `has_origin`
- [ ] Branch status poller: dirty/conflict/ahead/behind, throttled
- [x] Sidebar per `docs/ui.md` §3.2: three-line rows, status pills, archived section, attention sort, user footer — now fed by real projects and worktrees, with a first-run empty state that names the command to fix it
- [ ] Virtualize the session list; animate the reorder on the 260 ms curve
- [x] `ginka project add|list` and `ginka workspace list|new|remove`
- [ ] Command palette + global keymap infrastructure
- [ ] Workspace picker / quick switcher

**Exit criteria:** create and delete 20 worktrees across 3 projects; state survives restart; sync self-heals after manual `git worktree add` outside the app.

---

### M2 — Daemon split + first agent (target: 4 weeks)

Goal: a real agent runs in a worktree and its transcript renders.

- [ ] Extract the daemon: WS RPC server, bearer token, `~/.ginka/daemon.json` discovery, auto-spawn + health check + graceful takeover
- [ ] Request/response + server-push event streams with sequence numbers and replay cursors (so a reconnecting UI never misses events)
- [ ] `AgentDriver` trait + `claude` driver (stream-json over stdio), process supervision, cancellation
- [ ] Session persistence: tasks, chats, messages; resume from vendor session id
- [ ] Chat pane: streaming transcript, tool-call cards, reasoning blocks, virtualized list, pagination
- [ ] Composer: `@file` mentions, slash commands, drafts persisted per workspace, message queueing while the agent is busy
- [ ] Plan approval and ask-user-question interaction modes
- [ ] Agent status + "needs attention" derivation, surfaced back on the dashboard
- [ ] `codex` driver
- [ ] **Steering (N1):** inject a follow-up into the running turn where the transport supports it, with the queue as the declared fallback and the outcome surfaced in the transcript
- [ ] **Session options (N2, N3):** model / reasoning effort / service tier / access mode picker, catalogue discovered from each CLI with a static fallback, and an `apply_options` path that restarts only when the transport cannot absorb the change
- [ ] **Composer completion (N4):** provider-reported slash commands merged with the on-disk scan; `@file` index bounded
- [ ] **Session titles (N5):** provider `auto_title` vs user `title`, first-prompt placeholder, replaced silently when the provider's own title lands
- [ ] **Attachments (N6):** paste/drag images and files into the composer, stored by the daemon and referenced by URI; provider-emitted images externalised to the blob store
- [ ] **Provider settings (N14):** disable a provider, override its binary path, from `~/.ginka/settings.json` and the settings UI
- [ ] Protocol handshake carries a version and a wire-size bound (§4.1); daemon-host path rule honoured by every client path

> **Landed already (domain layer, ahead of the milestone):** N1 steering policy and the driver/session traits (`ginka-core::driver`), N2 the in-session-vs-restart rule, N3 the model/effort/tier vocabulary (`ginka-protocol::provider`), N4 completion triggers, command merge and the bounded file index (`ginka-core::composer`), N5 titles (`ginka-protocol::session`), N6 attachments and the blob store, N14 provider settings. What remains for M2 is the daemon, the wire and the first real driver behind them.

**Exit criteria:** two agents run concurrently in two worktrees for 30+ minutes; killing and restarting the UI loses no transcript; cancel actually kills the process tree.

---

### M3 — Terminal, changes, review loop (target: 4 weeks)

Goal: the loop that makes the app useful daily — read the diff, comment, send back.

- [ ] PTY pool in the daemon; terminal grid view in GPUI (`alacritty_terminal`)
- [ ] Terminal tabs, splits, scrollback search, parking/reattach with replay + width sync
- [ ] File-path detection in terminal and chat output → click opens the file
- [ ] Selection → "add to chat" / "add to terminal"
- [ ] Changes panel: status tree, per-file diff (unified + split), intra-line word diff, revert file
- [ ] Commit dialog: stage/unstage, message composer, agent-generated message
- [ ] **Diff review comments (Orca):** anchor markdown comments to diff lines, batch them, send the batch as one agent message
- [ ] **Checkpoints (N8):** three refs per turn — accepted-state, ending checkpoint, diff base — under `refs/ginka/`; rewind from any transcript position
- [ ] **Turn-scoped diffs (N7):** the changes panel selects its source — this turn, uncommitted, unstaged, staged, committed, branch
- [ ] **Commit messages (N9):** generated on a cheap tier at the lowest effort it accepts, independent of the session's model, over a size-capped diff

> **Landed already (domain layer):** N7 diff sources (`ginka-core::review`), N8 three-ref checkpoints and rewind (`ginka-core::checkpoint`), N9 the commit-message policy, prompt and commit path (`ginka-core::commit`). What remains is the terminal, the panels and the review interaction on top of them.

**Exit criteria:** a full task cycle — prompt → agent edits → review with 3 line comments → agent revises → commit — without touching another app.

---

### M4 — Code surface + layout + CLI (target: 4 weeks)

Goal: stop context-switching to an editor for reads, and make the app scriptable.

- [ ] File tree, quick open (nucleo), content search (ripgrep-as-library), cross-worktree search
- [ ] Integrate `gpui-component`'s `CodeEditor`: file tabs, editor history (go back/forward), markdown + image preview
- [ ] LSP wiring through `CodeEditor`: go-to-definition, hover, diagnostics
- [ ] Surfaces: right-panel dock + centre dock via `gpui-component` `DockArea`, per-workspace persistence, panel visibility rules, `docs/ui.md` §3.4 empty state
- [ ] `ginka` CLI covering projects, workspaces, chats, terminals, settings, cron
- [ ] Agent skills that drive the CLI (`ginka-start`, `ginka-chat`, `ginka-terminal`, `ginka-loop`)
- [ ] MCP server on the daemon exposing the same operations to agents
- [ ] **Fan-out (Orca):** one prompt → N worktrees → side-by-side comparison view → merge the winner
- [ ] **Transcript search (N10):** daemon-side search across a session's messages, with in-transcript jump
- [ ] **Skills library (N11):** discover `SKILL.md` across every ecosystem's roots, group duplicate installs by name, enable/disable by renaming. Distinct from the Ginka-driving skills above — this manages the agents' own

> **Landed already (domain layer):** N10 transcript storage and search (`ginka-core::transcript`), N11 skill discovery and enable/disable (`ginka-core::skills`).

**Exit criteria:** an agent can create a workspace, start a sibling agent, and read its diff entirely through the CLI/MCP.

---

### M5 — Depth (target: 4 weeks)

- [ ] Cronjobs: scheduler, project/workspace scope, `via: chat | terminal`, overlap-skip, run history
- [ ] Reports: usage events from drivers + on-disk session scanner with watermarks, cost aggregation by day/project/model, retention sweep
- [ ] **Plan usage meter (N12):** subscription rate-limit windows — percent used and reset time — beside the token and cost totals
- [ ] **Pricing (N13):** cost from a public rate table, fetched at most daily and cached beside the database; the page states its cost quality rather than guessing silently
- [ ] Embedded browser surface via `gpui-component`'s `webview` crate: address bar with history autocomplete, find-in-page, per-workspace history with frecency
- [ ] Design mode: click an element → send HTML/CSS/screenshot to the agent
- [ ] Notifications + sounds on agent completion / attention needed
- [ ] Scratch workspaces (`~/.ginka/projects/<date>/<slug>`) for projectless starts
- [ ] Remaining drivers: `acp`, `opencode`, `gemini`, `cursor`, `amp`

> **Landed already (domain layer):** N12 plan windows and N13 the rate table, pricing and cost quality (`ginka-core::usage`). What remains is collecting the events and drawing the page.

---

### M6 — Ship (target: 3 weeks)

- [ ] macOS: universal build, codesign, notarize, `.dmg`, Sparkle auto-update, Homebrew cask
- [ ] **One update contract across platforms (N15):** a signed appcast that Sparkle consumes on macOS and that Windows and Linux implement directly against the same key. Linux updates only the user-writable install layout; a package-manager-owned build defers to its manager instead of overwriting files it does not own
- [ ] **Bundle and sign the daemon with the app (N16);** keep a separately named debug daemon in development so daemon-side edits do not force an app relaunch
- [ ] Nightly channel from `main`
- [ ] Linux: `.tar.gz` + install script (`~/.local`), Wayland + X11 verified
- [ ] Windows: installer + portable zip (best-effort at v1)
- [ ] `CONTRIBUTING.md`, `RELEASING.md`, `SECURITY.md`, a maintained `CHANGELOG.md` — the release process has to be written down before the first release, not after it
- [ ] Onboarding: prerequisite checks (agent CLIs installed and authenticated), first-project flow
- [ ] Docs site + user documentation
- [ ] Performance pass against the budgets in §6.2

---

## 6. Cross-cutting concerns

### 6.1 Testing

- **Unit** in each crate; domain logic lives in `ginka-core` precisely so it is testable without a window.
- **Daemon integration tests** against a temp `$HOME` and real git repos created in `tempfile` dirs — this is where the worktree/branch edge cases (detached, locked, manually removed) get pinned down.
- **A fake agent binary** (Band's `fake-agent.mjs` equivalent, as a Rust test binary) that emits scripted event streams, so chat/session behaviour is testable without burning tokens or network.
- **UI tests** with GPUI's test harness for pane/layout logic; screenshot tests are explicitly *not* attempted at v1.
- **A seeded-session fixture** — a script that fills a temp `GINKA_HOME` with long transcripts, many workspaces and a few running-looking sessions. Every performance and virtualization claim in §6.2 needs something to make it against, and hand-driving an agent for twenty minutes to reproduce a scroll bug is not a workflow.
- Every bug fix lands with a regression test. Band's `apps/web/e2e` list is a good checklist of the failure modes worth covering (reconnect, parking, virtualization, tab self-heal, workspace switch stability).

### 6.2 Performance budgets

| Metric | Budget |
| --- | --- |
| Cold start to interactive window | < 500 ms |
| Warm start | < 300 ms |
| Workspace switch | < 50 ms |
| Frame time while an agent streams into a visible pane | < 8 ms (120 Hz safe) |
| Idle CPU with 5 running agents | < 3 % |
| Memory with 10 workspaces open | < 500 MB RSS |

Streaming transcripts and terminal output are the two places this will be lost; both need bounded, virtualized rendering from the first commit rather than a later optimization pass.

Budgets alone do not hold, so the mechanism is written down with them. GPUI rebuilds and lays out every visible element on every frame a view renders, which makes the working model **CPU ≈ redraw rate × visible element count**, and every rule below bounds one of those two terms:

- **Bound the redraw rate.** Provider chunks coalesce into one commit per frame interval rather than one per chunk, and decorative animation never re-arms a per-display-frame callback for the length of a turn. A single always-running animation is enough to pin a 120 Hz window on its own.
- **Bound what one frame can see.** Long collections are virtualized, and a row builder must never rebuild whole-session state — that work is hoisted to a cache refreshed once per frame.
- **Nothing a frame can reach may do I/O.** No subprocess, no filesystem walk, no network, no blocking lock inside `render` or a row builder — one `git` invocation is already several frames of budget. Work goes to the background executor, the result is stored on the entity, and render reads only that store; a miss means "not known yet" and degrades gracefully. Resolve a whole collection in one background pass rather than probing per row, guarded so a superseded pass cannot overwrite newer state.
- **Measure before and after.** Streaming regressions are found by counting frames and visible elements, not by reading code and guessing: the fix is almost always that one of the two terms above is wrong by an order of magnitude, not that some third thing is slow. When this becomes real work it gets its own `docs/performance.md`.

### 6.3 Security

- Daemon binds loopback only, bearer token in a `0600` file.
- Agent processes inherit a sanitized environment; secrets are never written to logs or the DB.
- The setup runner copies untracked files (`.env`) between worktrees — this is a deliberate, documented, per-project opt-in.
- Embedded browser: no shared cookie jar with the user's real browser; certificate errors surface, never auto-accept.

### 6.4 i18n / a11y

`rust-i18n` with `en` and `ja` (retrofitting localization is expensive — this was scheduled for M0 and has not landed; it is now an explicit M0 checkbox).

Accessibility is a product requirement, not a pass at the end. GPUI exposes no screen-reader tree yet, so here it means the three things that do not depend on that API and that regress silently when unchecked:

- **Keyboard operability.** Every control reachable by mouse is reachable and operable by keyboard, with a visible focus treatment and the conventional keys for the widget.
- **Honour reduce-motion.** The 260 ms curves in `docs/ui.md` are decorative; any direct per-frame animation checks the system setting and skips.
- **Never encode meaning in colour, hover or motion alone.** Our agent status is a coloured word *and* a glyph precisely for this reason (`docs/ui.md` §5), and anything revealed on hover is also reachable by focus.

## 7. Risks and open questions

| # | Risk | Mitigation / decision needed |
| --- | --- | --- |
| R1 | `gpui` is unpublished and moves fast; upstream breakage can stall work | The rev comes from `gpui-component`; we bump the toolkit, not GPUI. Budget a bump every ~6 weeks. Shipping apps already track this moving rev in production, so it is a known cost, not a novel one. |
| R2 | Scope. Band is ~800 commits of surface area with a full team behind it | Milestones are ordered so M3 already delivers a daily-usable product; M5 items are individually droppable. |
| R3 | Embedded browser needs native child views composited under GPUI overlays — a `WKWebView` sits above GPUI's own scene, so menus and tooltips render behind it | The apps that have solved this solved it by forking GPUI for layered scene rendering, which §4.6 forbids us: our rev comes from `gpui-component`. By M5 the options are that its `webview` crate already handles the overlap, that upstream has landed layered rendering, or that the browser ships as a separate window. Establish which *before* M5 starts, not during it. |
| R4 | Terminal fidelity (ligatures, sixel, IME, mouse reporting) is a deep well | Target "good enough to run an agent CLI", not Ghostty parity. |
| R5 | Editor + LSP could consume the whole schedule | Hard-capped: read-focused editor, no refactoring/completion features in v1. |
| R6 | Agent CLIs change their output formats without notice | Driver conformance tests against recorded fixtures; fail loudly with a clear "unsupported agent version" rather than silently mis-parsing. |
| R7 | **`bezel` and `gpui-component` link incompatible `gpui` crates** (`bezel-gpui 0.3.8+zed.82aeef` vs `gpui 0.2.2` from zed git) and cannot be mixed | Decided: `gpui-component` is the only linked toolkit; bezel is a design reference we port from under MIT. See §4.6. Re-evaluate if the forks converge. |
| R8 | `gpui-component`'s default look is macOS/Windows-conventional, not the glass aesthetic in `docs/ui.md` | Its theme system is token-driven; the glass layer and status glyphs are ours (`docs/ui.md` §5). Validate the look in M0 — if the toolkit fights the design there, that is the moment to reconsider R7, not later. |
| R9 | Prior art for this app exists, most of it is GPL-3.0, and reading it for behaviour is one keystroke away from copying it | Read it for behaviour and for the edge cases it has already hit; write the implementation ourselves, from the requirement rather than from the file. Licence is only half the reason (Q3) — the other half is that an implementation we did not write is one we cannot debug or upgrade. The rule covers permissively licensed references too, bezel included (§4.6). |
| **Q1** | Remote access (Band's tunnel + QR + mobile layout) — in or out? | Out for v1. Revisit after M6; a web client is cheap given `ts-rs` types, mobile-quality UI is not. |
| **Q3** | License | Band is source-available; most comparable projects in this space are GPL-3.0. Decide before the first public commit. Note that no GPL-3.0 Zed crate (`editor`, `rope`, `text`, `language`, `theme`, `ui`, …) may be linked until this is settled — see §4.6. |
| **Q4** | Name/branding, bundle id, update feed host | Before M6. |
| **Q5** | Computer use — letting an agent drive other macOS apps through the accessibility API, off by default and granted per application — in or out? | Out for now, and noted rather than forgotten. It overlaps Orca's design mode (M5) in intent but is a much larger surface: a permission model, a reverse-engineered platform API, and a per-app grant UI. Revisit only after M5 ships the browser pane. |
| **Q6** | An embedded JavaScript kernel exposed to agents over MCP — a persistent in-process REPL they can call as a tool — in or out? | Out for v1. Our MCP server exposes *app control* (M4); an execution sandbox is a different product with its own security surface, and agents already have a shell. |
| **Q7** | Do we publish telemetry at all? | Default no, and the answer has to be recorded either way. "Local-first" (§1) does not by itself forbid anonymous aggregate counts that carry no prompts, paths or agent output — but shipping any telemetry without a decision recorded here would contradict how the project describes itself, and adding it quietly later is worse. |

## 8. Decision log

| Date | Decision | Rationale |
| --- | --- | --- |
| 2026-08-31 | Target Band's feature set, not a new product concept | A known-good spec removes product risk and lets the work be measured against something concrete. |
| 2026-08-31 | Client–daemon split from M2, not later | Retrofitting a process boundary is far more expensive than starting with one, and it is what makes the CLI/MCP surface possible at all. |
| 2026-08-31 | Worktree-per-workspace, workspace id derived from an immutable `name` | Band learned this the hard way; keying off the live branch breaks when an agent switches branches mid-task. |
| 2026-08-31 | Drop mobile, SSH worktrees, and team features from v1 | Each is a product in itself and none serve the core review loop. |
| 2026-08-31 | `gpui-component` is the linked UI toolkit; `bezel` is a design reference we rebuild from | The two link incompatible `gpui` forks (R7), so it is either/or. Dock layout, `CodeEditor` + LSP, virtualized lists and the webview crate are worth more than bezel's head start on aesthetics, which we can reproduce. |
| 2026-08-31 | The `gpui` rev is owned transitively by the UI toolkit | Pinning it independently guarantees a conflict on the next toolkit upgrade. |
| 2026-08-31 | Zed's `editor` crate is not embedded; the code surface stays `gpui-component`'s `CodeEditor` | `editor` is `GPL-3.0-or-later` and would relicense the binary ahead of Q3, and its 73 in-workspace dependencies drag in most of Zed (including a rival `workspace`/`theme`/`ui` stack) with no published API to depend on. See §4.6. |
| 2026-08-31 | **Q2 resolved:** `rusqlite` with a hand-rolled `user_version` migration runner, not `sqlx` or `refinery` | The database is owned by one synchronous daemon, so async SQL buys nothing and costs a runtime. The runner is ~30 lines, embeds migrations at compile time, and refuses to open a database written by a newer build rather than silently downgrading it. |
| 2026-08-31 | CI lints with `cargo clippy -- -D warnings`, not a global `RUSTFLAGS` | `RUSTFLAGS` promotes warnings in third-party crates too, which makes the build fail on a dependency's schedule rather than on ours. |
| 2026-09-01 | Git goes through the `git` binary, not `gix` | Worktree plumbing has to use the binary anyway — it is the only implementation that agrees with the user's own `git worktree list`, hooks and config. Splitting reads into a second implementation would put the truth in two places to save a subprocess. Revisit only if a read shows up in a profile. |
| 2026-09-01 | The CLI's development binary is `ginka-cli`, installed as `ginka` at packaging time | The app crate at the workspace root already produces a `ginka` binary; two targets with one name silently overwrite each other in `target/debug`, which is how the app binary went missing until the sizes were compared. |
| 2026-08-31 | Testable UI code lives in `ginka-ui`; the binary crate holds views only | `rustc` overflows its stack expanding `#[test]` in a crate that also contains the toolkit's deeply nested builder chains — raising `recursion_limit` turns the error into a SIGBUS rather than fixing it. The split is forced by the compiler, and happens to be the structure rule 2 wanted anyway. |
| 2026-09-04 | Sixteen interface-shaping requirements written down as N1–N16 (§3.3) and scheduled into §5 | They were missing from the plan or present only as a weaker version of themselves. None is a feature to bolt on later: steering, absorbable option changes, three-ref checkpoints, externalised attachments and the daemon-host path rule each change an interface, and interfaces are what a plan is for. |
| 2026-09-04 | Ginka contains no code copied or ported from another project; equivalent functionality is written from the requirement | Licence is the obvious half — most prior art here is GPL-3.0, and copying would settle Q3 by accident the way linking Zed's `editor` would (§4.6). Ownership is the other half: an implementation we did not write is one we cannot debug, upgrade or explain. This holds for permissively licensed references too, which is why bezel's role changed with it. |
| 2026-09-04 | Daemon paths are daemon-host paths, and clients say so explicitly (§4.1) | Costs nothing while the daemon is a local child process, and is the difference between remote access (Q1) being additive and being a rewrite. |
| 2026-09-04 | Steering is a first-class driver capability; the follow-up queue is its declared fallback | The queue alone forces "stop, retype, resend" on providers that can take a mid-turn message, which is the interaction the app exists to remove. |
| 2026-09-04 | The driver traits are synchronous | A driver owns a child process and a reader thread; the trait calls are short and the waiting already happens off the caller's thread. `async` would add a reactor to a daemon that is otherwise synchronous (Q2) and buy nothing measurable. |
| 2026-09-04 | N1–N14 are implemented in `ginka-core` / `ginka-protocol` ahead of the milestones that consume them, test-first | Each is an interface the daemon, the CLI and the views all sit on. Landing them as tested domain code first means M2–M5 wire up an interface that already exists rather than inventing one under UI deadline pressure — and the awkward cases (a refused steer, a hand edit between turns, an unpriced model) are pinned by tests rather than discovered later. |
