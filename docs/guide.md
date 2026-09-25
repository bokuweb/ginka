# Using Ginka

Ginka runs coding agents — Claude Code, Codex, Gemini CLI, OpenCode — each in
a git worktree of its own, and keeps what they do in one place: the
conversation, the diff, the terminals, and what it cost. A background daemon
owns all of it, so agents keep working when the window closes; the window,
the `ginka` command and agents themselves (over MCP) are all clients of it.

This guide walks through a first session. [`cli.md`](cli.md) lists every
command.

## Install

- **macOS:** open `Ginka-<version>.dmg` and drag Ginka to Applications. The
  command line is inside the app; to put it on your `PATH`:

  ```bash
  ln -s /Applications/Ginka.app/Contents/MacOS/ginka /usr/local/bin/ginka
  ```

- **Linux:** unpack `ginka-<version>-linux-<arch>.tar.gz` and run
  `./install.sh`, which installs into `~/.local` (`PREFIX=` to choose) and
  adds a desktop entry. `./install.sh uninstall` removes it again.

- **From source:** `cargo run` starts the window; the command line is
  `cargo run -p ginka-cli --`. The browser surface needs the window run as
  an app: `scripts/dev-macos` builds one with Chromium inside (the first
  build downloads Chromium to `~/.local/share/cef`, about 500MB, and needs
  `cmake` and `ninja`).

Ginka runs the agent CLIs you already have and are signed in to; it never
signs in for you. `ginka agents` says which ones it found.

## First run

The home screen says what is left before the first prompt: an agent that
can run, and a project to run it in. **Add project…** registers a
repository or a plain folder. Then type what you want done and press Enter.

The conversation runs in a worktree on a branch of its own, so the agent's
changes stay apart from yours. The sidebar lists every workspace; a row says
whether its agent is working, waiting for you, or done.

## While an agent works

- **Follow up** by typing again. If the agent is mid-turn, the message is
  steered into the turn where the agent allows it, and queued otherwise; the
  queue is above the composer, where it can be edited, reordered, held or
  sent now.
- **Answer** a question, plan or permission request from its card.
- **Stop** with the stop button that replaces send while the agent works.
- **Access mode** (the chip in the composer) says what the agent may do
  without asking: read only, ask, or auto.

## Reviewing the work

The right panel's **Changes** surface shows what changed — unstaged and
staged, unified or side by side. Stage or throw away a file or a single
hunk, click a line to comment on it, and **Send to the agent** returns every
comment to the agent as one message. Commit, push and open a pull request
from the commit box (`gh` must be signed in for pull requests).

Every finished turn is a checkpoint: **Rewind** puts the worktree back to
how it was, and a turn's footer offers **Fork** onto another agent or
**Second opinion** from one.

## Several attempts at once

**Try several ways** in the composer asks the same prompt in a worktree per
attempt — several agents, several times each. **Compare** sets the attempts
side by side; **Keep** archives the others, and **Merge** merges the winner
into the branch your project is on first.

## Terminals, files and the browser

- The terminal dock at the bottom belongs to the daemon: builds keep running
  when the window closes. Quick commands (▶) run saved commands in it.
- The **Files** surface searches and edits the worktree, with language
  servers where they are installed.
- The **Browser** surface is a web view per workspace, with history
  completion and find in page; **Inspect** sends a page element to the
  composer.

## On a schedule

**Settings → Scheduled** (or `ginka cron`) runs a prompt or a command on a
cron schedule, in a workspace or the project's own checkout. A run is
skipped while the previous one is still going.

## What it cost

**Reports** shows tokens and cost by day, agent, login, model and project —
including agents you ran in a terminal outside Ginka, read from their own
session logs. A cost marked ≈ is estimated from the public rate table
rather than reported by the vendor.

## Several logins

A provider can have more than one login (`ginka account add`); each keeps
its own directory, and the composer's account chip picks which one a new
conversation uses. The usage chip shows the tightest rate-limit window.

## Letting agents drive Ginka

`ginka skills install` gives Claude Code and Codex skills for starting other
agents, following a session, using a terminal and running a review loop —
all through the `ginka` command. Agents Ginka starts are also given Ginka's
MCP server.

## Where things are

State lives in `~/.ginka` (`GINKA_HOME` to move it): the database, the
daemon's settings (`settings.json`, also `ginka settings show|set`), logs,
and crash reports. `ginka doctor` reports on it.
