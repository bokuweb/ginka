---
name: ginka-start
description: Start another coding agent through Ginka in a worktree of its own — one attempt, or several at once to compare. Use when asked to hand a task to another agent, run agents in parallel, or try several approaches.
---

# Start an agent with Ginka

Ginka runs agents (Claude Code, Codex, Gemini, OpenCode) in git worktrees,
through a daemon the `ginka` command talks to. Everything below is a shell
command; add `--json` to any of them for machine-readable output.

1. Find the project and a workspace to run in:

   ```bash
   ginka project list
   ginka workspace list <project>
   ginka agents                      # which agents are installed and signed in
   ```

   A new worktree on a new branch, so the work stays apart from yours:

   ```bash
   ginka workspace new <project> <branch>
   ```

2. Start the agent there with its first prompt. It prints the session id.

   ```bash
   ginka session start <project>/<branch> "<prompt>" --agent claude
   ```

3. Or ask the same question several ways at once — one worktree per attempt,
   branches named `<prefix>-1`, `<prefix>-2`, …:

   ```bash
   ginka fan-out <project> <prefix> "<prompt>" --agent claude --agent codex
   ```

Then follow it with the `ginka-chat` skill. Pick an agent that `ginka agents`
reports ready; give the prompt everything the other agent needs, since it
does not see this conversation.
