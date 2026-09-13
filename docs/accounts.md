# Accounts: several logins per provider, and the headroom on each

**Status:** design, agreed 2026-09-05; §12 landed in full on 2026-09-06 (see
the notes in §6 and §12), with Q9 still open. It is the
shape M5's "plan usage meter" (roadmap §3.3 N12) takes once a provider can
have more than one login, and it adds one requirement of its own (N17). Read
roadmap §4.4, §4.5 and §6.3 first; this document extends them and does not
repeat them.

## 1. The problem

A vendor CLI assumes one login per machine. Claude Code keeps its state under
`~/.claude`, Codex under `~/.codex`, and each reads exactly one. A person with a
work login and a personal one, or an API key beside a subscription, switches by
exporting `CLAUDE_CONFIG_DIR` or `CODEX_HOME` in a shell and remembering which
shell is which. Ginka starts every agent itself (`ginka-core::agent::spawn`),
so it is the one place that can make the choice explicit: *this chat runs on
that login*.

Two things are wanted, in this order:

1. **Choose which login a chat runs on**, from the composer and from the CLI,
   with the choice recorded on the session and on every usage event it
   produces.
2. **See how much of each login's rate-limit window is left** before choosing,
   which is N12 asked per login rather than per machine.

One thing is explicitly *not* wanted, and §7 records why: Ginka does not pick
the login for the user by watching the gauges. Switching is a decision a person
makes at the start of a chat, with the numbers in front of them.

### What this is for, and what it is not

The mechanism is the same one people already use by hand: a second directory
and an environment variable. It is for the ordinary cases — a work login and a
personal one, an organisation's API key beside a subscription, a colleague's
machine set up for two people. Both vendors' consumer terms forbid pooling
several subscriptions to get around the limits on one, and nothing here makes
that easier than a shell alias does: there is no automatic fail-over, no
rotation, and a session never moves between logins on its own. That is a
product boundary as much as a design one, and it is why §7 is a decision rather
than a milestone item.

## 2. Vocabulary

- **Provider** — a vendor CLI Ginka has a driver for: `claude`, `codex`, and
  the rest of `ProviderKind`. Unchanged.
- **Account** — one login of one provider: a directory that the provider's CLI
  keeps its own state in, plus a label. An account is *not* a credential. Ginka
  never reads, stores or forwards a token; the vendor's CLI writes its login
  into the directory and reads it back, as it does today under `~/.claude` and
  `~/.codex`.
- **The default account** — every provider has one, implicitly. Its directory
  is the vendor's own default and it cannot be removed. A user with one login
  per provider has exactly the setup they have today, with the same settings
  in the same place; this document changes nothing for them.
- **Plan window** — one rate-limit window of one account: a label, a percent
  used and a reset time. `ginka-core::usage::pricing::PlanWindow` already
  exists and is reused as is.
- **Headroom** — what is left of the tightest window. `PlanUsage::tightest`
  already decides which one that is.

## 3. The account record

An account is configuration, not a database row. It lives in
`~/.ginka/settings.json` beside the per-agent settings it extends, for the same
reasons those live there: a person edits it by hand, `ginka doctor` has to read
it when the daemon will not start, and there is nothing in it the database
would index.

```jsonc
{
  "agents": {
    "claude": { "program": "/opt/homebrew/bin/claude" }   // unchanged: per provider
  },
  "accounts": {
    "claude-work": {                 // the id: a slug, immutable once created
      "provider": "claude",
      "label": "Work",               // what the chip says
      "env": { "ANTHROPIC_BASE_URL": "https://gateway.example" }   // optional
    },
    "codex-personal": {
      "provider": "codex",
      "label": "Personal"
    }
  }
}
```

Rules that fall out of this:

- **The id is immutable and global**, the way a worktree `name` is (roadmap
  §4.4). Sessions and usage events refer to it as text, not as a foreign key:
  a session that ran on an account since removed still says which one, and the
  usage page still attributes its cost.
- **The default account's id is the provider's id** — `claude`, `codex` — and
  it never appears in `accounts`. The per-agent `settings.agents.<provider>`
  block that exists today *is* the default account's configuration.
- **The binary is per provider, never per account.** One CLI, several logins.
  `program` stays on `agents.<provider>` and there is no way to put it on an
  account.
- **`env` is the same escape hatch `AgentSettings::env` already is**, applied
  after the provider's and before the session's (§4). It exists so a gateway or
  an API key can be attached to one account rather than to every session of a
  provider. It is where a user who wants an API-key account puts the key, in a
  file they own with their own permissions — Ginka does not offer to hold it
  anywhere else, and the wire type never carries the values (§8).
- **The directory is `~/.ginka/accounts/<id>/`**, a daemon-host path (roadmap
  §4.1). It is created empty, mode `0700`, when the account is added, and the
  vendor fills it. Removing the account removes the entry; the directory is
  removed only when asked, because it holds the vendor's login and a login is
  the thing a person least wants deleted by accident.

## 4. How an account reaches the agent

A driver knows which variable its CLI reads its home from (rule 6: vendor
knowledge stops at the driver):

```rust
trait AgentDriver {
    // …
    /// The environment variable this provider's CLI reads its state directory
    /// from, when it has one. `None` means the provider cannot have a second
    /// account, and `account add` refuses with that reason.
    fn home_variable(&self) -> Option<&'static str>;
}
```

`claude` answers `CLAUDE_CONFIG_DIR`, `codex` answers `CODEX_HOME`. Nothing
above the driver spells either name.

`spawn` already layers the environment (`ginka-core::agent`): sanitize the
daemon's inherited session state, then apply the provider's settings, then the
session's own. Accounts add one layer between the last two:

```
sanitize
  → agents.<provider>.env           (the provider's settings, as today)
  → { home_variable: home_dir } + accounts.<id>.env   (the account)
  → the session's env
```

The account comes *after* the provider's settings so that a `CODEX_HOME`
somebody typed into `agents.codex.env` by hand cannot defeat the account the
chat was aimed at, and *before* the session's so that a fan-out arm or a test
can still override anything. The default account contributes nothing to this
layer, which is what "nothing changes for one login" means mechanically.

`probe` runs under the same layering, so *installed / signed in / version* is
asked per account, and the composer's agent chip — which already says when the
agent is signed out — says it for the account that is chosen.

### Signing in

Ginka never performs a login. `ginka account login <id>` (and the same action in
the window) opens a terminal owned by the daemon, with the account's
environment applied, running the provider's own login command. The browser
round-trip, the device code, the token file: all the vendor's, written into the
account's directory the way they are written into `~/.claude` today. When the
shell exits the daemon re-probes the account and pushes the result.

The daemon already owns PTYs and the window already has a terminal dock
(roadmap M3), so this is one request and no new surface.

## 5. Sessions run on an account

`SessionOptions` gains `account: AccountId`, next to the provider, model, effort
and tier it already carries. A session records the id it started on, and every
`usage_events` row it writes carries it too (§9).

**Changing the account of a running session restarts it**, exactly as changing
the provider does (roadmap §3.3 N2): the vendor's thread lives in the account's
directory, and `resume` cannot cross directories. `apply_session_options`
answers `OptionOutcome::Restart`, the Ginka session and its transcript
continue, and the next turn starts a new vendor thread with no context of the
old one. The composer says so before it happens, the way it does for a provider
change. The hand-off — seeding the new thread with a digest of the old — is
`ginka session fork --account` (and `--agent`), which copies the record and
sends the digest in front of the new thread's first prompt
(`ginka-core::handoff`, roadmap §4.4). It is a fork rather than a switch of
the running session, so the original stays where it was.

Starting a chat is where the choice is normally made, and where it costs
nothing.

## 6. Headroom: what each provider can tell us

`PlanUsage` is a gauge, not an event: what matters is the latest reading and
when it was taken. The daemon keeps one `PlanSnapshot` per account (§9) and
pushes a change to every client. A reading is never invented; a window whose
reset time the vendor did not give shows none (`PlanWindow::reset_label` already
answers with silence), and every reading is shown with its age, because a gauge
read two hours ago is a different fact from one read a minute ago.

Vendors differ in what reaches a headless client, and this was read from the
binaries on hand (Codex 0.142.5, Claude Code 1.0.124) rather than assumed. The
driver is where the difference stops (rule 6): both feed the same
`AgentEvent::PlanUsage { usage: PlanUsage }`.

### Codex

The older `exec --json` stream (the `msg` envelope) emits a `token_count`
event, and beside the token counts that event carries the account's rate
limits: a `primary` and a `secondary` window, each with `used_percent`,
`window_minutes` and `resets_at`. The driver reads them when they are there,
and that reading is free. The labels are derived from the window's length
(`5h`, `week`) rather than from the names `primary` and `secondary`, which
say nothing to a reader.

**Corrected 2026-09-06, from running 0.142.5:** the modern stream — the
`thread.*` / `turn.*` vocabulary, which is what a current CLI emits — carries
no rate limits at all. The way to a reading on a current Codex is therefore
the one the design had as the second step: `codex app-server` answers
`account/rateLimits/read` over stdio once it has been sent `initialize` and
`initialized`, and the daemon asks it the way `probe` asks for a version — a
short-lived process under the account's environment, stopped as soon as the
answer line arrives, because a server told to read one thing does not exit on
its own. The answer names the plan (`planType`) and the windows in camelCase
(`usedPercent`, `windowDurationMins`, `resetsAt`); the driver reads both
spellings. The exchange and the answer are pinned as fixtures in the driver's
tests (R6). This is the on-demand refresh: run when the user asks, and when
the account picker opens on an account whose reading is missing or older than
the shortest window.

### Claude Code

The stream-json output carries no percentage. The CLI learns its own position
from response headers (`anthropic-ratelimit-unified-status`, `-reset`, and
which window is the binding one: five-hour, weekly, weekly Opus) and shows it
only in its interactive interface. Headless, the one thing that reaches Ginka
is a turn that was refused: an error naming the limit and the time it lifts.

Two levels, and only the first is in this design:

1. **What the CLI itself shows in this version.** A refused turn becomes a
   window at 100 % with the reset the error named; a warning, where one is
   emitted, becomes a window marked *near its ceiling* with no percentage
   claimed. The gauge is honest about its shape: it says "at the wall, opens at
   14:30", not "83 %". A driver that finds a richer event in a newer CLI's
   stream reads it — the parser tolerates unknown events already — and that is
   how the gauge improves without a decision here.

   *Landed 2026-09-06:* the refusal's text is read from the assistant's
   message and from the result. The older wording, `Claude AI usage limit
   reached|<unix seconds>`, gives a window called `limit` with its reset; the
   newer sentences name the window — `5-hour`, `Weekly`, `Opus weekly` — and
   give the reset in prose, which is not turned into a number. One wall said
   twice in a turn is one reading. No warning reaches the headless stream in
   1.0.124, so nothing is claimed short of the wall.
2. **A percentage.** Newer Claude Code has a `/usage` screen that reads an
   Anthropic endpoint with the login's OAuth token. Ginka could call the same
   endpoint under the account's environment. Doing so means reading a
   credential the vendor's CLI stored — the keychain on macOS, a file elsewhere
   — and depending on an endpoint the vendor has not documented. That contradicts
   §3 (Ginka holds no credential) and roadmap R6 (formats change without
   notice) at once, so it is not built on this document's say-so. It is roadmap
   **Q9**, and stays open until decided.

### Refresh policy

Passive by default: the gauge moves when a turn moves it. Active refresh happens
on two occasions only — the user asks for it, and a chat is about to start on an
account whose reading is older than the tightest window is long. There is no
poll loop: a quota is cheap to observe from the traffic that spends it, and a
daemon that phones a vendor on a timer for a number nobody is looking at is not
local-first (rule 7).

## 7. Switching is manual

Ginka does not route. The question was asked — "can it pick the login with the
most left?" — and the answer is no, for three reasons that are each sufficient:

- **The signal is not there.** On Claude the gauge is a status, not a number,
  until Q9 is decided; on Codex it is a number only for the account that ran
  the last turn. A router that guesses from a stale gauge is worse than a
  person reading the same gauge, because the person knows it is stale.
- **A session cannot move.** The vendor's thread lives in the account's
  directory (§5). Automatic switching would restart the agent mid-conversation
  with no memory of it, at the moment the user least expects it.
- **It would be a limit-evasion tool.** §1 sets that boundary. A choice that a
  person makes, with the gauges in view, is the setup they have today made
  legible; a daemon that rotates logins is a different product.

What Ginka does instead: puts the gauge next to the choice. A separate composer
usage chip shows the tightest window even when there is only one login; the
account picker shows the same for every account of the provider; the sidebar
footer, which the UI spec already reserves for a plan label, shows the account
in use and its headroom. The usage chip follows pushed readings after turns and
refreshes explicitly when clicked, never on a timer. A person
who sees "5h · 92 % · resets in 40m" beside "Work" and "5h · 12 %" beside
"Personal" needs no router.

## 8. The protocol

Rule 3: the CLI and the MCP server get every one of these the day the window
does.

New in `ginka-protocol`:

```rust
/// One login of one provider. See `docs/accounts.md`.
pub struct Account {
    pub id: AccountId,
    pub provider: ProviderKind,
    pub label: String,
    /// The directory the provider's CLI keeps this login in. A daemon-host path.
    pub home: PathBuf,
    /// The provider's own default, which cannot be removed.
    pub is_default: bool,
    /// The *names* of the variables `env` sets. Values never cross the wire.
    pub env_keys: Vec<String>,
    /// What the last probe said, when there has been one.
    pub signed_in: Option<bool>,
}

/// The latest reading of an account's rate-limit windows.
pub struct PlanSnapshot {
    pub account: AccountId,
    pub usage: PlanUsage,
    /// Unix seconds. Shown beside the reading; a gauge without an age is a claim.
    pub observed_at: i64,
    pub source: PlanSource,   // Reported (from a turn) | Fetched (on demand)
}
```

Requests: `Accounts`, `AddAccount { id, provider, label }`,
`RemoveAccount { id, delete_home: bool }`, `LoginAccount { id }` (answers with
the terminal it opened), `RefreshPlanUsage { account }`. `StartSession` gains
`account: Option<AccountId>`, `None` meaning the provider's default. `Usage`
gains a grouping — by day, by agent, by account — where it has two today.

Pushes: `DaemonEvent::PlanUsageChanged { snapshot }` and
`DaemonEvent::AccountsChanged`. Events: `AgentEvent::PlanUsage { usage }` from
the drivers.

CLI:

```
ginka account list                       # id, provider, label, signed in, headroom, age
ginka account add --provider codex work  # creates ~/.ginka/accounts/work, 0700
ginka account login work                 # the vendor's login, in a daemon terminal
ginka account remove work [--delete-home]
ginka account refresh work               # on-demand headroom, where the provider has it
ginka chat --account work "…"            # unchanged otherwise
ginka usage --by account
```

## 9. Data

Migration `0007_accounts`:

- `sessions.account_id TEXT NOT NULL`, backfilled with the session's provider
  id — every existing session ran on what is now the default account, and the
  backfill says so rather than leaving the column nullable.
- `usage_events.account_id TEXT NOT NULL`, backfilled the same way, with an
  index `(account_id, at)` for the per-account view.
- `plan_snapshots (account_id TEXT PRIMARY KEY, plan TEXT, windows TEXT NOT NULL,
  observed_at INTEGER NOT NULL, source TEXT NOT NULL)`. Latest reading only;
  `windows` is the `Vec<PlanWindow>` as JSON, because the set and their labels
  are the vendor's and a column per window would be a migration per vendor.

The account record itself is in `settings.json` (§3), so there is no `accounts`
table and no foreign key. `ginka-core::usage` gains `by_account`, and the
retention sweep is unchanged: a snapshot is one row per account and is never
swept.

## 10. Security

Extends roadmap §6.3:

- Ginka holds no vendor credential and reads none. An account is a directory
  the vendor writes into; sign-in is the vendor's command in a terminal.
- `~/.ginka/accounts/<id>/` is created `0700`. `settings.json` is the user's
  file at the user's permissions, as today; `accounts.<id>.env` is documented
  as the place a key goes *if* the user wants one there, with the same warning
  the per-agent `env` carries.
- `Account` crosses the wire with the names of its `env` keys and never the
  values. Logs redact the account layer of the environment the way they
  redact the provider's (`is_inherited_session_state` already names the
  variables that matter).
- `doctor` reports accounts by asking the vendor's CLI (`probe` under the
  account's environment), never by reading the directory's files.

## 11. In the window

Additions to `docs/ui.md`, recorded there too:

- **Composer** — an account chip after the agent chip, present only when the
  chosen provider has more than one account. It carries the label and, when a
  reading exists, the tightest window as *percent · reset*. Its menu lists the
  provider's accounts with the same two facts and an *Add account…* entry.
- **Sidebar footer** — the plan label the spec reserves becomes the account in
  use and its headroom.
- **Reports surface** — a per-account section: every window with its
  percentage, reset and age, over the account's token and cost totals.
- Nothing encodes headroom in colour alone (roadmap §6.4): the percentage is
  printed, and *at the wall* is a word beside the number, not a red number.

## 12. Order of work

Each step is usable on its own and the earlier ones do not depend on Q9.
Steps 1–3 landed 2026-09-06: the account chip and picker, the sidebar footer,
the Reports surface (windows per login over the totals by login, agent and
day), and an *Add a login…* row in the agent and account pickers that opens a
small dialog — in the agent picker as well, because the account chip only
exists once there is a choice, and the first second login has to be reachable
from somewhere. Step 4 waits on Q9.

1. **Accounts.** The settings block, `home_variable`, the spawn layer, `probe`
   per account, `AccountId` on sessions and usage events, the migration, the
   requests, the CLI, the composer chip with no gauge on it yet.
2. **Codex headroom from the turn.** `AgentEvent::PlanUsage` from
   `token_count.rate_limits`, the snapshot table and push, the gauge on the chip
   and in Reports.
3. **Claude headroom as the CLI reports it.** The refused-turn and warning
   paths into the same event. On-demand refresh for Codex through
   `app-server`.
4. **Q9**, if decided in favour: a percentage for Claude, behind an explicit
   opt-in.

Tests come first in each (AGENTS.md conventions), and the cases with a decision
in them are the ones to write: an account added for a provider with no
`home_variable` is refused; the account layer beats a `CODEX_HOME` typed into
the provider's settings and loses to the session's; a session whose account
changes gets `Restart`; a recorded `token_count` with `rate_limits` yields two
labelled windows and one without yields nothing; a refused Claude turn yields
a window at 100 % with the named reset; `by_account` totals sessions on removed
accounts; the migration backfills every existing row with its provider id.
