# Ginka UI Specification

> Companion to [`roadmap.md`](roadmap.md). The roadmap says *what* we build and when; this document says *what it looks like* and *which components render it*.
> Last updated: 2026-09-13

## 1. Design direction

The reference is a four-region agent workstation on a dark, glass-tinted
surface. The two leading regions form one navigator: a stable project rail and,
only after a project is selected, that project's resizable session list. With
no selection the rail is 188 px and the new-session surface takes the rest.

```
┌──────────────────────────────────────────────────────────────────────────┐
│ ●●● Projects │ Workspace     │ Session title        │ Surfaces           │
├──────────────┼───────────────┼──────────────────────┼────────────────────┤
│ ⌸ comet      │ + New chat    │                      │ Files · Git · ...  │
│ ⌸ ginka      │               │ transcript           │                    │
│              │ ▸ session     │                      │ editor / diff      │
│              │ ▸ session     │                      │                    │
│              │               ├──────────────────────┤                    │
│              │ Archived      │ composer             │                    │
│              │ ▸ ...         │ [model][mode][usage] │                    │
├──────────────┴───────────────┼──────────────────────┤                    │
│ account · headroom           │ terminal dock        │                    │
└──────────────────────────────┴──────────────────────┴────────────────────┘
```

Five properties define the look:

1. **Glass over the desktop.** The window is translucent with a vibrancy blur; the user's wallpaper shows through at low contrast. Rounded window corners, no native title bar.
2. **Chromeless.** Borders are 6–8 % white hairlines, never solid lines. Panels are separated by tone, not by rules.
3. **Density with air.** The sidebar is dense (three lines per row), the transcript is generous (1.65 line-height, ~72ch measure).
4. **Status is ambient.** Agent state appears as a small animated glyph and a coloured word, never as a modal or a toast.
5. **Motion is short and consistent.** ~260 ms ease-out for list reordering; 200 ms for a panel coming in (fade, and a 14 px slide from its own edge) or going (the reverse, drawn until it has gone); 140 ms for a dialog to fade in and for a menu or picker card to fade and rise into place; a right-panel tab fades in as it comes to the front; ~120 ms for hover/press. Nothing bounces (`ginka_ui::motion`).

## 2. Design tokens

Defined once in `assets/themes/*.json`, installed into GPUI's global context at startup, consumed by every view. No view hardcodes a colour.

### Colour — dark (default)

| Token | Value | Use |
| --- | --- | --- |
| `bg.window` | `#0E0C12` @ 72 % + blur | window base, translucent |
| `bg.sidebar` | `#F2EEF8` @ 6 % | project rail |
| `bg.surface` | `#1B1624` @ 66 % | cards, composer, fields |
| `bg.raised` | `#241E2F` @ 72 % | raised surfaces |
| `bg.terminal` | `#0A090E` | terminal pane |
| `border.subtle` | `#FFFFFF` @ 6 % | panel separators |
| `border.strong` | `#FFFFFF` @ 12 % | focused input, selected row |
| `text.primary` | `#EDEAF4` | titles, the reader's own messages |
| `text.secondary` | `#A9A1BA` | subtitles, metadata |
| `text.muted` | `#716A83` | timestamps, placeholders |
| `accent` | `#AC94F1` | selection, links, focus ring |
| `status.working` | `#E97DB6` | running agent |
| `status.attention` | `#E8B737` | agent waiting on the user |
| `status.done` | `#57D184` | completed |
| `status.error` | `#ED7C7C` | failed |
| `code.bg` | `#AF9AEB` @ 10 % | inline code |

Two derived tints, computed from `accent` rather than stored: `row.hover` (14 %)
and `row.active` (22 %). Rows are otherwise transparent — they are told apart by
the space between them, and a grey fill over a translucent window is what turns
glass into cardboard.

**Every surface carries alpha**, not only the window: a fully opaque panel over
a blurred window looks like a mistake rather than a choice. What keeps them from
stacking into an opaque sheet is that **exactly one surface paints each pixel** —
the toolkit's own background is clear, `Root` paints the window once, and the
sidebar, the centre column and each card paint themselves once. Three coats of
70 % is 97 %, which is no longer glass. (Both rules are pedro's, whose palette
solved the same problem.)

The one deliberate exception is where text is read at length: the conversation
column and the right panel lay a second coat of `bg.window` over the glass, the
way e1's conversation column does, so the desktop shows through the sidebar
and not under a paragraph.

Text is drawn without macOS glyph thickening unless the user set a smoothing
level for the whole system (`ginka_ui::theme::font_smoothing_override`): the
toolkit thickens strokes by colour brightness whenever the setting is absent,
which is how a Mac ships, and near-white text then reads as bold.

A light theme ships with the same token names and WCAG AA contrast pairs. The
schema, palette, 13 px toolkit base and 12 px mono base intentionally match e1;
Kirikumo consumes the same token contract. Keeping these as serialized tokens,
instead of view constants, leaves the three applications able to share a theme
crate later without rewriting their views.

### Type

| Role | Font | Size / line-height | Weight |
| --- | --- | --- | --- |
| Toolkit base | UI sans (system / Inter) | 13 / toolkit default | 400 |
| Transcript body | UI sans, `text.secondary` | 11.5 / 1.6 | 400 |
| Session title | UI sans | 14 / 1.3 | 500 |
| Metadata | UI sans | 11–12 / 1.3 | 400 |
| Code / terminal | Mono (system mono / JetBrains Mono) | 11.5–13 / 1.35–1.5 | 400 |

### Geometry

4 px spacing grid. Radii: window 12 (the platform's), **card 10** (the composer
and anything else holding a group of controls — a card is an object on the
surface and the corner is what says so), panel 7, row 6, control 3. Tight on
purpose: softer corners at this density read as a toy rather than a tool.
Chips are rows, not pills: 28 px tall, 6 px radius, 12 px type. Navigator default 520 px (resizable 420–720), with a fixed
188 px project rail and a flexible session list shown only for the selected
project. Right panel
default 420 px (resizable, collapsible). Terminal dock default 30 % of the centre
column height.

## 3. Regions

### 3.1 Headers — there is no title bar

The window has no bar of its own. Each column paints itself to the top of the
window and carries its own 44 px header, and the four regions sit on one line, so the
window reads as one surface rather than as a bar laid over a layout. Nothing is
left to the platform but the traffic lights, positioned (13, 15) to land on
that line.

- **Leading column** — room for the traffic lights, then the sidebar toggle and
  the back/forward history. History spans project homes and workspace
  conversations, drops its forward branch after a new visit, retains the most
  recent 100 destinations, and skips projects or active workspaces that no
  longer exist. It is the sidebar's header while there is a
  sidebar, and moves onto the centre column when the sidebar is closed, because
  the lights do not move with it.
- **Centre column** — the agent glyph, the conversation's title and its
  `project` subtitle; on the right, new chat and the toggles for the terminal
  dock and the right panel. Nothing on the left when there is no conversation
  and no project: an empty window is not an error, and a placeholder title
  would be the only thing claiming otherwise.
- **Right panel** — the surfaces toolbar, at the same height.

Dragging any header moves the window and a double click zooms it. A drag is a
press that then moved, never the press alone, or every click on a control in
the strip would carry the window off with it.

### 3.2 Project rail and session list

Two adjacent navigation levels. The rail keeps projects stable while the
session list appears and changes with the selected project, matching the
spatial model used by e1 and leaving the centre column exclusively for the
active conversation. With nothing selected, the rail stands alone.

- **Project rail** — app header, then the places that are not projects —
  **Inbox** and **Notes** — the `Projects` action row, selectable project
  rows, and **Settings** (`⌘,`) at its foot. MonoCode's navigation: a place
  replaces the session list, the centre column and the right panel with
  columns of its own (§3.5–3.7), and picking a project comes back. The
  selected project remains highlighted while one of its sessions is open, and
  a lit place outranks it. The rail cannot be closed while a place other than
  a project is showing: there would be nothing on screen to get back from.
- **Row menu** — a `…` on each session row opens its actions under it:
  *Rename* (the title becomes a field; ↩ keeps it), *Pin* / *Unpin*
  (pinned rows lead the list and carry a star) and *Archive*. Archived rows
  offer *Restore*. A row with prompts waiting says how many.
- **Status filter** — a chip beside the session search cycles *All*,
  *Working*, *Needs you* and *Done*; `⌘1–9` pick only the rows it shows.
- **Session list header** — `Workspace` and a `+` for a new session. The whole
  sessions column is absent until a project is selected.
- **Conversation search** — directly below the header, with the rail's search
  icon focusing it and an explicit clear action. Matching is fuzzy and local
  over the row metadata already pushed by the daemon: title, model, provider,
  project and branch. Transcript contents remain the daemon-side N10 search;
  filtering this list never starts a second content-search path in the view.
- **New chat** — the first row under the search field, inset by the field's
  margin and a step below it so its hover reads as a list row. It clears the centre column
  for a conversation aimed at whatever is selected: a project, the project of
  the selected workspace, or nothing at all.
- **Section label** — `Projects`, small and muted, over the run of groups, with
  a `+` beside it that opens the add-project modal. The modal keeps the
  reader-facing project name and source-folder chooser in one card; selecting
  a folder prefills its basename, and successful registration selects the new
  project immediately. Escape, the close control and Cancel all dismiss it.
  The `+` and the equivalent composer row are absent for an externally
  addressed daemon: a folder picked on the client is not a path on that
  daemon's host.
- **Project heading** — folder icon + project name, muted, and **selectable**:
  a project is something the reader picks *before* there is a conversation, and
  picking one shows the home screen aimed at it. It stays highlighted while a
  session inside it is selected, because the two highlights identify different
  navigation levels.
- **Nothing registered** — one muted line under the section label. The adjacent
  `+` remains the single visual way into project registration; the CLI remains
  available without being repeated in the empty rail.
- **Workspace row**, indented under its project (6 px radius, selected =
  `bg.raised` fill):
  1. Agent glyph, session title, the pull request mark when the branch has
     one, right-aligned status: relative time (`now`, `46m`, `4h`) **or** a
     status pill (animated dot + `Working`). The pull request mark is its
     state's shape and colour — open in `status.done`, draft in `text.muted`,
     merged in `accent`, closed in `status.error` — names `#12 merged` on
     hover and opens the pull request on click.
  2. *Only when it says something the title does not:* git-branch icon +
     branch name (truncated from the left), the dirty dot, and the divergence.
     A workspace is named after the branch it was cut on, so this line appears
     when an agent has checked out something else inside the worktree — which
     is exactly when it matters.
- **Archived section** — collapsible header, one-line rows (glyph, title, age).
  Archive is durable workspace metadata: restoring a row returns it to its
  project without recreating the worktree or conversation. `Show N more` is a
  paging affordance once the archived list exceeds the first page.
- **Footer** — avatar, user name, and the account in use with its headroom —
  the plan label this line used to reserve, now attached to the login it
  describes (`docs/accounts.md` §11).
- Rows reorder on an attention sort (working → needs-attention → recent) with
  the 260 ms curve, and a project is ordered by the most urgent row in it, so a
  project with an agent working in it rises the way a row does. Reordering must
  never move the row under the cursor mid-click.

### 3.3 Centre column

- **Home screen** — what the column is before there is a conversation in it,
  which is how the window opens and where every "new chat" leaves it. One
  question at 30px — *What shall we build?*, or *What shall we build in
  `project`?* when one is chosen — over four starter cards (explore, build,
  review, fix) at the measure. A card fills the composer rather than sending
  it: a starter is the first half of a sentence the reader finishes, and a
  prompt that sent itself would start an agent on a question nobody asked. The
  scrolling transcript takes over the moment a prompt is away, before the first
  word arrives, because that is where the activity line lives.
  With a workspace chosen, a quiet line under the cards offers to **resume a
  conversation started in claude or codex**: a dialog lists what the agents'
  own CLIs kept for that directory (newest first, filtered as the reader
  types, each row saying which CLI, how many prompts and how long ago), and
  choosing one makes a session that continues that same vendor thread with
  its recent turns in the transcript. The palette offers the same dialog.
- **Transcript** — one centred column at the measure (720 px), with the composer
  under it at the same width: a conversation stranded against one edge of a wide
  window reads as a mistake rather than as a measure. Virtualized markdown:
  paragraphs, inline code chips, fenced code with tree-sitter highlighting,
  tool-call cards, reasoning blocks (dimmed), diff sidecars, plan-approval and
  ask-user cards with inline buttons. The reader's own words are a tinted
  bubble, right-aligned within the column. User messages and visible answers
  carry compact copy and quote actions. Quote prefers an exact non-blank text
  selection, falling back to the complete visible message, and normalizes it
  into a Markdown blockquote at the end of the existing draft. The command
  palette captures a selection before taking focus and offers the same action,
  so choosing it with the mouse cannot lose the range. While meaningful text
  is selected, a compact keyboard-focusable action floats at the transcript's
  top-right without moving the conversation; its mouse path retains the range
  before the toolkit clears selection on press. Every path leaves a blank line
  for the reply and returns focus to the composer. A turn that
  reports a structured todo/plan snapshot draws one shared **Tasks** card,
  regardless of provider. Later snapshots replace that turn's card in place;
  the provider's internal todo tool row stays hidden. Each visible row carries
  a status mark as well as colour, and the header counts completed actionable
  work while leaving cancelled rows visible but out of the denominator. A turn that
  succeeded prints no outcome of its own, because being
  answered is how a turn says it worked. A delegated agent is one bordered
  parent card rather than a second conversation: its reasoning, messages and
  tool lifecycle sit beneath the original brief, with explicit working,
  completed and failed words and glyphs. The fold retains at most 300 steps;
  the view shows the newest 12 and states how many earlier steps are hidden.
  Each completed turn exposes a compact *Fork* action when another installed,
  signed-in agent is available. Choosing one copies the transcript through
  that exact turn and continues in the same workspace with the receiving
  agent; the turn's transcript sequence, not its visual index, is the boundary.
  The same divider retains the provider, model, reasoning effort and service
  tier captured when that process started. A provider-reported actual model
  refines a requested alias, while later option changes cannot rewrite an
  earlier turn's provenance.
  ⌘F (Ctrl+F elsewhere) opens daemon-side find-in-page above the transcript;
  Enter/Shift+Enter or its buttons move through open-session matches in reading
  order, show the current excerpt and count, and reveal the folded block that
  owns the persisted sequence. The chosen block is visibly highlighted. A
  compact *Prompts (N)* strip expands into a bounded, numbered outline of
  top-level user prompts; choosing one reveals its folded block. Answers to an
  inline question or plan are not promoted into separate outline turns. The
  same toggle is available from the command palette. Workspace file locations
  in settled assistant prose, including inline-code locations, are links into
  the Files editor at the optional line and column. They use the terminal's
  worktree-bound detector; ordinary web links still open normally, and fenced
  code is not rewritten.
- **Activity line** — under the transcript while an agent works: a breathing dot
  and what it is doing (thinking, or the tool it is waiting on). Hidden while
  text is arriving, when the words are the indicator.
- **Composer** — one card: a multi-line auto-growing input with the
  `Do anything…` placeholder, and beneath it the attachment button on the left,
  then the model, effort and tier controls, the mode and agent chips, the
  account chip when that agent has more than one login, and a separate usage
  chip for the chosen login's tightest rate-limit window. The model chip also
  prints the selected effort when that model supports it, so a collapsed
  picker still says what the next turn will run. The percentage is
  printed and *at the wall* appears as a word beside it, never as colour alone
  (`docs/accounts.md` §11). Choosing an account persists it as that provider's
  default for future chats in every client; a continuing session keeps and
  displays its original account. Removing the active named account falls back
  to the provider's implicit default. While a session is working, the circular
  action is Stop when the draft is empty and Send as soon as a follow-up has been typed;
  that send steers or queues according to the driver's existing policy. The
  attachment button selects several files, and files dropped anywhere on the
  conversation column — transcript or composer — take the same upload path,
  with an overlay naming the drop while the drag is over it. Pasting an image while the composer has
  focus attaches the clipboard bytes without changing an ordinary text paste.
  Each route uploads into the daemon-owned store and shows removable filename
  chips without putting opaque references into the editable draft. PNG, JPEG,
  GIF and WebP bytes up
  to 4 MiB carry a local thumbnail in that chip; format detection uses the
  signature rather than the filename, and every other file remains a filename
  chip. An attachment by itself is a sendable prompt; upload errors stay beside
  the chips. `@`
  file mentions and `/` slash commands have an inline filtered menu. `↩`
  sends, `⇧↩` is a newline; sending while busy enqueues when steering is not
  available. Those pending prompts appear as one compact card immediately
  above the composer, in dispatch order. Each row has a stable daemon-owned
  identity and can be edited in the composer, removed, or moved earlier and
  later without creating a transcript entry. *Send now* is enabled only when
  the current live transport accepts steering; a refusal leaves the row in
  place. A queued prompt becomes immutable transcript history at dispatch,
  never at enqueue time. `⌘↩` while an agent works queues even where the
  turn could be steered (the Codex CLI's Tab). The card's head carries the
  count, a *Held* mark, *Hold/Resume* and *Clear*; each row also offers
  *Interrupt & send*, which stops the running turn and sends that row next
  (opencodex's Steer). A Stop or a failed turn holds the queue rather than
  emptying it, and a restarted daemon brings it back held.
  Focus is carried by the card's border at the accent's 55%, never by a hard
  ring: an outline at full strength reads as an error state.
- **Context bar** — a hairline strip under the composer: the project chip on
  the left, branch chip on the right. The project is a chip rather than a label
  because it is a choice — it opens the same list the sidebar offers, plus
  *New project…* and *Work without a project*, so a chat can be aimed without
  going to the sidebar and a project can be registered from the middle of the
  window where the reader already is. Choosing one starts a new conversation
  rather than moving the one on screen: an answer belongs to the worktree it
  was produced in. The branch chip reads local branches through the daemon,
  fuzzily filters them, switches an available branch without changing the
  workspace id, and creates the typed branch when it does not exist. A branch
  held by another worktree stays visible with its path but cannot be selected.
  Beside it, an explicit *Indexed* or *Index workspace* word reports whether
  semantic search is ready. Activating it starts `zg index` in the workspace's
  daemon-owned terminal, opening the dock so progress and failures stay visible.
- **Terminal dock** — a horizontally scrolling tab strip over a terminal surface, followed by fixed `+`, pane, split and find controls that never scroll out with a long tab list. The `+` remains available in an empty workspace dock so the first terminal can be opened there. Each tab title, its close action and `+` are separate keyboard-focusable buttons with action labels. The command palette exposes New Terminal only while a workspace can own it, then the terminal actions that can do something in the current state: split/single, other pane, find, previous/next tab, return to live output and two-step close; choosing one opens a hidden dock before acting. Tabs remain in daemon creation order across reattachment, independent of how their titles sort, and generated shell numbers are not reused while that daemon is alive. Collapsible; its visibility and height are remembered per workspace. The shells belong to the workspace and to the daemon, not to the window: a dock that opens adopts whatever is still running there and replays what it printed while nobody was looking. *Split* opens a second daemon-owned PTY on the right; the active pane has the active tab and focus edge, clicking either pane or activating the keyboard-focusable *Other pane* control changes the keyboard target, choosing a hidden terminal tab replaces only that focused pane, and *Single* returns to the focused pane without stopping the other shell. `⌘⇧[` / `⌘⇧]` (`Ctrl+PageUp` / `Ctrl+PageDown` elsewhere) move through terminal tabs with wrap-around and obey the same focused-pane replacement rule. Terminal input follows the emulated mode for application-cursor keys and encodes reverse Tab, Insert, F1–F12, Ctrl symbols and Alt text; Ctrl/Shift/Alt combinations on navigation and function keys use xterm modifier codes, Shift+Enter and Ctrl+Backspace retain their CLI control bytes, and window-level Cmd/Super shortcuts never leak their printable key into the PTY. `⌘V` (`Ctrl+Shift+V` elsewhere) pastes clipboard text into the active daemon terminal, leaving non-macOS `Ctrl+V` available to the shell as quoted insert; ordinary mode converts line breaks to the carriage returns a PTY expects, while negotiated bracketed-paste mode preserves the block between terminal markers and strips embedded escape bytes so pasted text cannot close that block early. The left/right pair and active pane are remembered per workspace and restored after navigation or a window restart only while the daemon still lists both PTYs. Both PTYs are resized to half width while split and back to full width when the split ends. The wheel and `Shift+PageUp` / `Shift+PageDown` browse that bounded history without sending input to the PTY; an accessible *N lines back* control reports the offset and returns directly to live output. `⌘F` while the terminal has focus, or the find control, searches the live grid and bounded history as literal text: lowercase queries ignore ASCII case, uppercase makes the query exact, Enter / Shift+Enter wrap through the results, and the selected occurrence is highlighted and revealed. Results update as the searched PTY prints, retaining the result number being read and clamping it only when the result set shrinks. Closing is a two-step keyboard-accessible action because it stops a running daemon process: the first activation changes that tab's control to *Close?*, the second closes it, and selecting another tab cancels the pending close. Workspace-relative file locations and absolute locations beneath the daemon-host worktree are underlined links; activating one opens the Files editor at its optional line and column. Links are buttons so keyboard focus and the visible focus ring work without a mouse. Parent traversal, URLs and absolute paths outside the worktree stay plain terminal text.

Dragging across the terminal grid selects exact character cells in either direction and highlights the range; copying prefers that selection, including its line breaks, then falls back to the visible viewport. A non-blank selection can also be quoted into the existing chat draft from the fixed toolbar or command palette without replacing what was already typed. The copy action is exposed whenever the active viewport contains text; `⌘C` (`Ctrl+Shift+C` elsewhere) invokes it, while non-macOS `Ctrl+C` continues to interrupt the foreground process. A click without a drag remains ordinary terminal focus, and blank terminals do not offer no-op copy or quote actions.

- **Conversation tabs** — once a second conversation is open, the column
  header becomes a strip of tabs, one per open conversation plus an unsaved
  *New chat*. `⌘T` opens a new one, `⌘W` closes the current, `⌘⇧]` / `⌘⇧[`
  and `⌃Tab` cycle; the strip is restored on the next launch. A tab whose
  workspace is archived closes itself.
- **Edit and resend** — hovering a sent prompt shows a pencil. Editing fills
  the composer under a banner that says what will happen; sending rewinds the
  worktree to before that prompt and continues in a fork on a fresh thread, so
  the original conversation and its changes are still there.
- **Fan-out and compare** — before a conversation starts, the composer offers
  *Try several ways*: a panel above it with an agent a row and a −/+ count
  each (six attempts at most), and *Try N ways*, which starts every attempt in
  its own worktree and opens each as a tab. A conversation that belongs to a
  fan-out shows *Compare N* in the header, which replaces the column with the
  attempts side by side — agent and branch, state, `+/−` lines, the last
  answer as Markdown — each with *Open*, a two-step *Keep* that archives
  the others, and a two-step *Merge* that first merges the attempt into the
  branch the project is on. A refused merge — a conflict, or uncommitted work
  in the project's checkout — is said above the columns and nothing is
  archived.
- **Second opinion** — each completed turn's footer offers every other ready
  agent as *Second opinion: <agent>*, which forks the conversation there with
  a request to review the work and change nothing.
- **Split diff** — each changes section has *Unified | Split*. Split puts the
  old file on the left and the new on the right, a removed line beside the
  line that replaced it; either side is clickable for a review comment.
- **Project menu** — a project in the rail has a ⋯ that opens *Rename*
  (in place), *Move up* and *Move down*.
- **Scheduled jobs** — Settings lists the project's cron jobs: schedule,
  name, what runs, when it next fires and how it last went, with *Pause*,
  *Run now* and *Remove*, and a form for a new one (command or prompt).
- **Reports** — a cost priced from the public rate table rather than by the
  vendor reads `≈$1.25`; sessions nothing could price are counted as
  `N unpriced` and left out of the total; a footnote says when the table
  was fetched.
- **Notifications** — each kind of news has its own sound (Settings turns
  them off), and the Dock badge counts the sessions waiting on the reader.
- **Quick commands** — the terminal dock's ▶ lists saved commands for the
  project and globally; a shell command opens a terminal named after it, a
  prompt command is sent as if typed. They are managed in Settings.

> **Defaults.** The right panel and the terminal dock start closed. Their
> surfaces land in M3, and two empty panels either side of the conversation is
> a worse first impression than a window that is only what works. Their sizes
> are remembered while they are closed.

### 3.4 Right panel — "surfaces"

A dock area that hosts one or more surfaces: **Terminal** (the workspace's daemon terminals, the same ones the dock shows: while this tab is on screen they are drawn here instead of in the dock, sized to the tab, and every action that would open the dock uses the tab instead), **Git** (status + diff + commit + per-file staging and review comments), **Files** (show a directory-first expandable tree while the query is empty; typing switches to fuzzy path and full-text results; scope chips search either this workspace or every active workspace in its project, and a project hit switches workspace before opening; keep several independently editable `CodeEditor` tabs, move through file visits with back/forward, toggle a live-buffer Markdown preview, find/replace, save, add a saved selection's exact line location to chat, or paste the current selection into the active terminal; stale revisions stay in their tab with the refusal shown, and dirty tabs refuse to close), **Browser** (one page per workspace — Chromium drawn as a GPUI element on macOS, a WebView2 child window on Windows — with address, back/forward/reload, find and inspect controls), **Reports** (usage by day, agent and account, with each account's rate-limit windows and the age of the reading), and **Skills** (the selected project's and user's agent skills, grouped by name with every install path and one all-copies enable/disable action). Sending a selection to the terminal opens the dock and a daemon-owned shell when necessary, preserves the selected text including its own newlines, and never appends an extra Return. The tree retains at most 2,000 files, says when it was truncated, and leaves search able to reach the complete daemon catalogue. Project search is globally bounded and excludes archived worktrees. Skills search matches names, descriptions, provider roots and daemon-host paths; scope and grouped enablement facets compose with the query, keep the catalogue's stable order, and show the visible/total count. Each path has a copy action. Installed local language servers provide hover, diagnostics and definition jumps across workspace file tabs; targets outside the canonical worktree are not opened, and missing or failed servers leave the editor in syntax-only mode. An externally addressed daemon also stays syntax-only because its daemon-host worktree path is never handed to a client-host language-server process. Browser inspect mode highlights the hovered DOM element, intercepts one click, and presents bounded selector, HTML, curated computed style, accessibility, bounds and development source context for review before appending it to the composer; page-provided content is validated again in Rust. Markdown preview renders no repository-named image or raw HTML image. PNG, JPEG, GIF and WebP files recognized from their bytes render through a separate local preview, bounded to 4 MiB before base64 wire encoding; unsupported and oversized binaries remain explanatory empty states. An image attachment opens a full-size annotation dialog from its thumbnail. The dialog offers pen, highlight, arrow, rectangle, ellipse and text tools plus undo and clear; attaching replaces that draft item with a self-contained SVG containing the immutable source image and marks, uploaded through the ordinary daemon attachment path. Empty state is a centred title, one line of help, and a stacked list of large surface buttons; the toolbar plus returns to it, while `⌘⌥←/→` cycles directly and wraps at either end. Each surface is a tab of the right panel's dock area: tabs are reordered, grouped and split by dragging, and the arrangement persists per workspace. The dock's tab bar is ours (`src/surface_dock.rs`): the same rounded tab as the conversation strip, with its close control, on a clear strip with one hairline under it — no rules between tabs or before the toolbar. The panel header's right-hand controls fill the window with the panel (hiding the rail, session list and centre until pressed again; not persisted) and close the panel, as `⌘⌥B` does. A header strip's sun or moon beside Settings, and a palette entry, switch between the dark and light themes.

The Git surface keeps Pull and Push visible even when the worktree is clean. Pull is a daemon-owned fast-forward-only operation; dirty or diverged branches leave the worktree untouched and show the refusal inline without clearing a commit-message draft.

History beside those controls expands a bounded newest-first commit graph without hiding the current diff. Each row is a lane drawing — a dot per commit, a lane per line of history, curves where branches fork and merge, laid out by `ginka_ui::graph` from the parent ids alone and coloured from the status palette — then the subject, short id and relative age. A row opens a read-only view of what that commit did against its first parent, with the way back to the uncommitted diff at its head. *Create PR* beside Push pushes the branch and opens a pull request with `gh`; the answer is a link to it, or the refusal in `gh`'s own words.

The diff list separates worktree-only changes from staged changes. The same path may appear in both when only some hunks are staged; each hunk header has an accessible Stage or Unstage action, and unstaged hunks also have a two-step Discard action. Stage, unstage and discard all regenerate the corresponding side of the diff; a stale header reports its refusal inline instead of applying to another hunk. Discard reverses only the worktree-versus-index patch, preserving already staged edits in the same file.

### 3.5 Inbox

Built with `--features github`: the e1 GitHub client, mounted as pieces rather
than as its window. A 360 px list column — the place's header, the
connections (GitHub, and *Add connection*, which says GitHub is the one there
is), section chips (Inbox, My PRs, Reviews, Assigned) and e1's list — then e1's
detail in the centre and its *Ask* pane at the far right while open. e1's own
navigation, window controls and palette stay behind. The rail shows a dot
beside *Inbox* while anything is unread, known only once the client has been
opened. Without the feature, the list column keeps its header and the centre
names the build command.

Both apps read one palette: Ginka installs e1's tokens alongside its own on
every theme change, so a system appearance switch reaches both.

### 3.6 Notes

A 360 px list of the notes — the chosen project's, or every one — most
recently touched first, with a new-note action, and the note being written in
the centre at 720 px: its title, then its markdown, with *Edit / Preview*
segments and a two-step delete. Typing saves after a 600 ms pause; there is no
save button. The daemon keeps them (`ginka notes`, MCP).

### 3.7 Settings

Appearance (System, Dark, Light), language (System, English, 日本語) and
notifications (on, off), each
a segmented control that applies at once and persists to `app.json`, and the
version with the state directory. Opened from the rail or `⌘,`.

## 4. Component mapping

| Region | Component source |
| --- | --- |
| Window shell, custom title bar | `gpui-component` `TitleBar` + `Root` |
| Sidebar container, collapse | `gpui-component` `Sidebar` (see its `examples/sidebar`) |
| Session list | `gpui-component` virtualized `List` with a custom row delegate |
| Right panel / terminal dock / splits | `gpui-component` **Dock** (`DockArea`, `Panel`, `TabPanel`, resizable + draggable tabs) |
| Transcript markdown | `gpui-component` `Markdown` |
| Code blocks, editor, diff | `gpui-component` `CodeEditor` (tree-sitter + LSP) |
| Composer input | `gpui-component` `Input` (multi-line) + our own mention/slash overlay |
| Command palette, quick open | `gpui-component` `Modal` + `List`, filtered with `nucleo-matcher` |
| Model / mode pickers | `gpui-component` `Dropdown` / `Popover` |
| Buttons, chips, badges, tooltips, avatar | `gpui-component` primitives |
| Terminal grid | our `ginka-terminal` view over `alacritty_terminal` |
| Agent status glyphs | ours (see §5) |
| Charts (Reports) | `gpui-component` charts |
| Embedded browser (M5) | `gpui-cef` (Chromium off-screen on macOS, WebView2 on Windows), pinned by revision |

The model picker is a searchable popover with a provider rail. The rail shows
each provider's own mark (Claude's starburst, Codex's prompt cloud, Gemini's
sparkle, OpenCode's frame) in its colour, and the model chip carries the same mark; the sidebar keeps
the neutral agent glyphs, because a row is scanned rather than chosen. It
renders only installed CLI catalogues and their supported option metadata,
never a view-owned model list. A CLI that reports itself signed out stays on
the rail, dimmed, and its list opens with a note to sign in — hiding it made
the provider look absent when the fix is one login away. A live catalogue wins over the driver's static offline
fallback. (Codex exposes `model/list`; Claude currently needs its stable alias
fallback because its CLI exposes no equivalent catalogue operation.) The last
valid model chosen per provider is restored; when a
later catalogue removes that id, the chip quietly returns to provider default.
Effort and tier chips appear only when the selected model advertises choices.
On an existing conversation they update the options used by its next turn;
the daemon asks the driver whether the provider thread can absorb that change.
When it cannot, the same row adopts a replacement session whose first turn is
given a bounded digest of the conversation so far.

Usage is a separate composer chip and therefore remains visible even when a
provider has only one login. The active transcript's cumulative input plus
output tokens appear beside the tightest rate-limit window; cache and reasoning
breakdowns are not counted twice. When a provider reports both current-context
tokens and its model capacity, the same chip adds their occupancy and percentage
as a distinct reading — cumulative session totals are never divided by a model
limit, because compaction would make that percentage false. Turn events push
these readings into it immediately; clicking requests a fresh rate-limit
reading. It is never refreshed by a timer. When the same provider reading says
manual compaction is supported, an adjacent **Compact** control starts the
provider-owned operation as a separate resumed turn. The control disappears
while a turn is active; compaction never enters the ordinary steer-or-queue
path.

## 5. What we build ourselves

Four things are not in any library and are load-bearing for the product's identity:

1. **Agent status glyphs** — a small animated mark per provider that also encodes state (idle / working / attention). Bezel's orbs are worth looking at for the motion; the implementation is ours.
2. **Glass/vibrancy theme layer** — translucent window background with a platform blur behind it, written against the platform API directly.
3. **Transcript event views** — tool-call cards, reasoning blocks, plan approval, ask-user, diff sidecars. These are specific to our `AgentEvent` model.
4. **Terminal view** — `alacritty_terminal` grid rendering, selection, file-path linkification, reattach/replay.

## 6. Interaction rules

- **Keyboard-first.** Every action reachable from the command palette (`⌘K`); no action mouse-only. `⌘P` quick open, `⌘⇧F` search, `⌘1..9` switch among the first nine visible active sessions in the selected project, `⌘[` / `⌘]` moves through project/session history (`Ctrl+Alt+↑/↓` elsewhere), `⌘⇧[` / `⌘⇧]` cycles terminal tabs while the terminal is focused (`Ctrl+PageUp/PageDown` elsewhere), and `⌘⌥←/→` cycles surfaces. Session numbers follow the same attention-first stable order as the list; archived or search-hidden rows do not take a number, and a number beyond the visible rows does nothing.
- **Panels open and close independently**, on VS Code's chords: `⌘B` sidebar, `⌘⌥B` right panel, `⌘J` terminal dock (`ctrl` elsewhere). Each also has a title-bar control, and the control's icon reports the *state* rather than the action — an open panel shows the "close" variant — so it reads without hovering. A closed panel keeps its size. Sidebar visibility and width are global navigation preferences; right-panel visibility/width, terminal visibility/height and the active surface persist by immutable workspace id in `app.json`, so switching or restarting restores what that workspace last showed. The centre column is not a panel and cannot be closed.
- **Focus is explicit.** A visible ring on the focused pane; `⌘K` never steals focus from a running terminal without returning it.
- **No blocking modals** except destructive confirmations (delete worktree, force push).
- **Never auto-scroll away from a user-scrolled transcript.** Pin-to-bottom only while already at the bottom.
- **Truncate paths from the left**, so the meaningful tail stays visible.

## 7. Accessibility

AA contrast for all token pairs in both themes. Full keyboard traversal. Respect the system reduce-motion setting — when set, transitions become instant and the status glyph stops animating (it still changes colour and shape). Minimum hit target 28 px.
