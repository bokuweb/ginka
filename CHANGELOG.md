# Changelog

Notable changes, newest first.

## Unreleased

## 0.2.0 — 2026-10-05

### Agents and sessions
- Fork a conversation on the same agent, as Claude Code does: a turn's
  *Fork* menu offers *Fork here on <agent>* first, and `/fork [message]`
  forks the whole conversation from the composer, the CLI, MCP or Slack,
  sending the message to the new branch only.
- A fork from the end keeps the agent's memory and gets a thread of its own
  on its first turn (Claude's `--fork-session`); before, the original and
  the fork wrote into one Claude session.
- A fork from an earlier turn starts a fresh thread with the record up to
  that turn; before, the agent still remembered what came after it.

### Packaging
- The macOS release builds install both architectures for the pinned Rust
  toolchain, sign Chromium's bundled libraries before notarizing, and print
  Apple's log when notarization is refused.

## 0.1.0 — 2026-10-04

The first release.

### Agents and sessions
- A background daemon owns agents, terminals, git and state; the window, the
  `ginka` CLI, an MCP bridge and a Slack connector all speak one protocol to
  it. One daemon per state directory; a newer build replaces an older one.
- Drivers for Claude Code and Codex, and Gemini CLI and OpenCode over the
  Agent Client Protocol; streaming answers, steering, a stored follow-up
  queue that survives stops and restarts, and permission questions.
- Several logins per provider, with each login's rate-limit headroom.
- Checkpoints at every turn, rewind, fork onto another agent, second opinion,
  edit-and-resend.
- Fan-out: one prompt in a worktree per attempt, compared side by side, the
  winner kept and merged.
- Scheduled prompts and commands on cron, with overlap-skip, history and a
  precheck that skips a firing its probe says is pointless.
- A turn refused by a usage limit holds its queue and resumes by itself once
  the window resets.
- Agents write a line of status on their workspace, shown in the sidebar.
- A merge or rebase stopped on conflicts is handed to the agent in one step.
- An Agents board: every agent across projects, by what it needs from you.
- The account picker names each login's email, organisation and plan.
- A second opinion from another model of the same provider when it is the
  only one signed in.
- Diff a branch against its base; force-push with a lease; hand a commit a
  hook refused to the agent.
- Copy a terminal's context, scrollback included, and OSC 52 clipboard
  writes from programs in it.
- `/plan` for one read-only planning turn; agent-written pull request
  titles and descriptions; a suggested login with more room; a *new* mark
  on conversations that finished out of sight.
- Review comments over a range of lines; reminders sent into an existing
  conversation.
- A pull request's checks, and the failing ones handed to the agent; editor
  autosave that never writes over a file an agent changed.
- Mute a project's notifications for a few hours or until turned back on.
- See the MCP servers Claude Code and Codex are configured with.
- Step to the next or previous conversation, close all tabs, and copy a
  conversation's id from its row.
- Rebind or free keyboard shortcuts in `keymap.json`.
- See a changed image as it was beside as it is.
- Lines an agent wrote are marked in the diff until someone edits them.
- Preview tabs: a single click in the file tree reuses one tab.
- Add and remove Claude Code's and Codex's MCP servers through their own
  CLIs.
- Check, on request, whether a newer agent CLI is out.
- `.worktreeshare` links heavy ignored directories (`node_modules`) into each
  new worktree instead of copying them.
- Usage and cost, priced from the public rate table where a vendor does not.
- An agent's questions are answered in a form above the composer: each
  question's choices with their descriptions, several picks where allowed,
  an answer of one's own, and skip — by number key or click
  (`ginka session respond --answer`, `--skip`).

### The window
- Project rail, virtualized session list and transcript, tabs, the home
  screen with first-run steps, and the window reopening where it was left.
- Review: unified and split diffs, staging by file and hunk, commits and
  amending an unpushed one, pull
  requests through `gh`, a history graph, and line comments sent back to
  the agent.
- Terminals owned by the daemon, with splits, scrollback search and file
  links; a files surface with an editor and language servers; a browser
  surface with element inspection.
- Inbox (the e1 GitHub client, `--features github`), notes, skills, quick
  commands, and settings; English and Japanese.
- Desktop notifications with sounds, and a Dock badge.

### Reach
- Reports count agents run outside Ginka, from Claude Code's and Codex's own
  logs, by model and by project.
- Skills that teach an agent to drive Ginka (`ginka skills install`), and
  `ginka terminal` for the daemon's terminals.
- The browser's address bar completes from history.
- A Linux archive with an install script.

### Robustness
- Crash reports written to the logs directory, and the daemon's settings
  read again when edited by hand.
- Pushes, commits, searches, diffs and the workspace list wait on git and
  the network without holding up every other window and command.
- Stopping the daemon stops the agents and terminals it started; a request
  that fails inside the daemon is answered with an error instead of
  dropping the connection.
- `ginka doctor` says which git, gh and agent CLIs it can run.

### Also in this repository
- `aws`, a standalone desktop client for Amazon SQS on the same toolkit:
  queues, messages, and sign-in through the AWS Console, IAM Identity
  Center or an existing profile.
