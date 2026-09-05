# Chat connectors: Slack first

**Status: design.** Nothing in this document is implemented. It exists so that the interfaces — the settings shape, the session origin, the transport trait, the outbound contract — are decided before the first line of adapter code, in the same spirit as roadmap §3.3.

A *connector* lets a message in a chat platform start, or continue, an agent turn on the user's own machine, and carries the agent's answer back to the thread it came from. Slack is the first platform and the only one this document specifies. The layering is written so that a second platform is a second adapter behind one trait, not a second design.

## 1. Why this exists, and why it is built the way it is

The wish is simple: "a message in `#ginka-bugs` runs Claude Code or Codex in this repository and posts the result back". Three things that look like they already answer it do not.

- **A plain MCP server cannot.** MCP is pull-shaped: the agent calls tools while it is running. Nobody is running when the Slack message arrives, and nothing in the base protocol lets a server start a turn. The `ginka mcp` server is the right way for a *running* agent to read a thread; it is not a trigger.
- **Claude Code channels are Claude-only and single-session.** Channels are Anthropic's extension of MCP that pushes events into an already-open `claude` session. They need an interactive or `-p` session kept alive in a terminal, they take one context for every message that arrives, Slack is not on the preview's plugin allowlist, and Codex has no equivalent. Their *patterns* — sender allowlists, permission relay with short request ids, "gate on the sender, not the room" — are good and are borrowed below.
- **Claude in Slack / Claude Tag runs in the cloud.** An `@Claude` mention spawns a Claude Code on the web session against a GitHub clone. It never touches the user's worktrees, the daemon, or Codex.

What does answer it is the shape Hermes Agent uses for its messaging gateway: **a long-lived process that owns the platform connection, keys sessions by where the conversation happens, gates on the sender, and dispatches into the agent it already knows how to run.** Hermes runs that as a separate `hermes gateway` service. Ginka already has a long-lived process that owns sessions, agent processes and the event stream — the daemon — so the gateway role goes there, as an optional module, and the platform-specific part is an adapter.

Hermes is MIT-licensed and was read for behaviour, not code (roadmap R9). What is taken from the reading is listed in §11 so the debt is visible.

## 2. Where it lives

```
   Slack workspace
        │  Socket Mode: the daemon dials out; nothing listens on the network
        ▼
┌──────────────────────────────────────────────────────────────────────┐
│ ginka-daemon                                                         │
│                                                                      │
│  connectors::slack        (adapter: Socket Mode client + Web API)    │
│        │ Inbound                             ▲ Outbound              │
│        ▼                                     │                       │
│  ginka-core::connector    (pure: decisions, folds, formatting)       │
│        │ Request                             ▲ DaemonEvent           │
│        ▼                                     │                       │
│  the same request handler the CLI, the app and `ginka mcp` use       │
│        │                                                             │
│        ▼                                                             │
│  drivers (claude, codex)  ──►  AgentEvent stream  ──►  SQLite         │
└──────────────────────────────────────────────────────────────────────┘
```

**The connector is a fourth client of the protocol, hosted inside the daemon.** That sentence carries three rules:

1. **It goes through `Request`.** Starting a session, sending a follow-up, cancelling, uploading an attachment, answering a question: each is the existing request. The connector never reaches into the session table or a driver directly. This is architectural rule 3 (the CLI, the UI and the MCP server share one protocol) applied to a client that happens to live in-process. Anything the connector can do, `ginka` can do from a shell.
2. **It is off unless configured.** No token, no module: the daemon starts exactly as it does today. Rule 7 (local-first) says no feature may *require* a remote service, and this one does not; it is a door the user opens.
3. **Slack stops at the adapter.** Envelope parsing, `chat.postMessage`, rate limits, `mrkdwn`: all of it lives in `ginka-daemon::connectors::slack`. `ginka-core::connector` sees `Inbound` and produces `Outbound`, and is tested against a scripted transport the way sessions are tested against `ScriptedSession`.

Why the daemon and not a separate binary: the thread-to-session map and the delivery ledger are state, and rule 1 says the daemon owns state; the event stream is in-process there; and the daemon already has a lifecycle, a log and a `doctor`. A separate `ginka-slack` binary would need its own supervision and a handful of new requests just to write its state back. Hermes pays that cost because its agent core has no daemon; we already have one. §4.1 of the roadmap lists "watchers, pollers, cron, MCP" as daemon residents, and a connector is the same kind of thing.

Why not the app: the app must be killable at any moment without losing anything (rule 1), and a Slack bot that stops answering when the window closes is the failure mode this project exists to avoid.

## 3. Layers and their interfaces

### 3.1 `ginka-core::connector` — the decisions

Pure code with no network. Three parts.

**Inbound.** A platform-neutral message and the policy that turns it into a request.

```rust
/// A message a transport received, already reduced to what the policy needs.
pub struct Inbound {
    pub connector: ConnectorId,          // "slack"
    pub channel: ChannelId,              // Slack conversation id, C…/G…
    pub thread: ThreadKey,               // the root message's ts, or this message's ts
    pub message: MessageId,              // this message's ts
    pub sender: SenderId,                // Slack member id, U…
    pub text: String,                    // mention stripped, entities unescaped
    pub mentions_bot: bool,
    pub files: Vec<InboundFile>,         // already fetched by the adapter
    pub is_edit: bool,
    pub from_bot: bool,
}

/// What the connector does with it.
pub enum Decision {
    Ignore(IgnoreReason),                // logged at debug, never answered
    Start { binding: BindingId, prompt: Prompt },
    Continue { session: SessionId, prompt: Prompt },
    Control { session: Option<SessionId>, command: Control },
    Verdict { session: SessionId, request_id: String, answer: Answer },
}
```

`decide(&Inbound, &Config, &dyn OriginIndex) -> Decision` is the whole policy, and every rule in §5 is a test against it.

**Outbound.** A fold over the session's `AgentEvent`s that yields what the thread should see, throttled and chunked, but not yet formatted for a platform.

```rust
pub enum Outbound {
    React { message: MessageId, glyph: Glyph, replace: Option<Glyph> },
    Progress { text: String },           // one edited-in-place status line
    ClearProgress,
    Reply { blocks: Vec<ReplyBlock> },   // the turn's answer, chunked
    Question { request_id: String, kind: QuestionKind, text: String, options: Vec<String> },
    Footer { changed_files: usize, checkpoint: Option<CheckpointId> },
}
```

`fold(&mut TurnState, &AgentEvent) -> Vec<Outbound>` owns the throttle for progress edits, the silence token, and the chunk boundaries. It is deterministic given a clock.

**Transport trait.** What an adapter must provide, kept small enough that a scripted one fits in a test.

```rust
pub trait ChatTransport {
    fn post(&mut self, channel: &ChannelId, thread: &ThreadKey, text: &str) -> Result<MessageId>;
    fn edit(&mut self, message: &MessageId, text: &str) -> Result<()>;
    fn delete(&mut self, message: &MessageId) -> Result<()>;
    fn react(&mut self, message: &MessageId, glyph: Glyph) -> Result<()>;
    fn unreact(&mut self, message: &MessageId, glyph: Glyph) -> Result<()>;
    fn upload(&mut self, channel: &ChannelId, thread: &ThreadKey, name: &str, bytes: &[u8]) -> Result<()>;
    fn typing(&mut self, channel: &ChannelId, thread: &ThreadKey) -> Result<()>;
}
```

`connector::testing::ScriptedTransport` records calls and answers with scripted ids, and is the only transport tests ever see.

### 3.2 `ginka-daemon::connectors::slack` — the adapter

- **Socket Mode.** The daemon opens `apps.connections.open` with the app-level token, receives a WebSocket URL, and connects with `async-tungstenite`, which the daemon already links. Every envelope is acknowledged within Slack's three-second window *before* it is processed; processing is queued. `disconnect` envelopes and dropped sockets reconnect with exponential backoff, and the state is published (§3.3).
- **Web API.** `chat.postMessage`, `chat.update`, `chat.delete`, `reactions.add` / `reactions.remove`, `files.getUploadURLExternal` + `files.completeUploadExternal`, `conversations.replies` (for thread context), `users.info` (for the sender's display name, cached). Rate limits are honoured from `Retry-After`; the adapter never retries a `chat.postMessage` it did not see fail, because a duplicate reply is worse than a missing one (the ledger in §7 handles the missing case).
- **Text.** Slack `mrkdwn` is not Markdown. Outbound converts the agent's Markdown: headings to bold lines, `**` to `*`, links to `<url|label>`, fenced code kept verbatim inside triple backticks, tables to a code block. Inbound unescapes `<@U…>` to `@name`, `<#C…|name>` to `#name`, `<url|label>` to `label (url)`, and `&amp;`/`&lt;`/`&gt;`.
- **Files.** Inbound files are fetched with the bot token and handed to `Request::UploadAttachment`; the prompt references them by the `ginka-attachment:` URI the daemon answers with (N6). Nothing is inlined.

### 3.3 Protocol additions

Small, and all of them things the CLI and the app need anyway.

- `Session.origin: Option<SessionOrigin>` with `{ connector, channel, thread }`. It is what the thread-to-session lookup indexes, what the sidebar draws as an origin chip, and what tells the outbound fold that this turn's answer has somewhere to go.
- `Request::StartSession` and `FanOut` gain `origin: Option<SessionOrigin>`. Nothing else about starting changes.
- `Request::ListConnectors` → `Response::Connectors { states }`, one `ConnectorState { id, enabled, connected, since, last_error, bindings }` each.
- `DaemonEvent::ConnectorStateChanged { state }`, so a window shows the dot going red without polling.
- `ginka slack status`, `ginka slack bindings`, `ginka slack allow <member-id>`, `ginka slack test <channel>` (posts one message and deletes it). Bindings are edited in the settings file; the CLI reads and validates, it does not own a second store.

## 4. Configuration

### 4.1 Secrets

Two tokens: the bot token (`xoxb-…`) for the Web API and the app-level token (`xapp-…`) for Socket Mode. They live in **`~/.ginka/connectors/slack.env`** with mode `0600`, or in `GINKA_SLACK_BOT_TOKEN` / `GINKA_SLACK_APP_TOKEN`, environment winning. They are never written to `settings.json`, never to SQLite, and never to a log line, including at trace level (§6.3). `ginka doctor` reports whether the file exists and its mode, not its contents.

### 4.2 Settings

Bindings are configuration, not state, so they sit in the daemon's `settings.json` beside the agent overrides:

```json
{
  "connectors": {
    "slack": {
      "enabled": true,
      "allowed_users": ["U01ABC2DEF3"],
      "approvers": ["U01ABC2DEF3"],
      "bindings": [
        {
          "channel": "C0123456789",
          "project": "ginka",
          "agent": "claude",
          "model": null,
          "access_mode": "ask",
          "trigger": "mention",
          "worktree": "shared",
          "max_concurrent": 2,
          "progress": "edit",
          "cleanup_progress": true
        }
      ]
    }
  }
}
```

- `allowed_users` **empty means deny everyone.** This is Hermes's default and the only safe one for a bot that runs a shell on your machine: inviting the bot to a channel must not, by itself, let the channel drive it. There is no `allow_all`.
- `approvers` is the subset that may answer questions, approve plans and grant permissions (§6.4). Defaults to `allowed_users`; the split exists because Claude Code's own channels documentation is right that "anyone who can reply can approve" is a stronger grant than "anyone who can ask".
- `channel` is the conversation id, never the name; names are renamed.
- `project` or `workspace`: a binding targets a project's own checkout by default, the same rule a chat started from the window follows (decision log 2026-09-05). `"worktree": "per_thread"` instead creates a workspace named `slack-<thread ts>` per conversation, which is what a channel used for parallel independent asks wants; it is opt-in because it leaves worktrees behind that someone has to remove.
- `access_mode` is the ceiling for the binding, and `"auto"` has to be written down here in plain text. There is no way to raise it from Slack.
- `trigger`: `mention` (default) needs `@bot` on a root message; replies inside a thread the bot already owns never need one. `all` takes every root message in the channel, for a channel that exists only for this.
- `max_concurrent` caps live turns per binding; a `shared` worktree forces `1`, because two agents editing one checkout is not concurrency, it is a merge.

`deny_unknown_fields` applies, as it does to the rest of the settings: a misspelt key fails loudly at daemon start rather than silently dropping a security setting.

### 4.3 The Slack app

A manifest ships at `assets/slack/manifest.json` so the setup is "create app from manifest, install, copy two tokens". It asks for exactly:

- Socket Mode on; an app-level token with `connections:write`.
- Bot scopes: `app_mentions:read`, `channels:history`, `groups:history`, `chat:write`, `reactions:write`, `reactions:read`, `files:read`, `files:write`, `users:read`.
- Event subscriptions: `app_mention`, `message.channels`, `message.groups`. Missing the two `message.*` subscriptions is, per Hermes's docs, the single most common setup failure: the bot sees mentions but never thread replies. `ginka slack status` says so explicitly when a thread reply is expected and none arrives within a probe window.

No `im:*` scopes. Direct messages are out of scope for the first version (§10).

## 5. Inbound: what a message becomes

Every rule here is a test against `decide`.

1. **Drop before looking.** Messages from the bot itself, any `bot_message` subtype, `message_changed`, `message_deleted`, and anything in a channel with no binding are `Ignore`d. Edits are ignored because a turn already ran on the original text and a second one would be a surprise.
2. **Gate on the sender, never the room.** `sender ∉ allowed_users` is `Ignore(NotAllowed)`. A bound channel is where the bot listens; the allowlist is who it listens to. Being in the channel is not a grant.
3. **Deduplicate.** Socket Mode redelivers when an ack is late. `(channel, message)` goes into a bounded seen-set (last 4096, persisted across restart via the ledger) and a repeat is `Ignore(Duplicate)`.
4. **Find the conversation.** The key is `(channel, thread)`, where `thread` is the root message's `thread_ts` when present and the message's own `ts` when it is a root. The origin index answers "is there a session for this key".
5. **Root message, no session:** needs `mentions_bot` unless the binding's trigger is `all`. Result is `Start` in the binding's target. The session's `title` is the first line of the text, as it is for any first prompt (N5).
6. **Reply in a thread the bot owns:** `Continue`, mention or not. The daemon applies the steer-or-queue policy (N1) exactly as it does for the composer: a message during a turn is steered on `claude`, queued on `codex`, and the thread gets a small reaction saying which.
7. **Mention inside a thread the bot has never seen:** `Start`, and the prompt carries the thread so far as quoted context — at most the last 50 replies, fetched by the adapter, each attributed by display name and marked as untrusted quoted material. This is what makes "we've been discussing this bug for an hour, @ginka fix it" work, and the bound is what keeps a 2,000-message thread from becoming the prompt.
8. **Control words.** A reply in an owned thread that is exactly one of `stop`, `status`, `new` (after stripping the mention) is a `Control`, not a prompt: `stop` is `CancelSession`, `status` posts the session state and the last tool activity, `new` closes the mapping so the next message starts a fresh session in the same thread. Anything longer is a prompt. Hermes uses slash commands for this; Slack slash commands need a public request URL, which Socket Mode was chosen to avoid.
9. **Verdicts.** `yes abcde`, `no abcde`, or `abcde: some text` where `abcde` is an open request id in this session becomes a `Verdict` — but only from an `approvers` member; from anyone else it is `Ignore(NotApprover)`, and the thread says so once.
10. **What the agent is told.** The prompt is wrapped so the agent knows where it came from: who asked, in which channel, and that the text and any quoted context came from a chat platform and are data. The wrapper is one fixed template in `ginka-core::connector::prompt` with a test on it, because it is the one place the connector puts words in the user's mouth.

## 6. Outbound: what the thread sees

The rule that keeps this simple: **an answer goes where its prompt came from.** A turn started from Slack posts to Slack; a turn started from the window does not, even in a session that has a Slack origin. Messages typed in the window are not mirrored into the thread. Someone reading the thread sees exactly the exchanges that happened in the thread.

1. **Acknowledge.** On `Start` or `Continue`, the triggering message gets 👀 within the ack window. It is the only feedback for the first few seconds and the thing that tells the sender the allowlist let them through.
2. **Progress.** One message, posted on `TurnStarted` and edited in place on every `ToolCall`, throttled to one edit per two seconds and to Slack's `chat.update` budget. It shows the current activity line (`Reading src/shell.rs`, `Running cargo test`) and nothing the tool returned. With `cleanup_progress` it is deleted when the reply lands; otherwise it collapses to one line. This is Hermes's accumulate-into-one-bubble pattern; a thread full of progress bubbles is the thing it exists to prevent.
3. **Reply.** On `TurnEnd`, the turn's assistant text as `mrkdwn`, chunked at 3,900 characters on a paragraph or fence boundary, never inside a fence. Beyond four chunks the remainder is uploaded as a `.md` snippet. If the text is exactly one of `[SILENT]`, `SILENT`, `NO_REPLY` the reply is suppressed (the transcript keeps it), which lets a prompt say "only speak up if you find something".
4. **Footer.** When the turn changed files: `3 files changed · checkpoint 7 · ginka review ginka` on one line. A link cannot open the app, so the line names the command that shows the diff.
5. **Reaction swap.** 👀 becomes ✅ on `TurnEnd`, ❌ on `Failed` or a non-zero `ProcessExited`, ❓ on `AwaitingInput`. A cancelled turn gets ⏹. Nothing encodes state in the reaction alone: each swap also comes with a one-line post when the state is not success.
6. **Questions.** `AskUser`, `PlanProposal` and `Permission` are posted as a question with a five-letter request id (lowercase, drawn without `l`, borrowed from Claude Code's relay because it survives a phone keyboard). Options are numbered. The post says who may answer and how. The verdict path is §5.9 and lands as `RespondToAgent`. The window's own prompt stays live; whichever answer arrives first wins, and the other place is told. Until the drivers surface these events (M5), this section is a contract, not code.
7. **Failure.** A driver failure posts the last stderr line and the session id, once. A connector failure (rate-limited past retry, token revoked) posts nothing to the thread — there is no reliable way to — and raises `ConnectorStateChanged` with `last_error`, which the sidebar and `ginka slack status` show.

## 7. State and lifecycle

**Persisted in SQLite** (daemon-owned, rule 1):

- `sessions.origin_connector`, `origin_channel`, `origin_thread` — nullable, with a unique index over the three. This is the thread-to-session map. It survives restarts, so a reply in a week-old thread resumes the vendor session through `vendor_session_id` like any follow-up.
- `connector_deliveries` — `id`, `connector`, `channel`, `thread`, `kind`, `payload`, `attempts`, `created_at`, `delivered_at`. Written *before* a post, marked after. On boot, undelivered rows younger than 24 hours with fewer than three attempts are sent again with a `Recovered reply, may repeat an earlier one` prefix. Hermes calls this the delivery ledger; it is the difference between a daemon restart losing a twenty-minute answer and not.
- `connector_seen` — the bounded dedup set, so a redelivery after a restart is still a duplicate.

**Not persisted:** the Socket Mode connection, progress message ids (a restart mid-turn abandons the progress bubble and the reply still lands via the ledger), the user-name cache.

**Concurrency.** One live turn per conversation, by construction: a second message meets the steer-or-queue policy. Across conversations, `max_concurrent` per binding, with a `shared` worktree pinned to one. A message that cannot start because the binding is at its cap is queued in order and reacted with ⏳; the queue is in memory and is small, because the cap is.

**Reconnect.** Backoff from one second to one minute, jittered, forever. While disconnected, nothing is lost on Slack's side beyond what its retention drops, and nothing is lost on ours because turns already running keep running and their replies wait in the ledger.

**Visibility in the window.** A Slack-originated session is a session: it appears in the sidebar under its workspace with an origin chip (`#ginka-bugs`), its transcript renders, and the composer works on it. Taking over from the window is normal; the thread simply does not hear what happens there (§6).

## 8. Security

Additions to roadmap §6.3, all of which the design above already assumes:

- **Deny by default, per sender.** No allowlist, no answers. The room is never a grant.
- **A connector cannot raise privilege.** Access mode is the binding's ceiling, `auto` is opt-in in a file the user edits by hand, and nothing said in Slack changes a setting.
- **Everything from the platform is data.** Text, quoted context, file names and the sender's display name go into the prompt inside the attribution wrapper. The agent is told where they came from; the connector never executes an instruction found in them.
- **Approval is a narrower grant than asking.** `approvers` exists for exactly this reason, and a question posted to a thread names who may answer it.
- **Secrets stay out of the database and the logs.** Tokens live in a `0600` file or the environment. Outbound text is logged at debug level with tokens redacted by construction, because the adapter never puts a token in a string it logs.
- **The network surface is outbound only.** Socket Mode means the daemon still binds loopback and nothing else. This is the reason it was chosen over the Events API.
- **Rate.** A per-sender budget of turns per hour, default 30, so a compromised member account is a nuisance rather than a bill.

## 9. Testing

The rules of `AGENTS.md` apply unchanged, and two of them decide the shape here:

- **No live Slack in tests.** `decide` and `fold` are pure and tested directly. The adapter is tested against recorded Socket Mode envelopes under `crates/ginka-core/tests/fixtures/slack/` (R6's fixture rule), and against `ScriptedTransport` for what it would have sent.
- **The awkward cases are the tests.** A message from an allowed sender in an unbound channel. A mention inside a thread that is 2,000 replies long. A verdict from a non-approver. A redelivered envelope after a restart. An edit to a message that already ran. A reply exactly equal to `stop`. A `[SILENT]` answer with a footer's worth of changed files. A `shared` binding with `max_concurrent: 3` written in the settings file (rejected at load, with the reason).

## 10. Open questions

| # | Question | Leaning |
| --- | --- | --- |
| C1 | Direct messages to the bot | Out for the first version. A DM has no channel to bind, so it would need a default binding, and Claude in Slack made the same cut. Revisit with a `"dm": { … }` binding if asked for. |
| C2 | Mirroring window-typed messages into the thread | No. The thread shows what happened in the thread; an "also continued in the app" line at most. Mirroring makes the thread a transcript viewer, and the app is the transcript viewer. |
| C3 | More than one Slack workspace | One. The settings shape is a single `slack` object, and making it a list later is an additive change. |
| C4 | Streaming the reply by editing a message as text arrives | Not initially. Progress is edited in place; the reply lands whole. Streaming into `chat.update` burns the rate budget the progress line needs, and a half-written reply invites a reply to it. |
| C5 | Which milestone | The domain layer (`ginka-core::connector`, the settings shape, the origin column) lands whenever, test-first, like N1–N14 did. The adapter is an M5 item, and the question relay (§6, item 6) waits for the drivers to surface questions. |

## 11. What was taken from reading Hermes Agent, and what was not

Read for behaviour under roadmap R9; nothing was copied. Taken:

- Socket Mode as the only Slack transport, so nothing listens.
- Sessions keyed by the chat origin, persisted, with an explicit reset (`new` here, `/reset` there).
- Deny-by-default sender allowlists with no room-level grant, and a split between people who may ask and people who may administer (`approvers` here, admin tiers there).
- One progress message edited in place, optionally cleaned up, rather than a bubble per tool call.
- The delivery ledger with bounded redelivery and an honest "may repeat" prefix.
- The silence token.
- Per-channel overrides of model and behaviour, as bindings.
- "Missing `message.*` event subscriptions is the number one setup failure", turned into a `status` diagnostic.

Not taken: the twenty other platforms, running the cron scheduler inside the gateway (Ginka's cronjobs are already a daemon feature, M5), voice, pairing by DM (there are no DMs; the allowlist is edited in a file or with `ginka slack allow`), and running the gateway as its own service (the daemon is the service).

From Claude Code channels: the five-letter request id without `l`, gating on `sender` rather than `chat`, and the observation that a reply path is an approval path and must be gated harder.
