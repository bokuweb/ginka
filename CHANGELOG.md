# Changelog

Notable changes, newest first. Ginka has not had a release; everything so
far is unreleased.

## Unreleased

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
- Scheduled prompts and commands on cron, with overlap-skip and history.
- Usage and cost, priced from the public rate table where a vendor does not.

### The window
- Project rail, virtualized session list and transcript, tabs, the home
  screen with first-run steps, and the window reopening where it was left.
- Review: unified and split diffs, staging by file and hunk, commits, pull
  requests through `gh`, a history graph, and line comments sent back to
  the agent.
- Terminals owned by the daemon, with splits, scrollback search and file
  links; a files surface with an editor and language servers; a browser
  surface with element inspection.
- Inbox (the e1 GitHub client, `--features github`), notes, skills, quick
  commands, and settings; English and Japanese.
- Desktop notifications with sounds, and a Dock badge.

### Robustness
- Crash reports written to the logs directory, and the daemon's settings
  read again when edited by hand.
