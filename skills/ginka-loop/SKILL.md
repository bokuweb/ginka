---
name: ginka-loop
description: Drive a task to done with another agent through Ginka — start it, wait for its turn, review the diff, send comments back, and commit or open a pull request; or schedule a prompt to run on a timer. Use for supervising an agent's work end to end.
---

# Review loop with Ginka

1. **Start** the agent in a workspace (see `ginka-start`) and note the
   session id.
2. **Wait** for the turn to end: `ginka session list <workspace>` until the
   state is `idle` or `finished`; `awaiting_input` means it asked something
   (`ginka session log`, then `ginka session respond`).
3. **Read the work:**

   ```bash
   ginka changes <workspace>            # files and counts
   ginka changes <workspace> --patch    # the diff itself
   ```

4. **Send it back** with comments anchored to lines, as one message:

   ```bash
   ginka review add <workspace> <path> "<what is wrong>" --line <n>
   ginka review send <workspace> <session>
   ```

   Then go back to step 2.
5. **Finish:**

   ```bash
   ginka commit <workspace> --generate  # or: ginka commit <workspace> "<message>"
   ginka pr <workspace>                 # push and open a pull request with gh
   ```

   Several attempts from a fan-out? Merge the one that won:
   `ginka workspace merge <workspace> --message "<message>"`.

**On a timer:** `ginka cron add <project> <name> --schedule "0 9 * * 1-5"
--prompt "<prompt>" --agent claude` starts a conversation on a schedule;
`--shell "<command>"` runs a command in a terminal instead. `ginka cron list`
shows when each fires next.
