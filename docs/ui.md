# Ginka UI Specification

> Companion to [`roadmap.md`](roadmap.md). The roadmap says *what* we build and when; this document says *what it looks like* and *which components render it*.
> Last updated: 2026-08-31

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

### 3.1 Title bar

Custom, `platform_title_bar` style. Left: traffic lights, sidebar toggle, back/forward history. Centre-left of the centre column: session icon + title + `org @ device` subtitle. Right of the right panel: new-surface `+`, expand, panel toggle. Drag anywhere empty moves the window.

### 3.2 Left sidebar

A tree: workspaces under the project they belong to, the way files sit under a
folder. The project was a line on every row until 2026-09-03, which spent a line
per row repeating what a heading says once, and left a reader scanning for
"which project is this" with nowhere single to look.

- **Header** — app name (bold) + chevron, then search and `+` new session.
- **Section label** — `Projects`, small and muted, over the run of groups.
- **Project heading** — folder icon + project name, muted.
- **Workspace row**, indented under its project (8 px radius, selected =
  `bg.raised` fill):
  1. Agent glyph, session title, right-aligned status: relative time (`now`,
     `46m`, `4h`) **or** a status pill (animated dot + `Working`).
  2. *Only when it says something the title does not:* git-branch icon +
     branch name (truncated from the left), the dirty dot, and the divergence.
     A workspace is named after the branch it was cut on, so this line appears
     when an agent has checked out something else inside the worktree — which
     is exactly when it matters.
- **Archived section** — collapsible header, one-line rows (glyph, title, age),
  `Show N more` footer.
- **Footer** — avatar, user name, plan/channel label.
- Rows reorder on an attention sort (working → needs-attention → recent) with
  the 260 ms curve, and a project is ordered by the most urgent row in it, so a
  project with an agent working in it rises the way a row does. Reordering must
  never move the row under the cursor mid-click.

### 3.3 Centre column

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
  then the agent chip (which says when the agent is missing or signed out), the
  mode chip, and the circular send button — which becomes a stop button while a
  session is working. `@` file mentions and `/` slash commands with an inline
  filtered menu. `↩` sends, `⇧↩` is a newline; sending while busy enqueues.
  Focus is carried by the card's border at the accent's 55%, never by a hard
  ring: an outline at full strength reads as an error state.
- **Context bar** — a hairline strip under the composer: worktree label on the left, branch on the right. Click either to switch.
- **Terminal dock** — tab strip (tab title + close, `+`, overflow chevron) over a terminal surface. Collapsible; remembers its height per workspace.

> **Defaults.** The right panel and the terminal dock start closed. Their
> surfaces land in M3, and two empty panels either side of the conversation is
> a worse first impression than a window that is only what works. Their sizes
> are remembered while they are closed.

### 3.4 Right panel — "surfaces"

A dock area that hosts one or more surfaces: **Terminal**, **Git** (status + diff + commit), **Files**, **Editor**, **Browser** (M5), **Reports**. Empty state is a centred title, one line of help, and a stacked list of large surface buttons. Surfaces are draggable between the right panel and the centre dock, and the arrangement persists per workspace.

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

## 5. What we build ourselves

Four things are not in any library and are load-bearing for the product's identity:

1. **Agent status glyphs** — a small animated mark per provider that also encodes state (idle / working / attention). Bezel's `agent` crate (orbs + avatar, MIT) is the reference implementation to port.
2. **Glass/vibrancy theme layer** — translucent window background with a platform blur behind it. Bezel's `theme/glass.rs` is the reference.
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
