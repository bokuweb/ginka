---
name: ginka-chat
description: Follow and steer an agent session Ginka is running — read what it said, send a follow-up, answer its questions, stop it. Use when checking on or talking to an agent started through Ginka.
---

# Follow a Ginka session

```bash
ginka session list [<workspace>]        # sessions, newest first, with their state
ginka session log <session>             # the transcript so far
ginka session log <session> --after <n> # only what came after position n
```

A session's state is `running` while it works, `awaiting_input` when it asked
something, and `idle` or `finished` when its turn is over.

- **Send a follow-up.** If the agent is still working it is queued behind the
  turn, or steered into it where the agent can take it.

  ```bash
  ginka session send <session> "<text>"
  ginka session queue <session>          # what is waiting
  ```

- **Answer a question, plan or permission request.** The transcript shows
  each request's id in brackets, and a question's options after it.

  ```bash
  ginka session respond <session> <request-id> "<answer>"
  ```

- **Stop it:** `ginka session cancel <session>`.
- **Carry on elsewhere:** `ginka session fork <session> --agent codex` copies
  the conversation onto another agent.

Poll `ginka session list` rather than looping on `session log`: the state
column is what says a turn is over.
