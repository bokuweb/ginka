# Ginka Roadmap

> Status: **in progress**. M0 and M1 are largely landed and M2's daemon, drivers and sessions are in; the remaining gaps are marked below.
> Last updated: 2026-09-02

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

### 2.3 [waku](https://github.com/egoist/waku) — the technical blueprint

waku is the existence proof that this app can be built in Rust + GPUI, and its structure is the closest thing we have to a reference implementation. We follow it on:

> **Where this stands (2026-09-02):** the crate split, the authenticated
> client–daemon RPC, the driver abstraction with `claude` and `codex`, the
> git-backed checkpoints, the TypeScript export, the config split and the
> scratch workspaces, and `rust-i18n` with `en` and `ja`, are all in. What is
> left from this list is scheduled rather than skipped: ACP (M5),
> `alacritty_terminal` (M3), and `ropey` and `nucleo-matcher` (M4).

- **Crate split**: `waku-protocol` (wire types) / `waku-core` (domain + drivers) / `waku-daemon` (headless server) / `waku-client` (RPC client) / root binary (GPUI UI). Ginka mirrors this.
- **Client–daemon split over authenticated WebSocket RPC**, with the daemon owning SQLite, the agent processes and git. The UI holds no authoritative state.
- **Provider drivers** as one module per agent (`claude`, `codex`, `amp`, `opencode`, `acp`, …) behind a common trait, with ACP used where the vendor supports it.
- **Git-backed checkpoints** tied to conversation turns, so a task can be rewound to any point in the transcript. Band has no equivalent; this is a genuine improvement.
- **Type export**: `ts-rs`-style generation from the Rust protocol crate, so any future web/mobile client is typed for free.
- **Concrete dependency choices**: `alacritty_terminal` for the PTY grid, `ropey` for buffers, `nucleo-matcher` for fuzzy matching, `smol` for the async runtime inside GPUI's executor, `rust-i18n` for localization.
- **Also worth copying**: `~/.ginka/projects/<date>/<slug>` scratch workspaces for "just start an agent, no project" flows, and the split between app-level config (`app.json`) and daemon config (`settings.json`).

## 3. Scope

### 3.1 v1.0 definition of done

A user can: register a project → create a worktree workspace → start a Claude Code (or Codex) session in it → watch status across all workspaces → read the diff → comment on the diff and send it back → run terminals → commit → and drive all of the above from `ginka` on the command line. On macOS, signed and auto-updating.

### 3.2 Non-goals for v1

- Being a general-purpose IDE. The editor exists to read code and make small corrections, not to replace Zed/VS Code.
- A hosted/multi-user service, accounts, or team sync.
- Windows/Linux parity at v1 (they are supported targets, but macOS ships first).
- Mobile clients, SSH worktrees, Linear/Jira integration.

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

### 4.2 Crate layout

```
ginka/
├─ Cargo.toml                # workspace root; the GPUI binary lives here (waku-style)
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
| Design reference | `bezel` (MIT, `crabtalk/bezel`) | **Not linked** — see §4.6 and risk R7. Ported selectively for the glass theme, agent status glyphs, and composer slash/mention handling. |
| Async | `smol` + GPUI's executor | Matches GPUI's model; avoid dragging in a second reactor. Tokio only where a dependency demands it. |
| DB | `rusqlite` + `refinery` (or `sqlx` w/ offline mode) | Decision due in M0. Bundled SQLite either way. |
| Terminal | `alacritty_terminal` + `portable-pty` | Same stack Zed and waku use. |
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
- `checkpoints` — waku-style: turn id → git tree/stash ref, so a transcript position maps to a working-tree state.
- `browser_history` — (workspace, url) unique, `visit_count` / `last_visited_at` for frecency. M5.

### 4.5 Agent driver abstraction

```rust
trait AgentDriver {
    fn id(&self) -> &'static str;                        // "claude" | "codex" | ...
    async fn probe(&self) -> Result<ProbeResult>;        // installed? authed? version?
    fn models(&self) -> Vec<ProviderModel>;
    async fn start(&self, spec: SessionSpec) -> Result<Box<dyn AgentSession>>;
    async fn resume(&self, cursor: ResumeCursor) -> Result<Box<dyn AgentSession>>;
}
```

Every driver normalizes into one `AgentEvent` stream (`TextDelta`, `Reasoning`, `ToolCall{..}`, `ToolResult`, `AskUser`, `PlanProposal`, `Usage`, `TurnEnd`, `SessionResult`). Two implementation paths:

1. **ACP** where the vendor speaks it — one adapter covers many agents.
2. **JSONL/stdio adapters** per vendor otherwise (Claude Code's stream-json, Codex, OpenCode, Gemini, Cursor, Amp).

Driver order of work: `claude` → `codex` → `acp` (covers several) → the rest.

### 4.6 UI stack

The UI is built on [`gpui-component`](https://github.com/longbridge/gpui-component) (Longbridge, production-tested in Longbridge Pro), with [`bezel`](https://bezel.gallery/) as a design reference and a source of MIT-licensed code to port. The visual specification lives in [`docs/ui.md`](ui.md).

**Why `gpui-component` is the linked dependency.** Four of its subsystems are things this app would otherwise have to build, and each is worth weeks:

- **Dock layout** — resizable panels, draggable tabs, persisted arrangement. This is the right panel, the terminal dock and the split panes. It deletes the "own GPUI dock implementation" line item from M4.
- **`CodeEditor`** — tree-sitter highlighting *and* LSP already wired. M4's editor and LSP work collapses into integration.
- **Virtualized `List` and `Table`** — the session sidebar and the Reports tables, meeting the performance budget without custom work.
- **`webview` crate** — the M5 embedded browser pane, which is otherwise the single hardest item on the roadmap (see R3).

Plus markdown rendering, charts, themes, CJK text handling, and a `Sidebar` component that matches the reference layout.

**Why `bezel` is *not* linked.** Bezel is a much closer fit in spirit — its crates are `theme` (including `glass.rs`), `motion`, `agent` (animated status orbs and avatars), `ui`, `syntax`, `markdown`, `editor` (with slash-command and link handling), and `terminal` (an `alacritty_terminal` view). That is almost a description of this app. But:

> **The two libraries link different, incompatible `gpui` crates.** `bezel` depends on `bezel-gpui 0.3.8+zed.82aeef` — a *republished fork* of Zed's GPUI under a different package name. `gpui-component` depends on `gpui 0.2.2` from `zed-industries/zed` git. Two package names means two distinct crates in the dependency graph, so `App`, `Window`, `Element` and every other core type are unrelated at the type level. They cannot be mixed in one binary.

So this is an either/or, not a "use both". We pick `gpui-component` because dock + editor/LSP + virtualized lists + webview are strictly more expensive to rebuild than bezel's aesthetics are to reproduce. Bezel is MIT, so where it is genuinely ahead we **port the code with attribution** rather than depending on it:

| Ported from bezel | Into | Milestone |
| --- | --- | --- |
| `theme/glass.rs` — translucent window + vibrancy | our theme layer | M0 |
| `agent/orbs`, `agent/avatar` — animated agent status glyphs | `ginka-ui` | M2 |
| `editor/slash.rs`, `editor/link.rs` — composer slash commands and file links | composer | M2 |
| `motion` — phase-based animation helpers | our motion layer | M0 |

**Revisit condition:** if bezel moves onto upstream `gpui` (or gpui-component onto bezel's fork), re-evaluate. Until then, one linked toolkit.

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
- [ ] Motion helpers ported from bezel; 260 ms list reordering
- [x] `export-types` binary for the protocol crate (`--features export`; the output is a build product and is not committed)
- [ ] Hot-reload settings on change
- [ ] Crash handler

**Exit criteria:** window opens in <300 ms warm; the three-column shell with a resizable sidebar and right panel renders against `docs/ui.md`; theme switches; migrations run on a fresh `~/.ginka`.

---

### M1 — Projects, worktrees, dashboard (target: 3 weeks)

Goal: the workspace list from Band's dashboard, fully working, with no agents yet.

- [x] Project registry: add/remove, `git` vs `plain` kind detection, default-branch probe
- [ ] Project rename, labels, reordering
- [x] Worktree lifecycle: create (branch from base), remove (force for dirty), pin
- [ ] Prune, and the locked-worktree cases `git worktree remove` refuses
- [ ] Setup runner: `.ginka/config.json` per project — copy untracked files (`.env`, etc.) and run setup commands on worktree creation
- [x] `syncWorktrees` equivalent: reconcile DB against `git worktree list`, updating `branch` / `head`
- [ ] Branch status poller: dirty/conflict/ahead/behind, throttled
- [x] Sidebar per `docs/ui.md` §3.2: three-line rows, status pills, archived section, attention sort, user footer — now fed by real projects and worktrees, with a first-run empty state that names the command to fix it
- [ ] Virtualize the session list; animate the reorder on the 260 ms curve
- [x] `ginka project add|list|remove`, `ginka workspace list|new|remove|pin`, `ginka daemon status|start|stop`, `ginka session list|start|send|cancel|log`, `ginka checkpoint list|restore` — all through the daemon
- [ ] Command palette + global keymap infrastructure
- [ ] Workspace picker / quick switcher

**Exit criteria:** create and delete 20 worktrees across 3 projects; state survives restart; sync self-heals after manual `git worktree add` outside the app.

---

### M2 — Daemon split + first agent (target: 4 weeks)

Goal: a real agent runs in a worktree and its transcript renders.

- [x] Extract the daemon: WS RPC server, bearer token, `~/.ginka/daemon.json` discovery, auto-spawn, refusal to start on top of a live daemon
- [ ] Graceful takeover of a running daemon by a newer build
- [x] Request/response + server-push event streams with sequence numbers, a bounded replay window, and an explicit `gap` frame when a cursor falls out of it
- [x] `AgentDriver` trait + `claude` driver (stream-json over stdio), process supervision, cancellation by process group
- [x] Session persistence: sessions and transcripts as normalized events; resume from the vendor session id
- [x] Chat pane: streaming transcript, tool-call cards, reasoning blocks, paged from a cursor and followed live off the daemon's push stream
- [ ] Virtualize the transcript list (§6.2's budget is lost here first)
- [x] Message queueing while the agent is busy
- [x] Composer sends: a follow-up to the running session, or a new agent in the workspace
- [x] Agent and model pickers in the composer, and a new-session toggle
- [ ] Composer: `@file` mentions, slash commands, drafts persisted per workspace
- [ ] Plan approval and ask-user-question interaction modes. The events and the `respond_to_agent` request exist; no shipped driver can interrupt a turn to raise them, so this waits on ACP
- [x] Agent status + "needs attention" derivation, surfaced back on the dashboard
- [x] Per-agent settings in `settings.json`: which binary to run and what environment to give it
- [x] `codex` driver (both generations of its JSONL)

**Exit criteria:** two agents run concurrently in two worktrees for 30+ minutes; killing and restarting the UI loses no transcript; cancel actually kills the process tree.

---

### M3 — Terminal, changes, review loop (target: 4 weeks)

Goal: the loop that makes the app useful daily — read the diff, comment, send back.

- [ ] PTY pool in the daemon; terminal grid view in GPUI (`alacritty_terminal`; bezel's `terminal` crate is the reference implementation)
- [ ] Terminal tabs, splits, scrollback search, parking/reattach with replay + width sync
- [ ] File-path detection in terminal and chat output → click opens the file
- [ ] Selection → "add to chat" / "add to terminal"
- [ ] Changes panel: status tree, per-file diff (unified + split), intra-line word diff, revert file
- [ ] Commit dialog: stage/unstage, message composer, agent-generated message
- [ ] **Diff review comments (Orca):** anchor markdown comments to diff lines, batch them, send the batch as one agent message
- [x] Checkpoints (waku): snapshot the worktree per turn, rewind to any of them
- [x] Rewinding from the transcript in the UI, confirmed in two steps
- [ ] Pruning old checkpoint refs

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

**Exit criteria:** an agent can create a workspace, start a sibling agent, and read its diff entirely through the CLI/MCP.

---

### M5 — Depth (target: 4 weeks)

- [ ] Cronjobs: scheduler, project/workspace scope, `via: chat | terminal`, overlap-skip, run history
- [ ] Reports: usage events from drivers + on-disk session scanner with watermarks, cost aggregation by day/project/model, retention sweep
- [ ] Embedded browser surface via `gpui-component`'s `webview` crate: address bar with history autocomplete, find-in-page, per-workspace history with frecency
- [ ] Design mode: click an element → send HTML/CSS/screenshot to the agent
- [ ] Notifications + sounds on agent completion / attention needed
- [x] Scratch workspaces (`~/.ginka/projects/<date>/<slug>`) for projectless starts, and plain folders as workspaces
- [ ] Remaining drivers: `acp`, `opencode`, `gemini`, `cursor`, `amp`

---

### M6 — Ship (target: 3 weeks)

- [ ] macOS: universal build, codesign, notarize, `.dmg`, Sparkle-style auto-update, Homebrew cask
- [ ] Nightly channel from `main`
- [ ] Linux: `.tar.gz` + install script (`~/.local`), Wayland + X11 verified
- [ ] Windows: installer + portable zip (best-effort at v1)
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

### 6.3 Security

- Daemon binds loopback only, bearer token in a `0600` file.
- Agent processes inherit a sanitized environment; secrets are never written to logs or the DB.
- The setup runner copies untracked files (`.env`) between worktrees — this is a deliberate, documented, per-project opt-in.
- Embedded browser: no shared cookie jar with the user's real browser; certificate errors surface, never auto-accept.

### 6.4 i18n / a11y

`rust-i18n` with `en` and `ja` from M0 (retrofitting localization is expensive). **Done:** every user-visible string in the window and the CLI resolves through `locales/app.yml`, which keeps both languages side by side so a gap is visible in review; the language follows `app.json`, then `LC_ALL`/`LC_MESSAGES`/`LANG`, then English. Keyboard-first navigation for every action; no action reachable only by mouse.

## 7. Risks and open questions

| # | Risk | Mitigation / decision needed |
| --- | --- | --- |
| R1 | `gpui` is unpublished and moves fast; upstream breakage can stall work | The rev comes from `gpui-component`; we bump the toolkit, not GPUI. Budget a bump every ~6 weeks. waku and gpui-component both prove this is workable. |
| R2 | Scope. Band is ~800 commits of surface area with a full team behind it | Milestones are ordered so M3 already delivers a daily-usable product; M5 items are individually droppable. |
| R3 | Embedded browser needs native child views composited under GPUI overlays | Exactly the problem waku's zed fork solves. If upstream hasn't landed layered scene rendering by M5, either take the fork or ship the browser as a separate window. |
| R4 | Terminal fidelity (ligatures, sixel, IME, mouse reporting) is a deep well | Target "good enough to run an agent CLI", not Ghostty parity. |
| R5 | Editor + LSP could consume the whole schedule | Hard-capped: read-focused editor, no refactoring/completion features in v1. |
| R6 | Agent CLIs change their output formats without notice | Driver conformance tests against recorded fixtures; fail loudly with a clear "unsupported agent version" rather than silently mis-parsing. |
| R7 | **`bezel` and `gpui-component` link incompatible `gpui` crates** (`bezel-gpui 0.3.8+zed.82aeef` vs `gpui 0.2.2` from zed git) and cannot be mixed | Decided: `gpui-component` is the only linked toolkit; bezel is a design reference we port from under MIT. See §4.6. Re-evaluate if the forks converge. |
| R8 | `gpui-component`'s default look is macOS/Windows-conventional, not the glass aesthetic in `docs/ui.md` | Its theme system is token-driven; the glass layer and status glyphs are ours (`docs/ui.md` §5). Validate the look in M0 — if the toolkit fights the design there, that is the moment to reconsider R7, not later. |
| **Q1** | Remote access (Band's tunnel + QR + mobile layout) — in or out? | Out for v1. Revisit after M6; a web client is cheap given `ts-rs` types, mobile-quality UI is not. |
| **Q3** | License | Band is source-available, waku is GPL-3.0. Decide before the first public commit. |
| **Q4** | Name/branding, bundle id, update feed host | Before M6. |

## 8. Decision log

| Date | Decision | Rationale |
| --- | --- | --- |
| 2026-08-31 | Target Band's feature set, not a new product concept | A known-good spec removes product risk and lets the work be measured against something concrete. |
| 2026-08-31 | Client–daemon split from M2, not later | Retrofitting a process boundary is far more expensive than starting with one, and it is what makes the CLI/MCP surface possible at all. |
| 2026-08-31 | Worktree-per-workspace, workspace id derived from an immutable `name` | Band learned this the hard way; keying off the live branch breaks when an agent switches branches mid-task. |
| 2026-08-31 | Drop mobile, SSH worktrees, and team features from v1 | Each is a product in itself and none serve the core review loop. |
| 2026-08-31 | `gpui-component` is the linked UI toolkit; `bezel` is a design reference ported under MIT | The two link incompatible `gpui` forks (R7), so it is either/or. Dock layout, `CodeEditor` + LSP, virtualized lists and the webview crate are worth more than bezel's head start on aesthetics, which we can reproduce. |
| 2026-08-31 | The `gpui` rev is owned transitively by the UI toolkit | Pinning it independently guarantees a conflict on the next toolkit upgrade. |
| 2026-08-31 | **Q2 resolved:** `rusqlite` with a hand-rolled `user_version` migration runner, not `sqlx` or `refinery` | The database is owned by one synchronous daemon, so async SQL buys nothing and costs a runtime. The runner is ~30 lines, embeds migrations at compile time, and refuses to open a database written by a newer build rather than silently downgrading it. |
| 2026-08-31 | CI lints with `cargo clippy -- -D warnings`, not a global `RUSTFLAGS` | `RUSTFLAGS` promotes warnings in third-party crates too, which makes the build fail on a dependency's schedule rather than on ours. |
| 2026-09-01 | Git goes through the `git` binary, not `gix` | Worktree plumbing has to use the binary anyway — it is the only implementation that agrees with the user's own `git worktree list`, hooks and config. Splitting reads into a second implementation would put the truth in two places to save a subprocess. Revisit only if a read shows up in a profile. |
| 2026-09-01 | The CLI's development binary is `ginka-cli`, installed as `ginka` at packaging time | The app crate at the workspace root already produces a `ginka` binary; two targets with one name silently overwrite each other in `target/debug`, which is how the app binary went missing until the sizes were compared. |
| 2026-08-31 | Testable UI code lives in `ginka-ui`; the binary crate holds views only | `rustc` overflows its stack expanding `#[test]` in a crate that also contains the toolkit's deeply nested builder chains — raising `recursion_limit` turns the error into a SIGBUS rather than fixing it. The split is forced by the compiler, and happens to be the structure rule 2 wanted anyway. |
| 2026-09-02 | The domain objects live in `ginka-protocol`, not `ginka-core` | A wire copy and a domain copy of "what a project is" drift, and the CLI and the app both have to name one without linking the daemon's git and SQLite code. `ginka-core` persists them; nothing in the protocol crate knows what a database is. |
| 2026-09-02 | Requests are a typed enum and failures are their own frame | A `serde_json::Value` payload hides the capability list rule 3 depends on, and a serialized `Result` would make `{"Ok":…}` part of the contract every future client reads. |
| 2026-09-02 | The transport is `async-tungstenite` over `async-net`, driven by smol | It works over any futures stream, so no runtime feature and no second reactor comes in behind it. Requests run on smol's blocking pool because git shells out and SQLite is synchronous; the connection's writer keeps draining pushes meanwhile. |
| 2026-09-02 | Server pushes carry a sequence number, with a bounded replay window and an explicit `gap` frame | A short replay would leave a reconnecting client believing it was caught up. Told about the gap, it re-reads what it cares about instead of patching. |
| 2026-09-02 | Drivers are pure: a command description and a line parser | Vendor formats are then testable against recorded fixtures rather than against a live CLI, which is the mitigation R6 asks for, and process supervision is written once above them. |
| 2026-09-02 | A turn is one process; a follow-up is a resume | The vendors' non-interactive modes exit when a turn ends, so there is no stdin to write to. This is why the vendor session id is persisted: without it a conversation can be replayed but not continued. |
| 2026-09-02 | An agent that exits zero having said nothing the driver could read is reported as failed | That is exactly what a vendor changing its output format looks like, and reporting it as a finished session with an empty transcript hides it. |
| 2026-09-02 | Agent processes start in their own process group and are stopped through the `kill` binary | Signalling the group is what makes a cancel reach the tools the agent started. The binary rather than `libc` keeps the workspace's `unsafe_code = "deny"` intact, on the same reasoning that sends git through its own binary. |
| 2026-09-02 | Checkpoints are commits on no branch, held by refs under `refs/ginka/checkpoints/` | A ref keeps them from being garbage-collected while keeping them out of `git log` and `git branch`. Restoring snapshots the state it is about to replace, because a rewind must never be the thing that loses work. |
| 2026-09-03 | Checkpoints are committed as Ginka, not as the configured user | `git commit-tree` needs an identity, and taking the user's put the app's bookkeeping in their history — and made a checkpoint impossible on a machine that has never run `git config user.email`, which is every container and CI runner. |
| 2026-09-03 | A cancel signals the agent's process group only after confirming the agent leads it | A child that did not get its own group is in ours, and signalling a group by its pid then hits something unrelated — on CI, the job. An agent that leads no group is stopped alone. |
| 2026-09-03 | Only the reader closes a connection's write queue | Two writers into one queue meant whichever stopped first closed it, and on a shutdown the event pump stops first — which dropped the answer to the request that asked for the shutdown. |
| 2026-09-02 | The CLI and the app both go through the daemon; `doctor` is the exception | Rule 3 is a structure rather than a promise only if there is one way in. `doctor` reads state directly because a daemon that will not start is exactly what it has to be able to report. |
