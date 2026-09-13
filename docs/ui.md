# Ginka UI Specification

> Companion to [`roadmap.md`](roadmap.md). The roadmap says *what* we build and when; this document says *what it looks like* and *which components render it*.
> Last updated: 2026-09-05

## 1. Design direction

The reference is a three-column agent workstation on a dark, glass-tinted surface:

```
┌──────────────────────────────────────────────────────────────────────────────────┐
│ ●●●  ⬓ ← →  │ ⬔ Session Title   org @ device        │  +            ⤢  ⬓        │
├─────────────┼───────────────────────────────────────┼──────────────────────────  │
│ ⌸ comet     │                                       │                            │
│   @ device  │   transcript                          │   right panel              │
│      ⌄  +   │   (markdown, tool cards,              │   ("surfaces")             │
│             │    reasoning, diffs)                  │                            │
│ ▸ session   │                                       │   empty state:             │
│ ▸ session   │                                       │     Open a surface         │
│ ▸ session   │                                       │     ┌────────────────┐     │
│             │                                       │     │ ▢ Terminal     │     │
│ Archived  ⌄ ├───────────────────────────────────────┤     ├────────────────┤     │
│ ▸ ...       │  ┌─ composer ─────────────────────┐   │     │ ⑂ Git          │     │
│ ▸ ...       │  │ Do anything…   [model][mode]▲ │   │     └────────────────┘     │
│ Show 25 more│  └────────────────────────────────┘   │                            │
│             │  ▤ Worktree            ⑂ branch-name  │                            │
├─────────────┼───────────────────────────────────────┤                            │
│ (W) Wing Lee│  [ ubuntu ×] [+]                   ⌄  │                            │
│     Alpha   │  ubuntu@dev:~/.ginka/worktrees/…$ █   │                            │
└─────────────┴───────────────────────────────────────┴────────────────────────────┘
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
| `bg.window` | `#0E0A14` @ 82 % + blur | window base, translucent |
| `bg.sidebar` | `#120D19` @ 70 % | left column |
| `bg.surface` | `#1B1426` @ 66 % | cards, composer, fields |
| `bg.raised` | `#241A33` @ 72 % | popovers, menus |
| `bg.terminal` | `#0A0710` | terminal pane |
| `border.subtle` | `#FFFFFF` @ 6 % | panel separators |
| `border.strong` | `#FFFFFF` @ 12 % | focused input, selected row |
| `text.primary` | `#EDE9F5` | titles, transcript body |
| `text.secondary` | `#A79FBC` | subtitles, metadata |
| `text.muted` | `#6F6885` | timestamps, placeholders |
| `accent` | `#A78BFA` | selection, links, focus ring |
| `status.working` | `#F472B6` | running agent |
| `status.attention` | `#FBBF24` | agent waiting on the user |
| `status.done` | `#4ADE80` | completed |
| `status.error` | `#F87171` | failed |
| `code.bg` | `#A78BFA` @ 10 % | inline code |

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

A light theme ships with the same token names and WCAG AA contrast pairs. Every colour is referenced by token; adding a theme must never require touching a view.

### Type

| Role | Font | Size / line-height | Weight |
| --- | --- | --- | --- |
| Transcript body | UI sans (Geist / Inter) | 15 / 1.65 | 400 |
| Session title | UI sans | 14 / 1.3 | 500 |
| Metadata | UI sans | 11.5 / 1.3 | 400 |
| Code / terminal | Mono (Geist Mono / JetBrains Mono) | 13 / 1.5 | 400 |

### Geometry

4 px spacing grid. Radii: window 12, **card 16** (the composer and anything else
holding a group of controls — a card is an object on the surface and the corner
is what says so), panel 10, row 9. Chips are rows, not pills: 28 px tall, 9 px
radius, 12 px type. Sidebar default 250 px (resizable 200–400). Right panel
default 420 px (resizable, collapsible). Terminal dock default 30 % of the centre
column height.

## 3. Regions

### 3.1 Headers — there is no title bar

The window has no bar of its own. Each column paints itself to the top of the
window and carries its own 44 px header, and the three sit on one line, so the
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

### 3.2 Left sidebar

A tree: workspaces under the project they belong to, the way files sit under a
folder. The project was a line on every row until 2026-09-03, which spent a line
per row repeating what a heading says once, and left a reader scanning for
"which project is this" with nowhere single to look.

- **Header** — app name (bold) + chevron, then search and `+` new session.
- **New chat** — the first row under the header. It clears the centre column
  for a conversation aimed at whatever is selected: a project, the project of
  the selected workspace, or nothing at all.
- **Section label** — `Projects`, small and muted, over the run of groups, with
  a `+` beside it that registers another. The reader who has one project wants
  the second added from the same place, and an empty state is by definition not
  there any more once they do.
- **Project heading** — folder icon + project name, muted, and **selectable**:
  a project is something the reader picks *before* there is a conversation, and
  picking one shows the home screen aimed at it. Selected only when the project
  itself is what the centre column shows — a selected row already says which
  project it is in, and two highlights read as two selections.
- **Nothing registered** — a muted line under the section label, the way to add
  one, and the `ginka project add .` command. Not a takeover of the sidebar: a
  window with no project is still a window you can talk to (the chat runs in a
  scratch worktree), so the list says what is missing without implying nothing
  works until it is fixed.
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
  answered is how a turn says it worked.
- **Activity line** — under the transcript while an agent works: a breathing dot
  and what it is doing (thinking, or the tool it is waiting on). Hidden while
  text is arriving, when the words are the indicator.
- **Composer** — one card: a multi-line auto-growing input with the
  `Do anything…` placeholder, and beneath it the attachment button on the left,
  then the agent chip (which says when the agent is missing or signed out),
  the account chip when that agent has more than one login — its label and
  the tightest rate-limit window as *percent · reset*, the number printed and
  *at the wall* a word beside it, never a colour alone (`docs/accounts.md`
  §11) — the mode chip, and the circular send button — which becomes a stop button while a
  session is working. `@` file mentions and `/` slash commands with an inline
  filtered menu. `↩` sends, `⇧↩` is a newline; sending while busy enqueues.
  Focus is carried by the card's border at the accent's 55%, never by a hard
  ring: an outline at full strength reads as an error state.
- **Context bar** — a hairline strip under the composer: the project chip on
  the left, branch on the right. The project is a chip rather than a label
  because it is a choice — it opens the same list the sidebar offers, plus
  *New project…* and *Work without a project*, so a chat can be aimed without
  going to the sidebar and a project can be registered from the middle of the
  window where the reader already is. Choosing one starts a new conversation
  rather than moving the one on screen: an answer belongs to the worktree it
  was produced in.
- **Terminal dock** — tab strip (tab title + close, `+`, overflow chevron) over a terminal surface. Collapsible; remembers its height per workspace. The shells belong to the workspace and to the daemon, not to the window: a dock that opens adopts whatever is still running there and replays what it printed while nobody was looking.

> **Defaults.** The right panel and the terminal dock start closed. Their
> surfaces land in M3, and two empty panels either side of the conversation is
> a worse first impression than a window that is only what works. Their sizes
> are remembered while they are closed.

### 3.4 Right panel — "surfaces"

A dock area that hosts one or more surfaces: **Terminal**, **Git** (status + diff + commit + per-file staging and review comments), **Files** (find a file, keep several independently editable `CodeEditor` tabs, move through file visits with back/forward, toggle a live-buffer Markdown preview, find/replace, save, and add a saved selection's exact line location to chat; stale revisions stay in their tab with the refusal shown, and dirty tabs refuse to close). Installed local language servers provide hover, same-file definition jumps and diagnostics; missing or failed servers leave the editor in syntax-only mode. **Editor** is the later full code surface with a tree and cross-file navigation. **Browser** lands in M5. **Reports** shows usage by day, agent and account, with each account's rate-limit windows and the age of the reading. Markdown preview renders no repository-named image or raw HTML image. PNG, JPEG, GIF and WebP files recognized from their bytes render through a separate local preview, bounded to 4 MiB before base64 wire encoding; unsupported and oversized binaries remain explanatory empty states. Empty state is a centred title, one line of help, and a stacked list of large surface buttons. Surfaces are draggable between the right panel and the centre dock, and the arrangement persists per workspace.

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

The model picker renders the provider catalogue's labels and supported option
metadata, never a view-owned model list. A live catalogue wins over the static
offline fallback. The last valid model chosen per provider is restored; when a
later catalogue removes that id, the chip quietly returns to provider default.
Effort and tier chips appear only when the selected model advertises choices.
On an existing conversation they update the options used by its next turn;
the daemon asks the driver whether the provider thread can absorb that change.
When it cannot, the same row adopts a replacement session whose first turn is
given a bounded digest of the conversation so far.

## 5. What we build ourselves

Four things are not in any library and are load-bearing for the product's identity:

1. **Agent status glyphs** — a small animated mark per provider that also encodes state (idle / working / attention). Bezel's orbs are worth looking at for the motion; the implementation is ours.
2. **Glass/vibrancy theme layer** — translucent window background with a platform blur behind it, written against the platform API directly.
3. **Transcript event views** — tool-call cards, reasoning blocks, plan approval, ask-user, diff sidecars. These are specific to our `AgentEvent` model.
4. **Terminal view** — `alacritty_terminal` grid rendering, selection, file-path linkification, reattach/replay.

## 6. Interaction rules

- **Keyboard-first.** Every action reachable from the command palette (`⌘K`); no action mouse-only. `⌘P` quick open, `⌘⇧F` search, `⌘1..9` switch session, `⌘⌥←/→` cycle surfaces.
- **Panels open and close independently**, on VS Code's chords: `⌘B` sidebar, `⌘⌥B` right panel, `⌘J` terminal dock (`ctrl` elsewhere). Each also has a title-bar control, and the control's icon reports the *state* rather than the action — an open panel shows the "close" variant — so it reads without hovering. A closed panel keeps its size and the whole arrangement persists to `app.json`, so a restart restores what the user left. The centre column is not a panel and cannot be closed.
- **Focus is explicit.** A visible ring on the focused pane; `⌘K` never steals focus from a running terminal without returning it.
- **No blocking modals** except destructive confirmations (delete worktree, force push).
- **Never auto-scroll away from a user-scrolled transcript.** Pin-to-bottom only while already at the bottom.
- **Truncate paths from the left**, so the meaningful tail stays visible.

## 7. Accessibility

AA contrast for all token pairs in both themes. Full keyboard traversal. Respect the system reduce-motion setting — when set, transitions become instant and the status glyph stops animating (it still changes colour and shape). Minimum hit target 28 px.
