# The `ginka` command

Every operation the window offers, from a terminal or a script. The command
talks to the daemon, starting it when it is not running; `--json` on any
command prints the daemon's own answer instead of a table. In a checkout
the command is built as `ginka-cli`.

This page lists every command. `ginka <command> --help` says the rest.

## `ginka doctor`

Report where Ginka keeps its state and whether that state is healthy

`ginka doctor`

## `ginka daemon`

Inspect and control the background daemon

- `ginka daemon status` — Report whether a daemon is running, and where
- `ginka daemon start` — Start a daemon if there is not one already
- `ginka daemon stop` — Ask the running daemon to exit. Agents it is running are stopped

## `ginka project`

Manage registered projects

- `ginka project add` — Register a repository or folder
- `ginka project list` — List registered projects
- `ginka project search` — Find literal source lines across every active workspace (`ginka project search <PROJECT> <QUERY>`)
- `ginka project remove` — Forget a project. Its files are left alone (`ginka project remove <PROJECT>`)
- `ginka project label` — Name a project the way you group it; an empty label clears it (`ginka project label <PROJECT> <LABEL>`)
- `ginka project move` — Put a project at a position in the order, 0 first (`ginka project move <PROJECT> <INDEX>`)

## `ginka workspace`

Manage workspaces, which are git worktrees

- `ginka workspace list` — List workspaces, reconciling against git first
- `ginka workspace new` — Create a worktree on a new branch (`ginka workspace new <PROJECT> <BRANCH>`)
- `ginka workspace scratch` — Make somewhere to work with no project at all
- `ginka workspace remove` — Remove a workspace's worktree (`ginka workspace remove <PROJECT> <NAME>`)
- `ginka workspace branches` — List the repository's local branches, and where each is checked out (`ginka workspace branches <WORKSPACE>`)
- `ginka workspace checkout` — Check a branch out in a workspace. The workspace keeps its id (`ginka workspace checkout <WORKSPACE> <BRANCH>`)
- `ginka workspace merge` — Merge a workspace's branch into another — by default the branch the project is on. A conflict is aborted and named (`ginka workspace merge <WORKSPACE>`)
- `ginka workspace index` — Build zvec-grep's index for a workspace, here in this terminal, so agents started in it get semantic search. Needs `zg` on PATH (`ginka workspace index <WORKSPACE>`)
- `ginka workspace pin` — Pin a workspace so it sorts first (`ginka workspace pin <WORKSPACE>`)
- `ginka workspace archive` — Archive a workspace without removing its worktree or conversation (`ginka workspace archive <WORKSPACE>`)
- `ginka workspace status` — Write the line of status the sidebar shows under a workspace, or clear it by giving none (`ginka workspace status <WORKSPACE> [NOTE]`)

## `ginka agents`

Report which agent CLIs this machine has, and whether they are usable

`ginka agents`

## `ginka account`

Manage logins: several per provider, each in a directory of its own

- `ginka account list` — List every login, with whether it is signed in and the latest reading of its rate-limit windows
- `ginka account add` — Add a login for a provider (`ginka account add --provider <PROVIDER> <ID>`)
- `ginka account remove` — Forget a login. Its directory — the vendor's sign-in — is kept unless asked otherwise (`ginka account remove <ID>`)
- `ginka account select` — Select the login future sessions of its provider use (`ginka account select <ID>`)
- `ginka account login` — Run the vendor's own sign-in for a login, here in this terminal (`ginka account login <ID>`)
- `ginka account refresh` — Ask the provider how much of a login's rate-limit windows is left (`ginka account refresh <ID>`)

## `ginka session`

Start and steer agent sessions

- `ginka session list` — List sessions, most recently active first
- `ginka session start` — Start an agent in a workspace (`ginka session start <WORKSPACE> <PROMPT>`)
- `ginka session send` — Send a follow-up. Queued if the agent is still working. With `--from <SESSION>` it is sent as that session, and the receiver is told who sent it and how to answer (`ginka session send <SESSION> <TEXT>`)
- `ginka session queue` — List follow-ups waiting behind the active turn (`ginka session queue <SESSION>`)
- `ginka session queue-edit` — Replace one queued follow-up without moving it (`ginka session queue-edit <SESSION> <ID> <TEXT>`)
- `ginka session queue-remove` — Remove one queued follow-up (`ginka session queue-remove <SESSION> <ID>`)
- `ginka session queue-move` — Move one queued follow-up to a zero-based position (`ginka session queue-move <SESSION> <ID> <INDEX>`)
- `ginka session queue-send-now` — Inject one queued follow-up into the active turn when supported (`ginka session queue-send-now <SESSION> <ID>`)
- `ginka session edit` — Edit a sent prompt, by the position `session log` shows it at, and run the conversation again from it in a new session (`ginka session edit <SESSION> <SEQ> <TEXT>`)
- `ginka session queue-add` — Queue a follow-up even where the running turn could take it now (`ginka session queue-add <SESSION> <TEXT>`)
- `ginka session queue-interrupt` — Stop the running turn and send this queued follow-up next (`ginka session queue-interrupt <SESSION> <ID>`)
- `ginka session queue-pause` — Hold the queue, or let it go with `--resume` (`ginka session queue-pause <SESSION>`)
- `ginka session queue-clear` — Throw away every queued follow-up (`ginka session queue-clear <SESSION>`)
- `ginka session compact` — Compact an idle provider conversation's context (`ginka session compact <SESSION>`)
- `ginka session respond` — Answer a question, plan or permission request in a running turn (`ginka session respond <SESSION> <REQUEST_ID> <RESPONSE>`)
- `ginka session options` — Replace provider options for later turns. Omitted options use the provider default (`ginka session options <SESSION>`)
- `ginka session cancel` — Stop an agent's process tree (`ginka session cancel <SESSION>`)
- `ginka session rename` — Rename a conversation (`ginka session rename <SESSION> <TITLE>`)
- `ginka session remove` — Forget a session, its transcript and its checkpoints (`ginka session remove <SESSION>`)
- `ginka session fork` — Take a copy of a conversation as it was, and carry on from there (`ginka session fork <SESSION>`)
- `ginka session cli` — List conversations an agent's own CLI started in a workspace's directory, which Ginka can adopt (`ginka session cli <WORKSPACE>`)
- `ginka session adopt` — Bring a conversation started in an agent's CLI into Ginka and carry on from it: the next turn resumes the same thread (`ginka session adopt <WORKSPACE> <AGENT> <ID>`)
- `ginka session search` — Find what was said, across conversations (`ginka session search <QUERY>`)
- `ginka session log` — Print a session's transcript (`ginka session log <SESSION>`)

## `ginka attach`

Store a file the daemon keeps, and print the reference a message refers to it by

`ginka attach <PATH>`

## `ginka files`

List a workspace's files, best matches first

`ginka files <WORKSPACE> [QUERY]`

## `ginka fan-out`

Ask the same question in several worktrees at once

`ginka fan-out --agent <AGENTS> <PROJECT> <PREFIX> <PROMPT>`

## `ginka search`

Find lines in a workspace's files

`ginka search <WORKSPACE> <QUERY>`

## `ginka show`

Print one of a workspace's files

`ginka show <WORKSPACE> <PATH>`

## `ginka save`

Save an existing UTF-8 workspace file without overwriting a newer edit

`ginka save --expected-revision <EXPECTED_REVISION> <WORKSPACE> <PATH>`

## `ginka skills`

The skills the agents can load, and whether each is on

- `ginka skills list` — List every skill, grouped across the places it was installed
- `ginka skills enable` — Turn every copy of a skill on (`ginka skills enable <NAME>`)
- `ginka skills disable` — Hide a skill from every agent by renaming its SKILL.md. Nothing is deleted (`ginka skills disable <NAME>`)
- `ginka skills install` — Install the skills that teach an agent to drive Ginka into Claude Code's and Codex's skills directories

## `ginka commands`

List the commands a workspace offers after `/`

`ginka commands <WORKSPACE>`

## `ginka slack`

The Slack connector: a bound channel starts an agent here, and the answer goes back to the thread (`docs/connectors.md`)

- `ginka slack status` — Whether the connector is configured, connected, and listening where
- `ginka slack bindings` — The channels the bot listens in, and where each one runs
- `ginka slack allow` — Let one more Slack member speak to the bot, by member id (`U…`) (`ginka slack allow <SENDER>`)
- `ginka slack test` — Post one message into a channel and take it back, to prove the tokens and the channel id are right (`ginka slack test <CHANNEL>`)

## `ginka usage`

Report what the work has cost

`ginka usage`

## `ginka changes`

Show what has changed in a workspace

`ginka changes <WORKSPACE>`

## `ginka history`

Show recent commits in a workspace, newest first

`ginka history <WORKSPACE>`

## `ginka review`

Leave comments on a diff and send them back to the agent

- `ginka review add` — Leave a comment on a file, and a line of it (`ginka review add <WORKSPACE> <PATH> <TEXT>`)
- `ginka review list` — Every comment waiting, in reading order (`ginka review list <WORKSPACE>`)
- `ginka review remove` — Take one comment back (`ginka review remove <COMMENT>`)
- `ginka review send` — Send the batch to a session's agent as one message (`ginka review send <WORKSPACE> <SESSION>`)

## `ginka stage`

Put a file into the next commit, or take it back out

`ginka stage <WORKSPACE> <PATH>`

## `ginka stage-hunk`

Put one exact diff hunk into the next commit, or take it back out

`ginka stage-hunk <WORKSPACE> <PATH> <HEADER>`

## `ginka revert-hunk`

Throw away one exact unstaged diff hunk

`ginka revert-hunk <WORKSPACE> <PATH> <HEADER>`

## `ginka revert`

Throw away a file's uncommitted work

`ginka revert <WORKSPACE> <PATH>`

## `ginka resolve`

Hand a stopped merge, rebase or cherry-pick's conflicts to an agent to resolve and finish

`ginka resolve <WORKSPACE>`

## `ginka commit`

Commit a workspace's work. `--amend` folds it into the last commit instead — keeping that commit's message when none is given — and is refused once the commit has been pushed.

`ginka commit <WORKSPACE> [MESSAGE]`

## `ginka push`

Push a workspace's branch, setting an upstream if it has none

`ginka push <WORKSPACE>`

## `ginka pull`

Fetch and fast-forward a clean workspace branch from its upstream

`ginka pull <WORKSPACE>`

## `ginka sync`

Bring a workspace branch level with its remote: publish it if it was never pushed, otherwise fast-forward, then push what is ahead

`ginka sync <WORKSPACE>`

## `ginka pr`

Push a workspace's branch and open a pull request for it with `gh`, titled from its commits. Prints the pull request's address

`ginka pr <WORKSPACE>`

## `ginka notes`

Markdown notes, kept by the daemon

- `ginka notes list` — List notes, most recently touched first
- `ginka notes show` — Print one note's markdown (`ginka notes show <ID>`)
- `ginka notes add` — Write a new note. The body is read from stdin when `--body` is absent
- `ginka notes edit` — Replace a note's title and body. The body is read from stdin when `--body` is absent (`ginka notes edit <ID>`)
- `ginka notes remove` — Forget a note (`ginka notes remove <ID>`)

## `ginka tickets`

Tickets: work an agent handed over, to start in its own session

- `ginka tickets list` — List tickets, newest first: open ones unless `--all`
- `ginka tickets raise` — Raise a ticket. The prompt is read from stdin when `--prompt` is absent (`ginka tickets raise <WORKSPACE>`)
- `ginka tickets start` — Start an open ticket in a new session (`ginka tickets start <TICKET>`)
- `ginka tickets dismiss` — Decide against an open ticket (`ginka tickets dismiss <TICKET>`)

## `ginka quick`

Saved shell commands and prompts, run in a workspace

- `ginka quick list` — List a project's quick commands and the global ones
- `ginka quick add` — Save a shell command (`--shell`) or a prompt (`--prompt`) (`ginka quick add <NAME>`)
- `ginka quick remove` — Forget a quick command (`ginka quick remove <ID>`)
- `ginka quick run` — Run a shell quick command in a new terminal in the workspace (`ginka quick run <WORKSPACE> <ID>`)

## `ginka settings`

The daemon's settings: show them, or change one

- `ginka settings show` — Print the daemon's settings as JSON, environment values hidden
- `ginka settings set` — Change one top-level setting. The value is JSON — `false`, `7`, `["codex"]` — and anything that is not is taken as a string (`ginka settings set <KEY> <VALUE>`)

## `ginka terminal`

The daemon's terminals in a workspace: open one, type into it, read what it printed, close it

- `ginka terminal list` — The terminals running in a workspace (`ginka terminal list <WORKSPACE>`)
- `ginka terminal open` — Open a shell in a workspace, and print its id (`ginka terminal open <WORKSPACE>`)
- `ginka terminal send` — Type a line into a terminal, and press Enter unless told not to (`ginka terminal send <TERMINAL> <TEXT>`)
- `ginka terminal read` — Print what a terminal has shown lately, as plain text (`ginka terminal read <TERMINAL>`)
- `ginka terminal close` — Close a terminal and stop its shell (`ginka terminal close <TERMINAL>`)

## `ginka cron`

Prompts and commands run on a cron schedule, on this machine's clock

- `ginka cron list` — List scheduled jobs, with when each fires next and how it last went
- `ginka cron add` — Schedule a shell command (`--shell`) or a prompt for an agent (`--prompt` with `--agent`) (`ginka cron add --schedule <SCHEDULE> <PROJECT> <NAME>`). `--precheck '<command>'` runs first on every scheduled firing, in the job's checkout; a non-zero exit skips that firing
- `ginka cron remove` — Forget a scheduled job and its history (`ginka cron remove <ID>`)
- `ginka cron run` — Fire a job now, as its schedule would (`ginka cron run <ID>`)
- `ginka cron runs` — A job's firings, most recent first (`ginka cron runs <ID>`)

## `ginka mcp`

Serve Ginka's operations to an agent over MCP, on stdin and stdout

`ginka mcp`

## `ginka checkpoint`

Rewind a workspace to a saved state

- `ginka checkpoint list` — List a workspace's checkpoints, newest first (`ginka checkpoint list <WORKSPACE>`)
- `ginka checkpoint restore` — Put a workspace back to a checkpoint's state (`ginka checkpoint restore <CHECKPOINT>`)
