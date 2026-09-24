---
name: ginka-terminal
description: Run a command in one of Ginka's workspace terminals and read its output — for builds, dev servers and anything long-running that should outlive this turn. Use when a command should run in a Ginka workspace rather than in this shell.
---

# Use a Ginka terminal

Ginka's terminals belong to its daemon: they keep running when this agent's
turn ends, and the reader sees them in the window's terminal dock.

```bash
ginka terminal open <workspace>              # prints the terminal id
ginka terminal send <terminal> "<command>"   # types it and presses Enter
ginka terminal read <terminal>               # what it has shown lately, as plain text
ginka terminal list <workspace>
ginka terminal close <terminal>              # stops its shell
```

`send --no-enter` types without pressing Enter. `read` returns the recent
scrollback, so read again to see new output — for example after waiting for
a build to finish.

Commands the project saves for everyone are quick commands:

```bash
ginka quick list --project <project>
ginka quick run <workspace> <id>             # runs in a new terminal named after it
```
