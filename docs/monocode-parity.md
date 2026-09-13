# MonoCode capability map

This is a behavioural inventory of [MonoCode](https://github.com/hardbeat920/monocode),
not an implementation source. It was read at release 0.1.44, commit
[`36d6d28`](https://github.com/hardbeat920/monocode/commit/36d6d28f50ec8ba1d7e12729e1f9a54cf383751f).
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
| Providers | Claude Code, Codex, Cursor, Grok Build, OpenCode, Pi, omp and fx; installed-CLI discovery; provider enable/disable; default and custom models | **Partial.** Claude and Codex run today. Codex's live `model/list` catalogue, per-model effort/tier metadata, static offline fallback and recent-model restoration are wired. Provider settings UI and ACP-backed drivers are M4/M5. |
| Conversations | Streaming sessions, resume, rename, delete, archive, pin, filters, folders, multi-select, recent-session restoration | **Partial.** Streaming/resume/rename/delete are done. Workspace archive is the first parity slice; filters and folders are M4 candidates. |
| Parallel work | Multiple tabs and windows, split conversation panes, background agents and subagents | **Partial.** Ginka runs concurrent workspace sessions and fan-out. Split transcript panes and reported subagents remain planned. |
| Composer | Multiline drafts, drag/drop/paste attachments, image preview, `@file`, `/command`, provider skills, plan mode, context compacting | **Partial.** Durable drafts, uploads, bounded file mentions and merged commands exist; attachment UI, plan approval and compact controls remain. |
| Follow-ups | Steer a running turn or queue messages; edit, remove, reorder and force-send queued prompts | **Partial.** The tested steer-or-queue policy exists. Queue management UI remains. |
| Interactive input | Permission approvals, agent questions, plan review/edit/approve, nested-agent input routing | **Partial.** The daemon now tracks open request ids, pauses the session, delivers a typed response into the live transport, persists the resolution, and rejects stale cards; UI, CLI, MCP and Slack share that path. A scripted transport pins the loop, while shipping drivers still do not raise native mid-turn requests. |
| Transcript | Markdown and code, reasoning/tool activity, tool diffs, failed-tool details, task lists, copy, quote-to-chat, prompt outline, search | **Partial.** Markdown/activity folding and daemon-side search exist. Task lists, quote-to-chat, in-page jump and prompt outline remain. |
| Context and usage | Per-turn model/provider provenance, context-window meter, manual compact, provider rate-limit windows | **Partial.** Usage and per-account headroom are done; context-window UI and per-turn provenance remain candidates. |
| Handoff | Change provider while retaining a bounded recap; cross-provider plan build | **Done** for conversation fork/handoff; plan build UI is not. |
| Second opinion | Send a completed answer to another provider in a split pane | **Partial.** Fan-out and cross-agent fork supply the domain pieces; the focused review action and comparison pane remain. |
| Checkpoints | Snapshot a session, exact session diff, undo while preserving pre-existing edits | **Done** in the three-ref checkpoint model and turn-scoped changes; UI polish remains. |
| Projects/worktrees | Open git repos or folders, project rail, per-session worktrees and branch creation/switching | **Done** for git/plain projects, scratch workspaces, worktrees and branch operations. Project appearance is a candidate. |
| Files/editor | File tree, quick/full-text search, syntax editor, linting, save/find/replace, file tabs, markdown/image preview, selection-to-chat | **Partial.** File/path/content search, conflict-safe saves, find/replace, independent file tabs, back/forward visit history, live-buffer Markdown preview, bounded local PNG/JPEG/GIF/WebP preview and saved selection-to-chat with exact line locations exist. Installed local language servers add tested stdio transport, versioned changes, hover, same-file definitions and diagnostics, with syntax-only fallback. The file tree and cross-file definition navigation remain in M4. |
| Source control | Staged/unstaged and hunk diffs, stage/unstage/discard, commit, pull/push/sync, history graph, PR creation, line comments | **Partial.** Diff sources, per-file stage/revert, commit/push and batched line comments are done. Hunks, pull/sync, history and PR creation remain. |
| Terminal | Embedded PTYs, persistent terminal tabs, dock positions, close-running confirmation | **Partial.** Daemon-owned PTYs, tabs, reattach and bounded replay are done. Splits/search and richer close flows remain. |
| Search/navigation | Global search across files/projects/transcripts, quick open, back/forward history, keyboard project/session navigation | **Partial.** Command palette, file search and transcript search protocol exist. One combined results UI and visit history remain. |
| Skills | Discover project/personal/provider skills, filter, enable/disable, create/reveal/copy path | **Partial.** CLI/MCP discovery and reversible enable/disable are done. Settings UI and creation actions remain. |
| Notes | Local markdown notes, tags/search, images, transcript selection to note | **Candidate.** Useful, but not required by the orchestrator's v1 loop. |
| Inbox | GitHub, GitLab and Linear issues/PRs/MRs; filtering, details, comments, diffs, linked sessions and ask/start flows | **Out for v1** as an orchestrator surface. Ginka's optional app rail can host dedicated clients without coupling them to core. |
| Notifications | Completion/input-needed desktop notifications, sound controls, dock badges | **Planned** for M5, with inactive-window and actionable-state rules. |
| Reminders/automation | One-shot session reminders with persistent notices | **Partial.** Ginka plans broader cron prompts with overlap-skip; one-shot reminders may be expressed by that scheduler. |
| Appearance/layout | Dark/light glass themes, scaling, fonts, backgrounds, Classic/Deck, zen mode, persisted tabs/splits | **Partial.** Theme tokens and three-column layout exist. Persisted dock layout and settings are M4; decorative backgrounds/mascots are candidates. |
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
   same-file definitions and diagnostics are wired; the file tree and
   cross-file definition navigation remain.
5. Unified search results and transcript jump, keeping the daemon-side search
   bound and virtualized.
6. Notifications and scheduler UI after the action states are reliable enough
   that a notification can take the reader to the exact pending request.

Each slice starts with a failing domain or service test. UI-only behaviour goes
in `ginka-ui` when it can be tested without a window; view builder chains stay
in `src/`.
