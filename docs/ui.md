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
5. **Motion is short and consistent.** ~260 ms ease-out for list reordering and panel transitions; ~120 ms for hover/press. Nothing bounces.

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
| `text.primary` | `#EDEAF4` | titles, transcript body |
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

A light theme ships with the same token names and WCAG AA contrast pairs. The
schema, palette, 13 px toolkit base and 12 px mono base intentionally match e1;
Kirikumo consumes the same token contract. Keeping these as serialized tokens,
instead of view constants, leaves the three applications able to share a theme
crate later without rewriting their views.

### Type

| Role | Font | Size / line-height | Weight |
| --- | --- | --- | --- |
| Toolkit base | UI sans (system / Inter) | 13 / toolkit default | 400 |
| Transcript body | UI sans | 15 / 1.65 | 400 |
| Session title | UI sans | 14 / 1.3 | 500 |
| Metadata | UI sans | 11–12 / 1.3 | 400 |
| Code / terminal | Mono (system mono / JetBrains Mono) | 12–13 / 1.5 | 400 |

### Geometry

4 px spacing grid. Radii: window 12, **card 16** (the composer and anything else
holding a group of controls — a card is an object on the surface and the corner
is what says so), panel 10, row 9. Chips are rows, not pills: 28 px tall, 9 px
radius, 12 px type. Navigator default 520 px (resizable 420–720), with a fixed
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
  the back/forward history. It is the sidebar's header while there is a
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

- **Project rail** — app header, the `Projects` action row, then selectable
  project rows. The selected project remains highlighted while one of its
  sessions is open.
- **Session list header** — `Workspace` and a `+` for a new session. The whole
  sessions column is absent until a project is selected.
- **Conversation search** — directly below the header, with the rail's search
  icon focusing it and an explicit clear action. Matching is fuzzy and local
  over the row metadata already pushed by the daemon: title, model, provider,
  project and branch. Transcript contents remain the daemon-side N10 search;
  filtering this list never starts a second content-search path in the view.
- **New chat** — the first row under the header. It clears the centre column
  for a conversation aimed at whatever is selected: a project, the project of
  the selected workspace, or nothing at all.
- **Section label** — `Projects`, small and muted, over the run of groups, with
  a `+` beside it that opens the add-project modal. The modal keeps the
  reader-facing project name and source-folder chooser in one card; selecting
  a folder prefills its basename, and successful registration selects the new
  project immediately. Escape, the close control and Cancel all dismiss it.
- **Project heading** — folder icon + project name, muted, and **selectable**:
  a project is something the reader picks *before* there is a conversation, and
  picking one shows the home screen aimed at it. It stays highlighted while a
  session inside it is selected, because the two highlights identify different
  navigation levels.
- **Nothing registered** — one muted line under the section label. The adjacent
  `+` remains the single visual way into project registration; the CLI remains
  available without being repeated in the empty rail.
- **Workspace row**, indented under its project (8 px radius, selected =
  `bg.raised` fill):
  1. Agent glyph, session title, right-aligned status: relative time (`now`,
     `46m`, `4h`) **or** a status pill (animated dot + `Working`).
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
- **Transcript** — one centred column at the measure (780 px), with the composer
  under it at the same width: a conversation stranded against one edge of a wide
  window reads as a mistake rather than as a measure. Virtualized markdown:
  paragraphs, inline code chips, fenced code with tree-sitter highlighting,
  tool-call cards, reasoning blocks (dimmed), diff sidecars, plan-approval and
  ask-user cards with inline buttons. The reader's own words are a tinted
  bubble, right-aligned within the column. A finished answer carries a copy
  action; a turn that succeeded prints no outcome of its own, because being
  answered is how a turn says it worked. A delegated agent is one bordered
  parent card rather than a second conversation: its reasoning, messages and
  tool lifecycle sit beneath the original brief, with explicit working,
  completed and failed words and glyphs. The fold retains at most 300 steps;
  the view shows the newest 12 and states how many earlier steps are hidden.
  Each completed turn exposes a compact *Fork* action when another installed,
  signed-in agent is available. Choosing one copies the transcript through
  that exact turn and continues in the same workspace with the receiving
  agent; the turn's transcript sequence, not its visual index, is the boundary.
  ⌘F (Ctrl+F elsewhere) opens daemon-side find-in-page above the transcript;
  Enter/Shift+Enter or its buttons move through open-session matches in reading
  order, show the current excerpt and count, and reveal the folded block that
  owns the persisted sequence. The chosen block is visibly highlighted.
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
  (`docs/accounts.md` §11). While a session is working, the circular action is
  Stop when the draft is empty and Send as soon as a follow-up has been typed;
  that send steers or queues according to the driver's existing policy. `@`
  file mentions and `/` slash commands have an inline filtered menu. `↩`
  sends, `⇧↩` is a newline; sending while busy enqueues when steering is not
  available.
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
- **Terminal dock** — tab strip (tab title + close, `+`, overflow chevron) over a terminal surface. Collapsible; its visibility and height are remembered per workspace. The shells belong to the workspace and to the daemon, not to the window: a dock that opens adopts whatever is still running there and replays what it printed while nobody was looking.

> **Defaults.** The right panel and the terminal dock start closed. Their
> surfaces land in M3, and two empty panels either side of the conversation is
> a worse first impression than a window that is only what works. Their sizes
> are remembered while they are closed.

### 3.4 Right panel — "surfaces"

A dock area that hosts one or more surfaces: **Terminal**, **Git** (status + diff + commit + per-file staging and review comments), **Files** (show a directory-first expandable tree while the query is empty; typing switches to fuzzy path and full-text results; scope chips search either this workspace or every active workspace in its project, and a project hit switches workspace before opening; keep several independently editable `CodeEditor` tabs, move through file visits with back/forward, toggle a live-buffer Markdown preview, find/replace, save, and add a saved selection's exact line location to chat; stale revisions stay in their tab with the refusal shown, and dirty tabs refuse to close), **Reports** (usage by day, agent and account, with each account's rate-limit windows and the age of the reading), and **Skills** (the selected project's and user's agent skills, grouped by name with every install path and one all-copies enable/disable action). The tree retains at most 2,000 files, says when it was truncated, and leaves search able to reach the complete daemon catalogue. Project search is globally bounded and excludes archived worktrees. Skills search matches names, descriptions, provider roots and daemon-host paths; scope and grouped enablement facets compose with the query, keep the catalogue's stable order, and show the visible/total count. Each path has a copy action. Installed local language servers provide hover, diagnostics and definition jumps across workspace file tabs; targets outside the canonical worktree are not opened, and missing or failed servers leave the editor in syntax-only mode. **Browser** lands in M5. Markdown preview renders no repository-named image or raw HTML image. PNG, JPEG, GIF and WebP files recognized from their bytes render through a separate local preview, bounded to 4 MiB before base64 wire encoding; unsupported and oversized binaries remain explanatory empty states. Empty state is a centred title, one line of help, and a stacked list of large surface buttons; the toolbar plus returns to it, while `⌘⌥←/→` cycles directly and wraps at either end. Surfaces are draggable between the right panel and the centre dock, and the arrangement persists per workspace.

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
| Embedded browser (M5) | `gpui-component` `webview` crate |

The model picker is a searchable popover with a provider rail. It renders only
installed, usable CLI catalogues and their supported option metadata, never a
view-owned model list. A live catalogue wins over the driver's static offline
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
provider has only one login. Turn events push new readings into it immediately;
clicking it requests a fresh reading. It is never refreshed by a timer.

## 5. What we build ourselves

Four things are not in any library and are load-bearing for the product's identity:

1. **Agent status glyphs** — a small animated mark per provider that also encodes state (idle / working / attention). Bezel's orbs are worth looking at for the motion; the implementation is ours.
2. **Glass/vibrancy theme layer** — translucent window background with a platform blur behind it, written against the platform API directly.
3. **Transcript event views** — tool-call cards, reasoning blocks, plan approval, ask-user, diff sidecars. These are specific to our `AgentEvent` model.
4. **Terminal view** — `alacritty_terminal` grid rendering, selection, file-path linkification, reattach/replay.

## 6. Interaction rules

- **Keyboard-first.** Every action reachable from the command palette (`⌘K`); no action mouse-only. `⌘P` quick open, `⌘⇧F` search, `⌘1..9` switch session, `⌘⌥←/→` cycle surfaces.
- **Panels open and close independently**, on VS Code's chords: `⌘B` sidebar, `⌘⌥B` right panel, `⌘J` terminal dock (`ctrl` elsewhere). Each also has a title-bar control, and the control's icon reports the *state* rather than the action — an open panel shows the "close" variant — so it reads without hovering. A closed panel keeps its size. Sidebar visibility and width are global navigation preferences; right-panel visibility/width, terminal visibility/height and the active surface persist by immutable workspace id in `app.json`, so switching or restarting restores what that workspace last showed. The centre column is not a panel and cannot be closed.
- **Focus is explicit.** A visible ring on the focused pane; `⌘K` never steals focus from a running terminal without returning it.
- **No blocking modals** except destructive confirmations (delete worktree, force push).
- **Never auto-scroll away from a user-scrolled transcript.** Pin-to-bottom only while already at the bottom.
- **Truncate paths from the left**, so the meaningful tail stays visible.

## 7. Accessibility

AA contrast for all token pairs in both themes. Full keyboard traversal. Respect the system reduce-motion setting — when set, transitions become instant and the status glyph stops animating (it still changes colour and shape). Minimum hit target 28 px.
