# e1 Roadmap

> Status: **M0 and M1 landed; M2 in progress**.
> Last updated: 2026-09-28

e1 now lives in Ginka's Cargo workspace. The standalone binary and embedded
views share its root `Cargo.lock`, toolkit revision, and release tag. This
roadmap keeps the remaining product milestones for both entry points.

## 1. Vision

**e1 is a native GitHub client, written in Rust on [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui), built to stand on its own and to be embedded in [Ginka](https://github.com/bokuweb/ginka).**

Ginka is an orchestrator for coding agents. The work those agents produce ends up on GitHub as pull requests, and the work they are asked to do starts there as issues and review requests. Today that half of the loop lives in a browser tab. e1 is that tab as a native surface: the inbox, the pull requests waiting on you, the issues assigned to you, and any repository's open work, in a window that looks and behaves like Ginka's — so that, once it works on its own, the same views can be mounted inside Ginka's right panel.

Three properties drive every decision below:

1. **Same bones as Ginka.** The same toolkit at the same rev, the same token schema, the same three-column frameless window with its optional far-right agent pane, the same crate split. Sameness is not aesthetic here; it is what makes embedding a mount rather than a port.
2. **Standalone and embedded.** e1 remains a useful GitHub client on its own, with no Ginka process anywhere. Its views also mount in Ginka from the same source tree (§4.3).
3. **Local-first, token-only.** A GitHub token is the only requirement. It is found in the environment or in `gh`, or obtained by signing in from the window and kept in the platform keychain; it never touches a plain file, and nothing needs an account beyond that.

## 2. What we take from the references

- **Ginka** (`bokuweb/ginka`) — the window: frameless, glass over a blurred desktop, three resizable columns with per-column header strips that drag the window. The crate split (`*-ui` holds what is testable without a window; the binary holds only views — here, the views move to a library). The tokens, verbatim in schema. The conventions: rustdoc on every public item, test-first for decisions, en+ja from the first commit.
- **GitHub's own web client** — what a person expects to find: notifications grouped by repository with the reason, the pull request header (base ← head, +/-, checks, review decision), the issue timeline. Read for what to show; the rendering is ours.
- **`gh` CLI** — the token, and the search syntax (`is:pr review-requested:@me`) that names the fixed sidebar sections.

## 3. Scope

### 3.1 v1.0 definition of done

A user can: open the window → see the unread inbox, the pull requests they authored, the ones waiting for their review and the issues assigned to them → pick a repository and list its open pull requests or issues, open or closed → open any item and read its description and comments → open it on GitHub → and have the arrangement of the window survive a restart. Then, in Ginka: the same views mounted as a surface, drawing through Ginka's daemon.

### 3.2 Non-goals for v1

- Being a general-purpose git client. Local repositories are Ginka's business.
- Writing to GitHub before reading it is right. Marking read, commenting and reviewing land in M2 and M3, after the read path is trusted.
- GitHub Enterprise Server, multiple accounts. One token, `api.github.com`.
- Its own offline database. Two caches — answers with their `ETag`s, and a snapshot of the store's memory — are what makes a launch instant and a refresh cheap (§4.8); a queryable database waits until something needs a query.

## 4. Architecture

### 4.1 Process model

One application process and no daemon: GitHub is the remote, and the token is the only durable identity state, and it is not ours. The window holds view state and in-memory caches only; closing it loses nothing but a few seconds of fetching. An Ask turn starts one bounded installed-CLI child process in structured-output mode, then keeps only the returned session id so the next turn can resume it.

```
┌────────────────────────────────────────────┐          ┌────────────────┐
│  e1 (GPUI app)                             │  HTTPS   │  api.github.com│
│  src/main.rs   lifecycle + window          │◄────────►│                │
│  e1-views      Shell / Sidebar / List /    │  (ureq,  │                │
│                Detail / Agent pane, over   │   bg exec)│               │
│                Arc<dyn GitHub>             │           │               │
│  e1-ui         tokens, settings, view models│         └────────────────┘
│  e1-github     GitHub trait, REST, Scripted│
│  e1-updater-macos  standalone Sparkle FFI  │
│  installed CLI child ◄── JSON turns/session│
└────────────────────────────────────────────┘
```

In Ginka (§4.3), the same `e1-views` sits inside Ginka's window and the `Arc<dyn GitHub>` it is given proxies through Ginka's daemon, so Ginka's rule that the daemon owns all state holds without the views knowing.

### 4.2 Crate layout

```
e1/
├─ Cargo.toml              # Ginka workspace member; the `e1` binary, deliberately thin
├─ src/main.rs             # paths, settings, locale, lifecycle, the window, Shell
├─ crates/
│  ├─ e1-github/           # model, GitHub trait, REST client, token discovery,
│  │                       # Scripted fake. No GPUI.
│  ├─ e1-updater-macos/    # standalone-only safe API over contained Sparkle FFI
│  ├─ e1-ui/               # Tokens + theme apply, Assets, Layout, AppSettings,
│  │                       # Paths, i18n, logging, nav and row view models, Fetch
│  └─ e1-views/            # Store, Shell, Sidebar, ItemList, Detail, AgentPane.
│                          # A library with no tests (AGENTS.md rule 6).
├─ locales/app.yml
├─ assets/themes/{dark,light}.json
├─ assets/icons/*.svg
└─ docs/{roadmap,ui}.md
```

Dependency direction: `e1-views → e1-ui → e1-github`. `e1-github` knows nothing about the UI; `e1-ui` knows the model but not the views; `e1-views` is the only crate that touches `gpui-component`'s render chains. The root binary alone depends on `e1-updater-macos`; none of the embeddable path does.

### 4.3 The embedding contract

Ginka already includes `e1-views` in its workspace and mounts its Inbox surface. Further integration of the GitHub panel into the right panel and sidebar remains product work. Five constraints keep the standalone and embedded entry points compatible:

| # | Constraint | Why it is decided now |
| --- | --- | --- |
| E1 | **Views are a library crate.** `src/main.rs` owns standalone lifecycle integration and opens the window, but draws nothing. | A view in a binary cannot be linked, while an embedded view must not inherit the standalone updater. |
| E2 | **`Arc<dyn GitHub>` is the only way a view reaches the network.** The trait is blocking, `Send + Sync`, and called on the background executor. | Ginka's daemon owns state; its implementation will answer over its RPC. A blocking trait can be implemented over a WebSocket channel with `block_on`; an async trait would fix the executor. |
| E3 | **No second reactor.** `ureq` over rustls, no tokio anywhere in the graph. | Ginka runs on `smol` and refuses another runtime in its process. |
| E4 | **`gpui-component` and `gpui` at Ginka's locked revs**, and no other GPUI library. | Two revs of `gpui` are two unrelated `App`, `Window`, `Element` types. Both binaries share the root `Cargo.lock`; a toolkit bump changes both together. |
| E5 | **The token schema is Ginka's.** `e1_ui::Tokens` deserialises the same JSON; when embedded, the host's tokens are installed instead of ours. | A view written against `text.secondary` renders correctly under either app's theme. Extracting a shared `glass-tokens` crate is the M4 step that makes this a type rather than a convention. |

What is *not* held constant: the sidebar's own header, window controls and settings persistence, which are the standalone shell's and will be the host's when embedded. They live in `Shell`, and `Shell` is the one view Ginka will not mount.

### 4.4 Data model

Everything a view draws is one of these, in `e1_github::model`:

| Type | What it is | Where it comes from |
| --- | --- | --- |
| `RepoId` | `owner/name`, the key everything else hangs off | parsed from `full_name`, `repository_url`, or a subject URL |
| `Repo` | a repository the viewer can reach: description, private, default branch, stars, last push | `GET /user/repos?sort=pushed` |
| `Viewer` | who the token is | `GET /user` |
| `Notification` | one inbox row: subject title and kind, the reason, unread, the repo, the item number when there is one | `GET /notifications` |
| `Item` | a pull request *or* an issue as a list row and a detail header: number, title, `Kind` (issue, or pull with draft/merged), open/closed, author, timestamps, labels, comment count, body | `/pulls`, `/issues`, `/search/issues`, `/issues/{n}` |
| `Pull` | an `Item` plus what only a pull has: base and head, additions, deletions, changed files, mergeability | `GET /repos/{r}/pulls/{n}` |
| `Comment` | one timeline entry: author, time, markdown body | `GET /repos/{r}/issues/{n}/comments` |
| `Checks` | every check run and commit status on a commit, with a tally and an overall state; an Actions job's run carries its id so its log can be read | `GET /repos/{r}/commits/{sha}/check-runs` and `…/status` |
| `ReviewComment` | a comment on a line of a pull's diff: path, line and side, or no line when the code has since changed | `GET`/`POST /repos/{r}/pulls/{n}/comments` |
| a job's log | plain text, following GitHub's redirect | `GET /repos/{r}/actions/jobs/{id}/logs` |
| `PullFile` | one file of a pull's diff: path, status, counts, the unified patch when GitHub sends one | `GET /repos/{r}/pulls/{n}/files` |
| `Tree` | every path in a repository at its default branch, and whether GitHub cut the list short | `GET /repos/{r}/git/trees/HEAD?recursive=1` |
| `FileContent` | one file: size, the text when it is text and under GitHub's inline limit, the web URL otherwise | `GET /repos/{r}/contents/{path}` |
| `Project` / `ProjectBoard` / `ProjectPage` | a Projects v2 board, its saved views, fields and items: every personal or organisation board visible to the viewer, with table, Kanban and roadmap layouts rendered natively; large boards arrive one page at a time | GraphQL `viewer.projectsV2`, organisations, `ProjectV2.views`, fields and items |

One `Item` type for both pulls and issues, rather than two, because every list and every detail header draws them the same way and only the state glyph differs; `Kind` is where that difference lives. A merged pull is `closed` on the wire with `merged_at` set — the wire never says "merged" — so `Item::state()` is where that rule is written and tested.

### 4.5 The `GitHub` trait

```rust
pub trait GitHub: Send + Sync {
    fn viewer(&self) -> Result<Viewer>;
    fn notifications(&self) -> Result<Vec<Notification>>;
    fn repositories(&self) -> Result<Vec<Repo>>;
    fn items(&self, repo: &RepoId, kind: ListKind, status: StatusFilter) -> Result<Vec<Item>>;
    fn search(&self, query: &str) -> Result<Vec<Item>>;
    fn item(&self, repo: &RepoId, number: u64) -> Result<Item>;
    fn pull(&self, repo: &RepoId, number: u64) -> Result<Pull>;
    fn comments(&self, repo: &RepoId, number: u64) -> Result<Vec<Comment>>;
    fn pull_files(&self, repo: &RepoId, number: u64) -> Result<Vec<PullFile>>; // defaults to Unsupported
    fn tree(&self, repo: &RepoId) -> Result<Tree>;                               // defaults to Unsupported
    fn file(&self, repo: &RepoId, path: &str) -> Result<FileContent>;            // defaults to Unsupported
    fn avatar(&self, url: &str) -> Result<Vec<u8>>;                              // defaults to Unsupported
    fn comment_on(&self, repo: &RepoId, number: u64, body: &str) -> Result<Comment>;
    fn set_open(&self, repo: &RepoId, number: u64, open: bool) -> Result<Item>;
    fn merge(&self, repo: &RepoId, number: u64) -> Result<()>;                   // all three default to Unsupported
    fn project_page(&self, project: &Project, after: Option<&str>) -> Result<ProjectPage>;
}
```

A write goes through the same trait as a read, and after one lands the store reads the item again rather than patching what it has: the write's answer is partial (a comment, a state) and GitHub's is the whole, which the `ETag` cache makes cheap to ask for.

Small on purpose: every method is one screen's question. Write operations arrive in M2/M3 as new methods with a default `Err(Unsupported)`, so a host implementation that cannot do them yet still compiles.

Two implementations ship: `Rest` (ureq, pagination by `Link` header up to a page cap, rate-limit headers surfaced as a typed error) and `Scripted` (in-memory, with a sample data set used by tests and by `E1_DEMO=1`).

### 4.6 Signing in

Three ways in, in order of authority: the environment (`E1_GITHUB_TOKEN`, `GITHUB_TOKEN`, `GH_TOKEN`), the keychain entry this app wrote, and `gh auth token`. With none of them the window opens on a sign-in screen that runs GitHub's **device flow**: the app asks GitHub for a short code, shows it, opens `github.com/login/device` in the reader's own browser with the code on the clipboard, and polls until GitHub hands it a token. No browser is embedded and no password passes through the process. The token goes to the macOS keychain through the `security` command (`security -i` reads from stdin, so it never appears in a process listing); signing out deletes that entry and only that entry — a token from the environment or from `gh` is someone else's to remove.

The device flow needs an OAuth app with the flow enabled. e1 ships as one — `bokuweb`'s `e1` app, whose client id is in the source (`auth::DEFAULT_CLIENT_ID`), because a client id is public by design and the device flow has no secret. A fork that registers its own sets `E1_GITHUB_CLIENT_ID` at build time or at run time.

### 4.7 Caches

Two, at two levels, and neither is a database.

- **Answers with their tags** (`e1_github::HttpCache`, `~/.e1/cache/http/`). Every `2xx` the REST client sees is kept under its URL with the `ETag` GitHub sent and the `next` page if there was one; the next request for the same URL carries `If-None-Match`, and a `304` is answered from disk. GitHub does not charge a `304` against the rate limit, so a refresh of the whole window costs round trips and nothing else. The cache knows nothing about what a body means, so it is right for every endpoint at once.
- **The store's memory** (`e1_ui::snapshot`, `~/.e1/cache/store.json`). After answers land, the store writes what it knows — viewer, repositories, inbox, every list, the last forty items read, the Project index and the last five complete Project boards — and the next launch reads it before the first request goes out. A cached Project renders immediately and revalidates in the background; an incomplete pagination run is never written. The window opens on yesterday's answer and replaces it a moment later, rather than opening on *Loading…*.

Both are cleared on sign-out, because a `304` for the last account's inbox is the last account's inbox. Both are safe to delete at any time.

Avatars are a third, simpler one: GPUI draws an image from a path and this app has no HTTP client the window could hand it, so the store fetches each avatar once through the trait (at 80 px) and keeps it under `~/.e1/cache/avatars/` named by a hash of its URL. Until it is there, the initial in a tinted circle stands in.

### 4.8 UI stack

`gpui-component` at rev `5a564d4` over `gpui` at zed rev `ef07591`, exactly Ginka's lock. The reasoning is Ginka's (`docs/roadmap.md` §4.6 there) and is not repeated; the additional constraint here is E4. Used from it: `h_resizable`/`resizable_panel`, `Root`, `Icon`, `TextView::markdown`, `Tooltip`, `Input`. Built here: the frameless header strips, the state glyphs, the list rows, the detail header.

## 5. Milestones

| Milestone | Delivers | Status |
| --- | --- | --- |
| **M0 Shell** | Workspace mirroring Ginka's; tokens, assets, settings, layout persistence; frameless glass window with three resizable columns and draggable header strips; `⌘B`/`⌘⌥B`; en+ja; token discovery; `Scripted` and `E1_DEMO=1` | landed |
| **M1 Read** | Inbox; the four fixed sections (inbox, my pulls, review requests, assigned); repositories; per-repo pulls and issues, open/closed; detail with markdown body, labels, pull header, comments; open on GitHub; `⌘R` refresh; stale-while-revalidate `Fetch` | landed |
| **M2 Review** | Pull files and diffs (landed: a Files tab, every diff in one virtualized list, folded per file, unified or split, `e1_ui::diff`); comments on diff lines, read and written (landed); sign in from the window by device flow, token in the keychain, sign out (landed); the file finder and file reading (landed); search over issues and pulls (landed); the two caches (landed, §4.7); avatars (landed); checks and their Actions logs (landed); review decision; mark a notification read; polling the inbox | in progress |
| **M3 Act** | Comment, close, reopen, merge with a method (landed); approve / request changes (landed); labels, assignees and projects edited in place (landed — projects over GraphQL, which needs the `project` scope); all visible personal and organisation Projects listed from the sidebar and read natively, including their saved table, Kanban and roadmap views (landed); the checks and the merge as GitHub's card (landed); draft and ready for review (landed, GraphQL); CLI-backed session chat in a far-right pane (landed); edit title and body; `⌘K` palette over the sections, the repositories and GitHub's search (landed, `e1_ui::palette` and `e1-views/src/palette.rs`), and over every action next | in progress |
| **M4 Embed** | Extract the shared token crate (E5 as a type); `GitHubPanel` mounted in Ginka's right panel over a daemon-backed `GitHub`; Ginka's sidebar shows the sections | |
| **M5 Polish** | Light theme sign-off, keyboard traversal audit, reduce-motion, virtualized detail timeline, on-disk cache if the in-memory one proves too little | |
| **M6 Ship** | Local ad-hoc universal app/ZIP/DMG and the standalone Sparkle bridge (landed); Developer ID signing; notarized and stapled DMG; protected tag-driven GitHub Release flow; Sparkle updates through a signed appcast and R2; installation, update and Keychain smoke tests (`docs/releasing.md`) | in progress |

## 6. Quality bars

- **Performance.** A list of a thousand rows scrolls at frame rate: `uniform_list`, rows built from precomputed `ItemRow`s, no per-frame formatting.
- **Never block the UI thread.** Every trait call runs on the background executor; a stale value is shown while the fresh one loads.
- **Accessibility.** AA contrast pairs in both themes; every state has an icon as well as a colour; every control keyboard-reachable; 28 px minimum hit target.
- **i18n.** `en` and `ja` maintained together in `locales/app.yml`; a missing key is a review failure, not a runtime one.

## 7. Open questions

| # | Question | Notes |
| --- | --- | --- |
| Q1 | Offline cache on disk? | Not until the in-memory `Fetch` is shown to be too little. If it lands, SQLite, and behind the trait so the host's daemon can own it. |
| Q2 | GraphQL for the pull header? | Review decision and checks are one GraphQL query and three REST calls. Decide in M2 with the numbers. |
| Q3 | Licence | Decide before the first public commit. Nothing GPL is linked. |

## 8. Decision log

| Date | Decision | Why |
| --- | --- | --- |
| 2026-09-27 | Share sidebar section labels through `bgpui-kit` | e1 supplies its current palette to the same small primitive as Ginka, including when e1 views are embedded. The pinned Git revision keeps the existing toolkit and GPUI lock compatible. |
| 2026-09-21 | The last five complete Project boards live in the store snapshot | Projects are GraphQL-only, so the HTTP `ETag` cache cannot make a second launch instant. A complete board is shown immediately and revalidated in the background; intermediate pages are deliberately transient so quitting during pagination can never replace a good cache with a partial board. Five boards bound disk and serialization cost while covering normal recent navigation. |
| 2026-09-21 | Large Projects publish a small opening page, then each hundred-item page, instead of waiting for the complete board | GraphQL item cursors are necessarily sequential, but the UI does not need to be. The first 25 items include fields and saved views and render immediately; subsequent requests use GitHub's 100-item maximum, omit repeated metadata, append items behind a loading indicator, and fade newly arriving cards in. A per-Project generation discards late pages from a superseded refresh. |
| 2026-09-20 | Saved Project views render with their native table, board or roadmap layout | A Project's rows alone discard the way its owner organised the work. Board columns follow the saved vertical grouping field and configured option order, and dragging a card writes the field value back through GraphQL; roadmaps use date fields or iteration spans. Both long directions remain virtualized, and the web link stays available for editing or unsupported filter details. |
| 2026-09-20 | Projects is a fixed sidebar section over every personal and organisation board visible to the viewer | A user's Projects page is not just `viewer.projectsV2`; organisation boards are a material part of the answer. The right panel reads all paginated items and their `Status` value into a virtualized native list, while preserving the explicit web link in the header. |
| 2026-09-14 | An ask is a far-right chat pane backed by the CLI's resumable session | A modal can compose one hand-off but cannot hold a conversation, and opening Terminal divides the work from the GitHub context that prompted it. The first structured CLI turn carries that context, its returned session id resumes later turns, and the virtualized chat remains visible beside the item. Credentials and model access remain the installed CLI's; e1 only owns its child process and stdout. |
| 2026-09-13 | Automatic updates follow Waku's Sparkle contract: GitHub Releases are the publication gate, R2 serves an Ed25519-signed appcast and immutable ZIPs, and the updater stays in the standalone binary boundary | Sparkle owns safe replacement and relaunch, old archives enable efficient deltas, and keeping the bridge out of `e1-views` prevents an embedded GitHub surface from trying to update Ginka. The unavoidable Objective-C FFI and its scoped unsafe exception live in a dedicated macOS updater crate. The standard Sparkle UI ships before any custom sidebar presentation. |
| 2026-09-09 | The search leaves the centre strip for a ⌘K palette | The strip belongs to a repository — its tabs, its open/closed toggle — and the search went everywhere, so a global control was sitting in a repository's own furniture. It was also the strip's only child that could not shrink: a 240 px field beside chips that hold their width is the first thing a narrow centre column pushes off the edge, which is how the problem was noticed. The palette costs a magnifier in whichever strip is the leading one, and it took the sections and the repositories with it, so ⌘K is now the way to anywhere rather than a second way to search. |
| 2026-09-09 | The palette is the toolkit's `Command`, with its filter turned off | It already owns the part that is easy to get wrong: ↑↓ reaching the list while the caret stays in the field (its bindings are registered after the input's, so they win at the same node), the virtualized rows, the headings, and Escape clearing a query before it closes the dialog. Its own filter is a substring of the label, which would have dropped the *search GitHub* row the moment nothing else matched, so `filterable(false)` and the matching is `e1_ui::palette` over `nucleo` — the file finder's matcher, and testable without a window. |
| 2026-09-07 | The first release is a universal, Developer ID-signed and notarized DMG on GitHub Releases | A one-binary app does not need an installer, while signing, Hardened Runtime, notarization and a stapled ticket give a direct download the normal Gatekeeper path. The exact flow and secret boundary live in `docs/releasing.md`. App Store, Homebrew and automatic updates were deferred at this point; the 2026-09-13 decision brings Sparkle into M6. |
| 2026-09-05 | Views live in `e1-views`, a library, not in the binary | Embedding (E1). Ginka keeps views in its binary because nothing mounts them; here something will. |
| 2026-09-05 | The `GitHub` trait is blocking, run on the background executor | A host that owns state behind a socket can implement a blocking call with `block_on`; an async trait would commit both apps to one executor (E2, E3). |
| 2026-09-05 | `ureq` for HTTP, no tokio | E3. Every async client on crates.io brings tokio; Ginka runs on smol. |
| 2026-09-05 | `Cargo.lock` seeded from Ginka's | E4. The toolkit's own manifest does not pin `gpui`, so a fresh resolve would take zed's HEAD and diverge from what `gpui-component 5a564d4` was built against. |
| 2026-09-05 | Token schema copied from Ginka; no shared crate yet | E5 as a convention now, a type in M4. A shared crate before there is a second consumer is a crate with one user. |
| 2026-09-05 | One `Item` type for pulls and issues | Every list and detail header draws both the same way; the state glyph is the only difference and `Kind` carries it. |
| 2026-09-05 | Token discovered, never stored | Rule 8. `gh` already keeps it in the keyring; a second copy on disk is a second thing to leak. |
| 2026-09-05 | Sign in by device flow; the token goes to the keychain through `security`, not to a file | Superseding the line above for the token the window obtains: a client that cannot sign itself in is one that only works for people who already have `gh`. The keychain is where `gh` keeps its own, and `security -i` keeps the secret off the command line. Shelling out rather than linking Security.framework keeps the crate free of a platform dependency it would use in one place. |
| 2026-09-05 | A pull's diff is one file at a time, not all at once | Superseded the next day: see below. |
| 2026-09-06 | A pull's diffs are one virtualized list across every file, each foldable | With the rows in a `uniform_list` a hundred files cost what the screen shows, so the reason to open one at a time went away, and a review reads top to bottom. The file headers are rows of the same height as the lines, which is what lets it be one list. |
| 2026-09-06 | Answers cached by `ETag`, and the store snapshotted, rather than a local database | Both are dumb and both are enough: a `304` is free and a snapshot makes the first frame full. A database earns its schema when something needs a query across what was fetched, and nothing does yet. |
| 2026-09-06 | Merge takes two presses; close and comment take one | A merge is the one action here git cannot take back, so the button arms on the first press and says *Merge now?*; a close can be undone with the button beside it, and a comment can be deleted on the web. A modal would be the alternative, and `docs/ui.md` §6 has no modals but destructive confirmations — this is that confirmation, in place. |
| 2026-09-06 | The logo is one colour — white on dark, navy on light — drawn with `Icon` | Superseding the gradient mark of the same morning: a coloured square read as a badge rather than a mark, and one colour that answers the theme is what every other glyph in the window does. The colour is a method on `Tokens`, not a token, because no theme file should have to name the logo. |
| 2026-09-06 | The `dev` profile optimises dependencies | A debug GPUI drops frames scrolling fifty rows, and a window that stutters cannot be judged. Dependencies rarely change, so their optimisation is paid once; our crates stay at `opt-level = 1` to keep the edit loop short. |
| 2026-09-06 | The sidebar is frosted, not painted | A second coat of dark over the window composited to near-opaque and the glass was lost on the left. A white tint at 6 % over a window at 72 % is what a frosted pane looks like, and it separates the column from the centre by tone rather than by darkness. |
| 2026-09-07 | The checks stay on a merged pull, with their runs unfolded | The card was gated on the pull being open, so the moment a pull merged its logs went with it — and a merged pull with a red run is exactly when someone goes looking for the log. The runs are unfolded from the start because the Log link inside a folded row was not being found. |
| 2026-09-07 | The diff is a variable-height `list`, no longer a `uniform_list` | A comment under a line is taller than a line, and so is the box for writing one. gpui's `list` measures what it draws and stays virtualized; the rows keep their fixed heights where they had them. |
| 2026-09-07 | A split diff pairs a removal with the addition that follows it | Side by side is only worth its width when a changed line reads as one row; pairing the two runs in order is what GitHub does and what a reader expects. The leftovers of the longer run stand alone with an empty half. |
| 2026-09-07 | Every column's view sits in a `flex_1 min_h_0` slot under its strip | A view given the column's full height under a 44 px strip ran 44 px past the window, and that 44 px was where the comment button and the sidebar footer lived. The same floor that let the conversation scroll shrink lets each column's view fit. |
| 2026-09-07 | Pickers and the merge menu are popovers, not inline | Opening one inline pushed the conversation down and widened the card; a `deferred` + `anchored` layer over the page moves nothing, and a press outside closes it. Where GitHub's shape is a popover, ours is one too now. |
| 2026-09-07 | A log is grouped by the job's steps, placed by the clock | GitHub's job page folds the log into steps, and that is how a reader finds the failing one. The log text does not say which step a line is from, but the Actions API says when each step started; `e1_ui::log::assign` puts each line under the last step that had started when the line was written. The steps are one more `GitHub::job` call, cached like the log. |
| 2026-09-07 | The log is a variable-height `list`, and its lines wrap | A `uniform_list` cut long lines off at the column's edge, and a build log is mostly long lines. The same `list` the diff moved to measures each row. |
| 2026-09-07 | A run's row has two buttons, not a click | The whole row opened the log, and the *Details* link on it opened the web — and, being inside the row, both. *Log* and *Open in browser* are now two buttons that each do one thing, and the row does nothing. A log remembers what was on screen before it, so *Back* returns there. |
| 2026-09-07 | The checks card starts folded again | It was unfolded because the *Log* link inside a folded row was not being found. Now that each run has two visible buttons, there is nothing to hunt for, and a card of a dozen runs pushed the merge button off the screen. |
| 2026-09-07 | A pending check turns | The loader glyph standing still looked like a failure of a different kind. The toolkit's `Spinner` animates it; the same one now stands wherever the app is waiting on something. |
| 2026-09-07 | What is still running is polled every 20 s | A spinner that GitHub never tells us to stop spun for ever after the run finished. The detail column re-fetches the checks, or the job and its log, while any of them is pending and nothing is in flight; a timer, not a push, because GitHub offers no push to a desktop client without a webhook endpoint. |
| 2026-09-07 | A list's check marks come from one GraphQL query, not a call per row | REST has no way to ask about many pulls at once, and fifty check-run requests per list is the N+1 the question was about. GraphQL takes fifty aliased `repository { pullRequest { commits(last: 1) { statusCheckRollup } } }` fields in one round trip; the marks land a moment after the rows, from the store's cache, and the rows never wait for them. |
| 2026-09-07 | Diff lines wrap; nothing is clipped | The split view clipped anything past its half, and a unified line past the column was lost too. The rows are already variable-height, so a long line just makes its row taller. |
| 2026-09-07 | A review comment can cover a range of lines | Clicking a line comments on that line; ⇧-clicking a second line on the same file and side stretches it to the lines between, and the API's `start_line`/`start_side` carry the range. GitHub's own shape, with a modifier instead of a drag handle. |
| 2026-09-07 | The files column is a tree, with the finder behind the search box | A flat list of every path is a finder, not a file browser: it answers "where is X" and not "what is in here". The tree is folded from the file paths rather than from GitHub's directory entries, so a folder that holds nothing cannot appear, and the flat matches come back the moment anything is typed. |
| 2026-09-07 | Highlighting is the toolkit's tree-sitter, not our own | `gpui-component` already ships the grammars and Zed's highlight queries for thirty-odd languages behind its `tree-sitter-languages` feature, which its own code editor uses. Linking that is one line in `Cargo.toml`; vendoring grammars and query files here would be a second copy to keep current for no gain. `e1_ui::code` owns only the path-to-language map and the per-line slicing. |
| 2026-09-07 | The parse is kept and the styles are not | A file is parsed once when it lands, and each row asks the parse for its own line as it is drawn. Resolving every line's styles up front would build vectors nobody scrolls to, and would have to be thrown away and rebuilt when the reader switches between light and dark. |
| 2026-09-07 | `cc` is pinned back to 1.2 | The grammars' build scripts want `cc ~1.2`, and the lock carried 1.4 from `embed-resource` through `gpui`, which cannot be two versions at once. 1.2.67 satisfies both. This does not touch the `gpui`/`gpui-component` revs that rule 4 pins to Ginka's. |
| 2026-09-08 | The light theme is rebuilt around a cool near-white, nearly opaque | Its ground was a violet-tinted white at 85 %, and a light window that lets a wallpaper through takes that wallpaper's cast over every surface: everything read as purple haze. The ground is now `#F7F8FC` at 95 %, the sidebar tints with near-black rather than white, and the status colours are GitHub's light set, which are made to be read on white. |
| 2026-09-08 | Row tints and the table head are derived per theme | The accent at 22 % is a soft highlight on a dark ground and a stain on a light one. `Colors` now carries the appearance its theme declared — copied in at load, not written in the file — so the two derived fills can differ without a second set of tokens to author. |
| 2026-09-08 | A label's lightness is moved until it reads; its hue is not | Label colours are chosen against GitHub's background. Taken as written, `enhancement`'s pale cyan vanishes on our light theme and a deep blue vanishes on the dark one. The hue carries the meaning, so it is kept and the lightness is clamped per theme, which is what GitHub does. |
| 2026-09-08 | The sidebar's header row is gone | It carried the viewer's avatar and login, and the footer carries them too. One window does not need to say twice whose it is; the traffic-light strip above the column keeps the room it needs either way. |
| 2026-09-08 | The appearance control is two states, not three | *Follow the system* was a deferral to the OS rather than a palette, so choosing it landed on the appearance that was already showing and read as a control that did nothing. What it did well is kept without being a state: the choice is now `Option<Appearance>`, and `None` — nothing chosen yet — opens on the OS's answer and follows it until the first click. A settings file that still says `system` reads as `None` rather than failing to parse, which would have reset every other setting in it. |
| 2026-09-08 | The theme is installed after the frame, never during one | Switching left the window half changed: the columns that paint their own surface took the new palette and everything else — the window's own background, the sidebar, the centre — kept the old one until some later event happened to invalidate it, which read as a switch that took seconds. The cause is that `Window::refresh` does nothing while the window is drawing, and the install was running from `Shell::render`. It is deferred out of the draw now, so the refresh lands and every view redraws in the next frame. Nothing else may install a theme from inside a render. |
| 2026-09-08 | A page of a listing is a hundred rows, not fifty | A hundred is GitHub's ceiling for every endpoint the app calls, and asking for fewer only means asking again. With the walk still stopping at three pages, the three search-backed sections go from fifty rows to as many as three hundred. |
| 2026-09-08 | Search walks its pages like every other listing | It answered with an object where the listings answer with an array, so it could not go through `get_pages`, and it had been left as a single request: every other list walked to the page cap and these three stopped at one page. |
| 2026-09-08 | `/notifications` ignores `per_page` above fifty | Measured against the API: `per_page=100` comes back with fifty rows and a `Link` header that pages on. So the inbox is fifty a page whatever we ask for, and how deep it goes is `PAGE_CAP` alone. Recorded because the next person to widen a list will otherwise look for the bug in our code. |
| 2026-09-10 | A listing follows up to one hundred pages | Three pages made the safety bound visible as a product limit: notifications stopped at 150 because GitHub returns only fifty per page, while every other listing stopped at 300. The client now follows `next` until it ends, with one hundred pages only as protection from a broken or circular link; that permits 5,000 notifications or 10,000 rows elsewhere. Notifications ask for their documented fifty explicitly rather than pretending the shared hundred applies. |
| 2026-09-08 | A repository's history is a fourth tab, over the API rather than a clone | The picture that asked for it was a local git client, with worktrees and fetch and push; e1 has no clone and wants none. What that picture shows of a *history* — the commits, the rail beside them, and what each one changed — is all in `/repos/{repo}/commits` and `/commits/{sha}`, so the tab is those two calls and nothing else is implied. |
| 2026-09-08 | The rail's lanes are computed in `e1_ui::graph`, not in the view | Which lane a commit sits in is the one genuinely tricky part of drawing a history, and it is a pure function of the shas and their parents. Putting it in `e1-ui` is `AGENTS.md` rule 6: it is tested there, straight histories and merges both, and the view only draws what it is told. |
| 2026-09-08 | The rail says where a thread enters and leaves a row, not which lanes are busy | The first cut recorded, per row, the lanes with something running in them. That draws sticks: nothing in it can say *this* thread bends from that lane into this dot, so a branch left its parent with no line to show for it. A row now carries its segments — from lane, to lane, top half or bottom — and a bend is a segment whose ends differ. |
| 2026-09-08 | A thread is a painted ribbon of small quads | `paint_path` fills; it does not stroke. Two long curved outlines leave the fill rule to guess, and it guessed a blob. A dozen convex quads along the curve fill exactly, and at this size they are a curve to any eye. |
| 2026-09-08 | One colour a lane, from the theme's own palette | A single-colour rail leaves the reader counting pixels to see which line is which. The lanes cycle through the accent and the status hues — nothing a theme has not already authored — which is what every history viewer does and needs no legend. |
| 2026-09-08 | A bend is an S, vertical at both ends | The first bends were quarter-rounds: they left a lane vertically and arrived at the next sideways. That is fine where a curve ends at a row's edge and nothing follows, and wrong everywhere else — arriving sideways at a dot, or at an edge where the same thread carries straight on, leaves a hook and a corner. A cubic whose control points sit on the two lanes stands upright at both ends, so every join between a bend, a straight run and a dot is smooth. |
| 2026-09-08 | Asking an agent borrows a CLI rather than talking to a model | e1 holds no key and runs no agent. What it can do is notice that the reader already has Claude Code, Codex or Cursor installed, signed in and configured, and start one on what is on screen. Nothing here touches a credential: it finds a binary, writes a prompt and opens a terminal on it, which is where those CLIs live. Ginka is the orchestrator; this is the hand-off to it. |
| 2026-09-08 | The CLI is looked for on the login shell's `PATH`, not the window's | A window opened from Finder inherits `launchd`'s environment, and every one of these CLIs installs somewhere only the shell knows about — a version manager's shims, `~/.local/bin`, a self-contained install linked into nothing. The login shell is asked what `PATH` is, the usual bin directories are tried besides, and the per-CLI odd places (`~/.claude/local/claude`) after that. Read from waku's shape, pedro's discovery and Ginka's probe; written here. |
| 2026-09-08 | The prompt is an argument, and the session is a terminal | Claude Code, Codex and Cursor all take the first message as their one positional argument and then stay interactive, which is exactly the shape wanted: e1 starts the session on the right question in the right checkout and gets out of the way. The prompt travels inside a generated script as a quoted argument, because the three agree on the argument and none of them agree on a file. |
| 2026-09-08 | A session starts in the repository's checkout when there is one | An agent in the wrong directory is worse than one in the home directory. `ghq root` and the usual roots are joined with `owner/name` and the one with a `.git` in it wins; with no checkout the session starts wherever it starts and the prompt still carries the URL. |
| 2026-09-08 | The ask carries GitHub's own handles, not just a link | A link is a thing to open; a number is a thing to work with. The prompt lists what e1 already knows — owner, repository, issue or pull number, branch, head commit, labels; for a log the job and workflow run ids, the failed step, and the pull it was opened from — so an agent can go and read the rest with `gh` rather than being told to visit a page. |
| 2026-09-08 | Which CLI an ask goes to is remembered, and changed in the same box | Picking from a list every time is a question nobody wants asked twice. The choice is a settings field (`agent`, by the kind's stable id), the button says where the ask would go, and the list marks it — so changing it is picking a different row, which is also how it is sent. A remembered CLI that is no longer installed falls back to the first one found. |
| 2026-09-08 | A `deferred` popover dismisses on its own bounds, never on its parent's | The ask box closed on every click inside it: the dismiss was on the strip that owns it, and a deferred child is painted outside its parent's bounds, so its own clicks read as clicks outside. The handler belongs on the card, which is what the pickers and the merge menu already did. |
| 2026-09-08 | The ask is a dialog, and a selection offers it where it ends | A panel that closes when the pointer strays is the wrong shape for something a reader reads before sending, and what is sent — the excerpt, the context, which CLI — is worth reading. Dragging over text raises a chip where the pointer let go; the chip and the strip both open the same dialog. |
| 2026-09-08 | Selected text comes from the toolkit's window-wide selection | A rendered comment is the toolkit's `TextView`, and its selection belongs to the window rather than to any view, so there is nothing in our own tree to ask. `gpui_base::TextSelection::selected_text` is read on mouse-up, which is the only moment a selection is finished. That is why `gpui-base` is linked: one call, from the same repository and rev as the toolkit. |
| 2026-09-08 | The window renders the dialog layer | `Root` draws the selection layer, the view, the tooltips and the native menus — and not dialogs. `Root::render_dialog_layer` is the app's to place, and until it was placed a dialog opened, held focus and drew nothing. |
| 2026-09-08 | Anything floating at a pointer is anchored in window coordinates | The selection chip is placed where the mouse came up, and a mouse event's position is the window's. Anchored against the column that owns it, it landed a column's width to the right of the pointer every time. |
| 2026-09-08 | Everything the toolkit floats is opaque | A dialog paints `tokens.background`, which is the window's glass, and a menu paints `popover`, which was mapped to the translucent raised surface. Both showed the page through them. The mapping now uses the opaque `Colors::popover`, and the ask dialog paints it too. |
| 2026-09-08 | The CLI list is one row that folds open | Three rows of CLI take more height than the choice deserves when the answer is nearly always the same one. The dialog names where the ask will go and folds open into the rest, which is also where the default is changed. |
| 2026-09-08 | No agent CLI, no offer | The strip, the selection chip and the dialog all check first. An offer that cannot be taken is a worse thing to show than nothing at all, and the reader has no way to fix it from inside the window. |
| 2026-09-08 | The ask is typed into a composer, not a field | One line is not enough room for a question worth asking, and the toolkit's field draws a border and a focus ring that read as one crooked outline over an opaque panel. It is a textarea in the same frame the comment composer uses, three lines growing to ten, with ⌘⏎ to send as a comment has. |
| 2026-09-08 | The excerpt in the dialog is folded | It is the one thing in that panel the reader already knows, having picked it out a moment before, and it was the tallest thing in it. One line says where it came from and how many lines it is; the fold opens it. |
| 2026-09-08 | Every surface offers the ask the same way, and the strip is gone | A bar along the bottom is a second place to look for something the pick itself can offer. Picking anything — dragged text, lines of a log, lines of a file, lines of a diff — raises the same chip at the pointer, and the chip opens the same dialog. What the strip did that a pick cannot is ask about a whole issue or pull, and that is a button beside *Open on GitHub*. |
| 2026-09-08 | In a diff the ask is ⌥-click, because a plain click is taken | A click on a diff line has meant *comment here* since the review work landed, and one gesture cannot mean two things. ⌥ picks the line out for an agent and ⌥⇧ stretches the pick; everywhere else, where nothing is taken, a plain click does it. |
| 2026-09-08 | A failed start keeps the dialog open and says so | The dialog closes on success, which is the terminal's cue to appear. On a failure there is nowhere left to put the message once the strip is gone, so the dialog stays and carries it. |
| 2026-09-08 | The composer is under the diff too | Approving lived on the conversation tab only, so a review formed while reading the diff had to be carried back to another tab to be sent. The same card is under both; under the diff its plain button reads *Comment* rather than *Send*, because that is what those words become. |
| 2026-09-08 | An empty request for changes is refused here, not at GitHub | The API rejects `REQUEST_CHANGES` with no body, and a round trip to learn that is a click that appears to do nothing. The composer says what is missing and sends nothing. Approving is left alone: it says everything it needs to by itself. |
| 2026-09-08 | The history pages as it is scrolled | `GitHub::commits` takes a page number and returns that page, rather than walking three of them and stopping; the store appends and the view asks for the next when the reader comes within twenty rows of the end. A short page is the end of the history, which is the only signal GitHub gives. The ask is deferred out of the layout that noticed it: a fetch begun mid-layout notifies into the frame it is part of. |
| 2026-09-08 | Content fades in, and only when it is content | Whole rows appearing in one frame read as a flicker: nothing moved, so nothing said that anything happened. A 320 ms fade says it clearly enough to register, keyed on what the content is — this list, this item, this job — so it plays once on arrival and never while the thing is being read. A skeleton is not faded: fading in a placeholder and then the thing behind it is two flickers where there were none. |
| 2026-09-08 | `duration.fade` is a token | The two durations the palette carried are a hover and a panel; content arriving is neither, and picking one of them for it would have been picking the wrong one twice. |
| 2026-09-08 | Joining a lane does not end it | A merge whose second parent already had a lane drew the bend into that lane and suppressed the lane's own line for that row, so a thread that carried on below appeared to stop in mid-air. Only a lane opened by the bend itself has nothing above it; a lane that was already running keeps its line, and the two together are the Y a join should look like. |
| 2026-09-08 | A commit's diff is the pull's diff, not a second one | The rows were built inside `rebuild` for a pull's files; they are a method now that takes files and comments, and a commit passes none. Unified and split, the wrapping and the fold-a-file header all came along for nothing. |
| 2026-09-08 | The toolkit's highlight palette follows the theme | `Theme::highlight_theme` is what colours fenced code in a markdown comment, and it was left at the toolkit's default, so a code block kept the light palette through the dark theme. It is set with the rest of the mapping now. This only became visible when the grammars were linked. |
| 2026-09-09 | Which model an ask goes to, and how much thinking it gets, are picked in the same dialog and remembered per CLI | The CLIs already take both — Claude Code as `--model` and `--effort`, Codex as `-m` and a `model_reasoning_effort` override, Cursor as `--model` alone — and which one a question deserves changes with the question, not with the week. So the dialog offers the chosen CLI's own choices as chips above the row that sends, and the settings keep them per CLI (`agent_models`, `agent_efforts`), because a model name means nothing to the next CLI along. |
| 2026-09-09 | The model list is a suggestion, and nothing chosen is the first chip | Vendors rename models between releases, so a list compiled into a build goes stale; the names are what each CLI's `--help` gives, the value that reaches the command line is whatever string the settings carry, and one written into `~/.e1/app.json` by hand is passed through unheard-of. The first chip is the CLI's own default and adds no flag at all, which is how a reader goes back to whatever they configured for it and what every CLI e1 has no suggestions for gets. |
| 2026-09-09 | The search leaves the centre strip for a ⌘K palette | The strip belongs to a repository — its tabs, its open/closed toggle — and the search went everywhere, so a global control was sitting in a repository's own furniture. It was also the strip's only child that could not shrink: a 240 px field beside chips that hold their width is the first thing a narrow centre column pushes off the edge, which is how the problem was noticed. The palette costs a magnifier in whichever strip is the leading one, and it took the sections and the repositories with it, so ⌘K is now the way to anywhere rather than a second way to search. |
| 2026-09-09 | The palette is the toolkit's `Command`, with its filter turned off | It already owns the part that is easy to get wrong: ↑↓ reaching the list while the caret stays in the field (its bindings are registered after the input's, so they win at the same node), the virtualized rows, the headings, and Escape clearing a query before it closes the dialog. Its own filter is a substring of the label, which would have dropped the *search GitHub* row the moment nothing else matched, so `filterable(false)` and the matching is `e1_ui::palette` over `nucleo` — the file finder's matcher, and testable without a window. |
| 2026-09-06 | The editable parts borrow GitHub's shapes | A reader who knows GitHub's gear-and-filter picker and its three-band merge card is not asked to learn ours. The first attempt (chips under a row, a *Merge now?* toggle) was smaller and read as a puzzle. Where GitHub's shape is a popover, ours opens in place under the heading: the panel is narrow and a popover over it would cover what it edits. |
| 2026-09-06 | Projects go over GraphQL; everything else stays on REST | Projects (v2) have no REST surface. One `graphql` helper carries the four queries; the `ETag` cache does not apply to them, which is fine for a picker. The device-flow scope grows `project`; a `gh` token without it gets GitHub's refusal in the picker rather than a silent empty list. |
| 2026-09-06 | A divider drag is tracked at the window while it is held | An element's mouse-move listener only hears the pointer while it is the hovered one, and a drag across the centre crosses text fields and scrollbars that claim the pointer. A `canvas` registered for the drag's duration hears every move. |
| 2026-09-06 | The columns are ours, not the toolkit's resizable group | Two rounds of reading the toolkit's resize algorithm left the right divider still not dragging, and a divider is not a place to keep guessing. Three flex children with explicit widths, a grab area on each edge and the drag tracked at the root is forty lines, all of them ours to debug; it also made the slide animation and the centre's floor trivial. `flex_none` (below) was the previous attempt. |
| 2026-09-06 | Loading is a skeleton, not a word | A reader who sees the shape of a list knows a list is coming and where to look; *Loading…* says only that they are waiting. Only a first load shows one — a refresh keeps the stale value. |
| 2026-09-06 | The sized columns are `flex_none` | The toolkit gives every panel `flex_grow: 1`, so the window's spare width was split three ways and dragging the divider on one side moved the column on the other. Only the centre grows now; the toolkit's own docs call this the load-bearing override. |
| 2026-09-06 | Appearance is a three-way cycle in the sidebar footer | Dark, light, follow the system: three states need no menu, and the footer is where the person is. Following the system re-resolves on the window's appearance observer, so a desktop that turns dark at sunset takes the window with it. |
| 2026-09-06 | Repositories are grouped under their owner | The reference sidebar groups chats under a project; a person with three organisations reads their repositories the same way. Folded owners are remembered in `app.json`. |
| 2026-09-06 | Header strips keep 6 px from their column's edges | The resize handle's grab area overlaps the strips, and a press on it meant for the divider was starting a window move. Insetting the strips is what the toolkit's own handle padding assumes. |
| 2026-09-06 | The file finder fetches the whole tree in one request and matches locally | One request for twenty thousand paths and then no latency at all beats a request per keystroke. GitHub truncates very large trees and the finder says so. |
| 2026-09-06 | The OAuth client id is committed | It is public by design: it names the app and authenticates nothing, and the device flow never sees a secret. Keeping it out of the source would only mean every user registering their own app before the sign-in button worked. |
| 2026-09-05 | `pull_files` has a default `Unsupported` body on the trait | The first method added after the trait shipped, and the pattern for every later one: a host implementation that lags the trait still compiles and the view draws the refusal. |
