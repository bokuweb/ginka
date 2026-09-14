# MonoCode capability map

This is a behavioural inventory of [MonoCode](https://github.com/hardbeat920/monocode),
not an implementation source. It was first read at release 0.1.44 and reviewed
again through main commit
[`08ba69d`](https://github.com/hardbeat920/monocode/commit/08ba69da65b0d252fa49cb4548c5c9b13a884fe7).
Ginka implements the requirements in its own architecture: a daemon owns all
durable state, every capability crosses the shared protocol, and a workspace is
the worktree-sized task identity.

The statuses describe Ginka at 2026-09-13:

- **Done** means a usable vertical path exists, even when polish remains.
- **Partial** means the domain/protocol or a narrower UI is present.
- **Planned** means the roadmap already schedules it.
- **Candidate** is useful prior art that needs a product decision before scope.
- **Out** conflicts with Ginka's stated v1 non-goals.

| Area | MonoCode capability | Ginka status / interpretation |
| --- | --- | --- |
| Providers | Claude Code, Codex, Cursor, Grok Build, OpenCode, Pi, omp and fx; installed-CLI discovery; provider enable/disable; default and custom models | **Partial.** Claude and Codex run today. The searchable provider/model popover reads the driver catalogues; Codex uses live `model/list`, both drivers retain an offline fallback, and recent model/effort/tier choices are restored. Provider settings UI and ACP-backed drivers are M4/M5. |
| Conversations | Streaming sessions, resume, rename, delete, archive, pin, filters, folders, multi-select, recent-session restoration | **Partial.** Streaming/resume/rename/delete and workspace archive are done. The sidebar fuzzily searches title, model, provider, project and branch; folders and advanced filters remain M4 candidates. |
| Parallel work | Multiple tabs and windows, split conversation panes, background agents and subagents | **Partial.** Ginka runs concurrent workspace sessions and fan-out. Claude delegated-agent calls now stay in one parent row with a bounded, live trail of their reasoning, messages and tools. Codex child threads wait on its app-server transport; split panes and agents that outlive a turn remain planned. |
| Composer | Multiline drafts, drag/drop/paste attachments, image preview, `@file`, `/command`, provider skills, plan mode, context compacting | **Partial.** Durable drafts, uploads, bounded file mentions and merged commands exist. While a turn runs, an empty draft offers Stop and a written draft offers Send through the existing steer-or-queue policy. Attachment UI, plan approval and compact controls remain. |
| Follow-ups | Steer a running turn or queue messages; edit, remove, reorder and force-send queued prompts | **Partial.** The tested steer-or-queue policy exists. Queue management UI remains. |
| Interactive input | Permission approvals, agent questions, plan review/edit/approve, nested-agent input routing | **Partial.** The daemon now tracks open request ids, pauses the session, delivers a typed response into the live transport, persists the resolution, and rejects stale cards; UI, CLI, MCP and Slack share that path. A scripted transport pins the loop, while shipping drivers still do not raise native mid-turn requests. |
| Transcript | Markdown and code, reasoning/tool activity, tool diffs, failed-tool details, nested agent activity, task lists, copy, quote-to-chat, prompt outline, search | **Partial.** Markdown/activity folding, delegated-agent rows, daemon-side search, whole-message copy and quote-to-chat exist. Task lists, selected-range quoting and prompt outline remain. |
| Context and usage | Per-turn model/provider provenance, context-window meter, manual compact, provider rate-limit windows | **Partial.** Per-account headroom is always visible in the composer, updates from turn pushes, and refreshes on click; context-window UI and per-turn provenance remain candidates. |
| Handoff | Change provider while retaining a bounded recap; cross-provider plan build | **Done** for conversation fork/handoff from CLI/MCP and from any completed turn in the window; plan build UI is not. |
| Second opinion | Send a completed answer to another provider in a split pane | **Partial.** Fan-out and cross-agent fork supply the domain pieces; the focused review action and comparison pane remain. |
| Checkpoints | Snapshot a session, exact session diff, undo while preserving pre-existing edits | **Done** in the three-ref checkpoint model and turn-scoped changes; UI polish remains. |
| Projects/worktrees | Open git repos or folders, project rail, per-session worktrees and branch creation/switching | **Done** for git/plain projects, scratch workspaces and worktrees. Registration is a name-and-source-folder modal; the sessions column appears only for the selected project. The context-bar branch picker fuzzily switches or creates branches and explains branches held by another worktree. Project appearance beyond its display label remains a candidate. |
| Files/editor | File tree, quick/full-text search, syntax editor, linting, save/find/replace, file tabs, markdown/image preview, selection-to-chat | **Done.** A bounded expandable file tree, file/path/content search, conflict-safe saves, find/replace, independent file tabs, back/forward visit history, live-buffer Markdown preview, bounded local PNG/JPEG/GIF/WebP preview and saved selection-to-chat with exact line locations exist. Installed local language servers add tested stdio transport, versioned changes, hover, workspace-local definitions across tabs and diagnostics, with syntax-only fallback. |
| Source control | Staged/unstaged and hunk diffs, stage/unstage/discard, commit, pull/push/sync, history graph, PR creation, line comments | **Partial.** Diff sources, per-file stage/revert, commit/push and batched line comments are done. Hunks, pull/sync, history and PR creation remain. |
| Terminal | Embedded PTYs, persistent terminal tabs, dock positions, close-running confirmation | **Partial.** Daemon-owned PTYs, tabs, reattach and bounded replay are done. Splits/search and richer close flows remain. |
| Search/navigation | Global search across files/projects/transcripts, quick open, back/forward history, keyboard project/session navigation | **Partial.** Command palette, workspace/project-wide file search, semantic indexing and daemon-side open-conversation search with ⌘F/⌘K, excerpts and previous/next jumps exist. Project hits switch to their owning workspace before opening, and ⌘1–9 selects the corresponding visible active session in the current project. One combined file+transcript results UI and broader visit history remain. |
| Skills | Discover project/personal/provider skills, filter, enable/disable, create/reveal/copy path | **Partial.** CLI/MCP and the window's Skills surface discover grouped project/user installs, show every scope and daemon-host path, search metadata and paths, compose scope/state filters, reversibly enable/disable all copies, and copy paths. Creation and host-aware reveal remain. |
| Notes | Local markdown notes, tags/search, images, transcript selection to note | **Candidate.** Useful, but not required by the orchestrator's v1 loop. |
| Inbox | GitHub, GitLab and Linear issues/PRs/MRs; filtering, details, comments, diffs, linked sessions and ask/start flows | **Out for v1** as an orchestrator surface. Ginka's optional app rail can host dedicated clients without coupling them to core. |
| Notifications | Completion/input-needed desktop notifications, sound controls, dock badges | **Planned** for M5, with inactive-window and actionable-state rules. |
| Reminders/automation | One-shot session reminders with persistent notices | **Partial.** Ginka plans broader cron prompts with overlap-skip; one-shot reminders may be expressed by that scheduler. |
| Appearance/layout | Dark/light glass themes, scaling, fonts, backgrounds, Classic/Deck, zen mode, persisted tabs/splits | **Partial.** The e1-compatible theme/type tokens and project-rail/session-list/transcript/surface layout exist. Right-panel and terminal visibility/dimensions plus the active surface persist per workspace, and surfaces can be chosen from the toolbar or cycled by keyboard; draggable DockArea tabs/splits and settings UI remain M4. Decorative backgrounds/mascots are candidates. |
| Updates/platforms | Signed in-app update flow and packaged macOS/Linux/Windows builds | **Planned** for M6; macOS ships first. |
| Security | Local CLI credentials, command allowlists, CSP, no remote Markdown images | **Different architecture.** Ginka is native and daemon-authenticated; equivalent subprocess/path/content boundaries remain requirements. |

## Implementation order

Parity is not a request to reproduce every surface. Work proceeds in vertical,
test-first slices that strengthen Ginka's core loop:

1. Workspace archive/restore, preserved through git reconciliation and exposed
   over protocol and CLI; the existing sidebar archive section consumes it.
2. Model/effort/tier picker and recent choices, using the existing driver-owned
   option policy and catalogue probes. The live typed catalogue and recent
   model are done. Model/effort/tier selection now persists into every turn of
   new and existing conversations, and the driver decides whether the provider
   thread absorbs a live change. A `restart_required` answer now creates a new
   provider thread and carries the normalized transcript as a bounded digest.
3. Native plan/approval/question flows through `RespondToAgent`. The daemon,
   clients and scripted pausing transport are done; a shipped ACP or app-server
   transport that raises the normalized requests remains.
4. CodeEditor integration: edit/save is done with daemon-owned atomic writes and
   optimistic revision checks. Find/replace and saved selection-to-chat with
   exact file-line mentions, independent tabs, back/forward visit history and
   live-buffer Markdown preview and bounded local image preview are done. LSP
   server selection, bounded JSON-RPC transport, live-buffer changes, hover,
   diagnostics and workspace-local definition navigation across tabs are wired.
5. Unified search results and transcript jump, keeping the daemon-side search
   bound and virtualized.
6. Notifications and scheduler UI after the action states are reliable enough
   that a notification can take the reader to the exact pending request.
7. Delegated-agent activity: Claude's recorded `stream-json` shapes now become
   provider-neutral parent/step/finish events. Steps merge by provider id,
   orphan child output is dropped, open children settle at the turn boundary,
   and both text and retained history are bounded. Codex remains deliberately
   deferred until its app-server child-thread notifications replace the current
   `exec --json` transport; guessing an undocumented shape would violate R6.

Each slice starts with a failing domain or service test. UI-only behaviour goes
in `ginka-ui` when it can be tested without a window; view builder chains stay
in `src/`.
