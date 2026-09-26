//! The MCP surface: the daemon's operations as tools an agent can call.
//!
//! `AGENTS.md` rule 3 says the UI, the CLI and MCP are the same capability
//! list, and this is the third of them: one tool per request, translated into
//! the protocol's own `Request` and answered with the protocol's own
//! `Response` as JSON. Nothing here reaches into the domain directly, so a
//! capability cannot exist for an agent and not for a person.
//!
//! The transport is stdio JSON-RPC, which is what an agent spawns; the state
//! still lives in the daemon, and the process running this is a bridge to it.
//! Everything in this module is pure — a message in, a message out — because
//! that is what makes a protocol testable without a socket.

use anyhow::{Result, anyhow};
use ginka_protocol::rpc::Request;
use ginka_protocol::{CheckpointId, ProjectName, SessionId, WorkspaceId};
use serde_json::{Value, json};

/// The version of the MCP spec these messages are shaped for.
///
/// Sent back verbatim from `initialize`: a client that asked for another one
/// is told what it is actually talking to rather than being refused, which is
/// what the spec asks for.
pub const PROTOCOL_VERSION: &str = "2024-11-05";

/// One tool, as `tools/list` describes it.
pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    /// The JSON Schema of its arguments.
    pub schema: Value,
}

/// Every tool the daemon offers.
///
/// Named `ginka_*` because an agent sees them beside every other server's
/// tools, and a bare `commit` in that list is ambiguous in a way that costs a
/// wrong call to find out.
pub fn tools() -> Vec<Tool> {
    let workspace = json!({
        "type": "string",
        "description": "The workspace id, as ginka_workspaces lists them",
    });
    vec![
        Tool {
            name: "ginka_projects",
            description: "List the repositories Ginka knows about.",
            schema: json!({"type": "object", "properties": {}}),
        },
        Tool {
            name: "ginka_workspaces",
            description: "List the worktree workspaces, optionally in one project.",
            schema: json!({
                "type": "object",
                "properties": {"project": {"type": "string"}},
            }),
        },
        Tool {
            name: "ginka_workspace_create",
            description: "Cut a new worktree workspace on a branch.",
            schema: json!({
                "type": "object",
                "properties": {
                    "project": {"type": "string"},
                    "branch": {"type": "string"},
                    "base": {"type": "string", "description": "What to branch from; the project's default branch otherwise"},
                },
                "required": ["project", "branch"],
            }),
        },
        Tool {
            name: "ginka_workspace_archive",
            description: "Archive a workspace without deleting it, or restore it to active work.",
            schema: json!({
                "type": "object",
                "properties": {
                    "workspace": workspace,
                    "restore": {"type": "boolean", "description": "Restore instead of archive"},
                },
                "required": ["workspace"],
            }),
        },
        Tool {
            name: "ginka_agents",
            description: "Which coding agents this machine has, and whether they are signed in.",
            schema: json!({"type": "object", "properties": {}}),
        },
        Tool {
            name: "ginka_accounts",
            description: "Every login of every provider, with whether each is signed in. A session can be started on one by id.",
            schema: json!({"type": "object", "properties": {}}),
        },
        Tool {
            name: "ginka_account_select",
            description: "Select the login future sessions of its provider use. Running sessions keep their original login.",
            schema: json!({
                "type": "object",
                "properties": {"account": {"type": "string"}},
                "required": ["account"],
            }),
        },
        Tool {
            name: "ginka_sessions",
            description: "List agent sessions, newest first.",
            schema: json!({
                "type": "object",
                "properties": {"workspace": workspace},
            }),
        },
        Tool {
            name: "ginka_session_start",
            description: "Start an agent in a workspace and send it a prompt.",
            schema: json!({
                "type": "object",
                "properties": {
                    "workspace": workspace,
                    "agent": {"type": "string", "description": "A driver id: claude, codex"},
                    "prompt": {"type": "string"},
                    "model": {"type": "string"},
                    "reasoning_effort": {"type": "string", "description": "A value advertised for the selected model"},
                    "service_tier": {"type": "string", "description": "A value advertised for the selected model"},
                    "access": {"type": "string", "enum": ["read-only", "ask", "auto"], "description": "What the agent may touch; ask (edit freely, commands asked about) otherwise"},
                    "account": {"type": "string", "description": "An account id from ginka_accounts; the provider's active account otherwise"},
                },
                "required": ["workspace", "agent", "prompt"],
            }),
        },
        Tool {
            name: "ginka_fan_out",
            description: "Ask the same question in one new worktree per attempt, and start an agent in each.",
            schema: json!({
                "type": "object",
                "properties": {
                    "project": {"type": "string"},
                    "prefix": {"type": "string", "description": "What the branches are called: prefix-1, prefix-2"},
                    "prompt": {"type": "string"},
                    "agents": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "One driver id per attempt; repeats ask the same agent twice",
                    },
                    "base": {"type": "string"},
                },
                "required": ["project", "prefix", "prompt", "agents"],
            }),
        },
        Tool {
            name: "ginka_session_fork",
            description: "Copy a conversation up to a point and carry on from there. Naming another agent or account moves it: the new agent is handed a digest of the transcript with its first prompt, because it cannot continue the old one's thread.",
            schema: json!({
                "type": "object",
                "properties": {
                    "session": {"type": "string"},
                    "after": {"type": "integer", "description": "The transcript position to fork at; all of it otherwise"},
                    "agent": {"type": "string", "description": "A driver id to move the conversation to: claude, codex"},
                    "model": {"type": "string"},
                    "account": {"type": "string", "description": "An account id from ginka_accounts"},
                },
                "required": ["session"],
            }),
        },
        Tool {
            name: "ginka_session_send",
            description: "Send a follow-up to a session. Queued if it is mid-turn.",
            schema: json!({
                "type": "object",
                "properties": {"session": {"type": "string"}, "text": {"type": "string"}},
                "required": ["session", "text"],
            }),
        },
        Tool {
            name: "ginka_session_queue",
            description: "List follow-ups waiting behind a session's active turn, in dispatch order.",
            schema: json!({
                "type": "object",
                "properties": {"session": {"type": "string"}},
                "required": ["session"],
            }),
        },
        Tool {
            name: "ginka_queue_edit",
            description: "Replace one queued follow-up without changing its dispatch position.",
            schema: json!({
                "type": "object",
                "properties": {
                    "session": {"type": "string"},
                    "id": {"type": "integer", "minimum": 1},
                    "text": {"type": "string"}
                },
                "required": ["session", "id", "text"],
            }),
        },
        Tool {
            name: "ginka_queue_remove",
            description: "Remove one queued follow-up before it reaches the transcript.",
            schema: json!({
                "type": "object",
                "properties": {
                    "session": {"type": "string"},
                    "id": {"type": "integer", "minimum": 1}
                },
                "required": ["session", "id"],
            }),
        },
        Tool {
            name: "ginka_queue_move",
            description: "Move one queued follow-up to a zero-based dispatch position.",
            schema: json!({
                "type": "object",
                "properties": {
                    "session": {"type": "string"},
                    "id": {"type": "integer", "minimum": 1},
                    "index": {"type": "integer", "minimum": 0}
                },
                "required": ["session", "id", "index"],
            }),
        },
        Tool {
            name: "ginka_session_edit",
            description: "Edit a sent prompt (by transcript seq) and run the conversation again from it in a new session; the worktree goes back to the checkpoint before that prompt and the original conversation is kept.",
            schema: json!({
                "type": "object",
                "properties": {
                    "session": {"type": "string"},
                    "seq": {"type": "integer", "minimum": 1},
                    "text": {"type": "string"}
                },
                "required": ["session", "seq", "text"],
            }),
        },
        Tool {
            name: "ginka_queue_add",
            description: "Queue a follow-up behind the running turn instead of steering it in. With nothing running or waiting it is sent at once.",
            schema: json!({
                "type": "object",
                "properties": {"session": {"type": "string"}, "text": {"type": "string"}},
                "required": ["session", "text"],
            }),
        },
        Tool {
            name: "ginka_queue_interrupt",
            description: "Stop the running turn and send this queued follow-up next; the rest of the queue keeps its order.",
            schema: json!({
                "type": "object",
                "properties": {
                    "session": {"type": "string"},
                    "id": {"type": "integer", "minimum": 1}
                },
                "required": ["session", "id"],
            }),
        },
        Tool {
            name: "ginka_queue_pause",
            description: "Hold a session's queue (paused: true) or let it go (paused: false), which sends the first prompt if nothing is running.",
            schema: json!({
                "type": "object",
                "properties": {"session": {"type": "string"}, "paused": {"type": "boolean"}},
                "required": ["session", "paused"],
            }),
        },
        Tool {
            name: "ginka_queue_send_now",
            description: "Inject one queued follow-up into the active turn when the transport supports live input. A refusal leaves it queued.",
            schema: json!({
                "type": "object",
                "properties": {
                    "session": {"type": "string"},
                    "id": {"type": "integer", "minimum": 1}
                },
                "required": ["session", "id"],
            }),
        },
        Tool {
            name: "ginka_session_compact",
            description: "Compact an idle provider conversation's context. Refused while a turn is running or when the provider has no explicit compact operation.",
            schema: json!({
                "type": "object",
                "properties": {"session": {"type": "string"}},
                "required": ["session"],
            }),
        },
        Tool {
            name: "ginka_session_respond",
            description: "Answer a question, plan or permission request inside a running turn. Use the request id from the transcript event.",
            schema: json!({
                "type": "object",
                "properties": {
                    "session": {"type": "string"},
                    "request_id": {"type": "string"},
                    "response": {"type": "string"}
                },
                "required": ["session", "request_id", "response"],
            }),
        },
        Tool {
            name: "ginka_session_options",
            description: "Replace the model, reasoning effort and service tier used by later turns. Omitted values select provider defaults. The response says whether the provider thread absorbed the change or needs a restart.",
            schema: json!({
                "type": "object",
                "properties": {
                    "session": {"type": "string"},
                    "model": {"type": "string"},
                    "reasoning_effort": {"type": "string"},
                    "service_tier": {"type": "string"}
                },
                "required": ["session"],
            }),
        },
        Tool {
            name: "ginka_session_cancel",
            description: "Stop whatever a session is running.",
            schema: json!({
                "type": "object",
                "properties": {"session": {"type": "string"}},
                "required": ["session"],
            }),
        },
        Tool {
            name: "ginka_transcript",
            description: "Read what a session has said, from a sequence number.",
            schema: json!({
                "type": "object",
                "properties": {
                    "session": {"type": "string"},
                    "after": {"type": "integer"},
                    "limit": {"type": "integer"},
                },
                "required": ["session"],
            }),
        },
        Tool {
            name: "ginka_changes",
            description: "The diff of a workspace: what has changed and how.",
            schema: json!({
                "type": "object",
                "properties": {
                    "workspace": workspace,
                    "staged": {"type": "boolean", "description": "Only what is staged for the next commit"},
                    "unstaged": {"type": "boolean", "description": "Only worktree edits not yet in the index"},
                    "since_checkpoint": {"type": "string"},
                    "commit": {"type": "string", "description": "What one commit did, by the id ginka_history gives"},
                },
                "required": ["workspace"],
            }),
        },
        Tool {
            name: "ginka_quick_commands",
            description: "The reader's saved shell commands and prompts: a project's own and the global ones.",
            schema: json!({
                "type": "object",
                "properties": {"project": {"type": "string"}},
            }),
        },
        Tool {
            name: "ginka_quick_run",
            description: "Run a saved shell command in a new terminal in a workspace; answers with the terminal id.",
            schema: json!({
                "type": "object",
                "properties": {"workspace": workspace, "id": {"type": "string"}},
                "required": ["workspace", "id"],
            }),
        },
        Tool {
            name: "ginka_notes",
            description: "The reader's markdown notes, most recently touched first; one project's when project is given.",
            schema: json!({
                "type": "object",
                "properties": {"project": {"type": "string"}},
            }),
        },
        Tool {
            name: "ginka_note_save",
            description: "Write a markdown note. Without an id it is a new one; with one, it replaces that note's title and body.",
            schema: json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string"},
                    "project": {"type": "string"},
                    "title": {"type": "string"},
                    "body": {"type": "string"},
                },
                "required": ["body"],
            }),
        },
        Tool {
            name: "ginka_stage_hunk",
            description: "Stage or unstage one exact hunk from the current diff. Stale hunk headers are refused.",
            schema: json!({
                "type": "object",
                "properties": {
                    "workspace": workspace,
                    "path": {"type": "string"},
                    "header": {"type": "string"},
                    "staged": {"type": "boolean"},
                },
                "required": ["workspace", "path", "header", "staged"],
            }),
        },
        Tool {
            name: "ginka_revert_hunk",
            description: "Permanently discard one exact unstaged hunk. Stale hunk headers are refused.",
            schema: json!({
                "type": "object",
                "properties": {
                    "workspace": workspace,
                    "path": {"type": "string"},
                    "header": {"type": "string"},
                },
                "required": ["workspace", "path", "header"],
            }),
        },
        Tool {
            name: "ginka_history",
            description: "Read a workspace's recent commit history with parent topology.",
            schema: json!({
                "type": "object",
                "properties": {
                    "workspace": workspace,
                    "limit": {"type": "integer", "minimum": 0, "maximum": 200},
                },
                "required": ["workspace"],
            }),
        },
        Tool {
            name: "ginka_commit",
            description: "Commit a workspace's work.",
            schema: json!({
                "type": "object",
                "properties": {
                    "workspace": workspace,
                    "message": {"type": "string"},
                    "staged_only": {"type": "boolean"},
                },
                "required": ["workspace", "message"],
            }),
        },
        Tool {
            name: "ginka_workspace_merge",
            description: "Merge a workspace's branch into another — by default the branch the project is on. Uncommitted work is committed first with `message`; a conflict is aborted and named.",
            schema: json!({
                "type": "object",
                "properties": {
                    "workspace": workspace,
                    "into": {"type": "string"},
                    "message": {"type": "string"},
                },
                "required": ["workspace"],
            }),
        },
        Tool {
            name: "ginka_cron_jobs",
            description: "List the prompts and commands scheduled with cron, with when each fires next.",
            schema: json!({
                "type": "object",
                "properties": {"project": {"type": "string"}},
            }),
        },
        Tool {
            name: "ginka_cron_save",
            description: "Schedule a prompt (via chat, with an agent) or a shell command (via terminal) on a cron expression — five fields or @daily and the like, on the daemon host's clock. Omit `workspace` to run in the project's own checkout; pass `id` to replace a job.",
            schema: json!({
                "type": "object",
                "properties": {
                    "id": {"type": "integer"},
                    "project": {"type": "string"},
                    "workspace": workspace,
                    "name": {"type": "string"},
                    "schedule": {"type": "string"},
                    "via": {"type": "string", "enum": ["chat", "terminal"]},
                    "agent": {"type": "string"},
                    "body": {"type": "string"},
                    "enabled": {"type": "boolean"},
                },
                "required": ["project", "name", "schedule", "via", "body"],
            }),
        },
        Tool {
            name: "ginka_cron_remove",
            description: "Forget a scheduled job and its history.",
            schema: json!({
                "type": "object",
                "properties": {"id": {"type": "integer"}},
                "required": ["id"],
            }),
        },
        Tool {
            name: "ginka_cron_run",
            description: "Fire a scheduled job now, skipping it if its previous run is still going.",
            schema: json!({
                "type": "object",
                "properties": {"id": {"type": "integer"}},
                "required": ["id"],
            }),
        },
        Tool {
            name: "ginka_project_label",
            description: "Label a project the way the reader groups it; an empty label clears it.",
            schema: json!({
                "type": "object",
                "properties": {"project": {"type": "string"}, "label": {"type": "string"}},
                "required": ["project", "label"],
            }),
        },
        Tool {
            name: "ginka_project_move",
            description: "Put a project at a position in the sidebar order, 0 first.",
            schema: json!({
                "type": "object",
                "properties": {"project": {"type": "string"}, "index": {"type": "integer", "minimum": 0}},
                "required": ["project", "index"],
            }),
        },
        Tool {
            name: "ginka_push",
            description: "Push a workspace branch, setting its upstream on the first push.",
            schema: json!({
                "type": "object",
                "properties": {"workspace": workspace},
                "required": ["workspace"],
            }),
        },
        Tool {
            name: "ginka_sync",
            description: "Bring a workspace branch level with its remote in one action: publish it if it was never pushed, otherwise fast-forward it, then push what is ahead. Refuses dirty or diverged work.",
            schema: json!({
                "type": "object",
                "properties": {"workspace": workspace},
                "required": ["workspace"],
            }),
        },
        Tool {
            name: "ginka_pull",
            description: "Fetch and fast-forward a clean workspace branch. Refuses dirty or diverged work instead of merging or rebasing it.",
            schema: json!({
                "type": "object",
                "properties": {"workspace": workspace},
                "required": ["workspace"],
            }),
        },
        Tool {
            name: "ginka_files",
            description: "Find files in a workspace by path.",
            schema: json!({
                "type": "object",
                "properties": {
                    "workspace": workspace,
                    "query": {"type": "string"},
                    "limit": {"type": "integer"},
                },
                "required": ["workspace"],
            }),
        },
        Tool {
            name: "ginka_search",
            description: "Find lines in a workspace's files. The query is taken literally.",
            schema: json!({
                "type": "object",
                "properties": {"workspace": workspace, "query": {"type": "string"}},
                "required": ["workspace", "query"],
            }),
        },
        Tool {
            name: "ginka_project_search",
            description: "Find literal source lines across every active workspace in a project.",
            schema: json!({
                "type": "object",
                "properties": {
                    "project": {"type": "string"},
                    "query": {"type": "string"},
                    "limit": {"type": "integer"},
                },
                "required": ["project", "query"],
            }),
        },
        Tool {
            name: "ginka_read_file",
            description: "Read one of a workspace's files.",
            schema: json!({
                "type": "object",
                "properties": {"workspace": workspace, "path": {"type": "string"}},
                "required": ["workspace", "path"],
            }),
        },
        Tool {
            name: "ginka_write_file",
            description: "Save an existing UTF-8 workspace file. Pass the revision returned by ginka_read_file so a newer edit is never overwritten silently.",
            schema: json!({
                "type": "object",
                "properties": {
                    "workspace": workspace,
                    "path": {"type": "string"},
                    "text": {"type": "string"},
                    "expected_revision": {"type": "string"},
                },
                "required": ["workspace", "path", "text", "expected_revision"],
            }),
        },
        Tool {
            name: "ginka_branches",
            description: "A workspace's local branches: which is checked out there, and which other worktrees hold.",
            schema: json!({
                "type": "object",
                "properties": {"workspace": workspace},
                "required": ["workspace"],
            }),
        },
        Tool {
            name: "ginka_checkout",
            description: "Check a branch out in a workspace, creating it from HEAD when `create` is set. The workspace keeps its id.",
            schema: json!({
                "type": "object",
                "properties": {
                    "workspace": workspace,
                    "branch": {"type": "string"},
                    "create": {"type": "boolean"},
                },
                "required": ["workspace", "branch"],
            }),
        },
        Tool {
            name: "ginka_skills",
            description: "The skills installed for the agents, grouped across every place each was installed, with whether each is enabled.",
            schema: json!({
                "type": "object",
                "properties": {"project": {"type": "string", "description": "Only this project's skills, plus the user's"}},
            }),
        },
        Tool {
            name: "ginka_skill_enable",
            description: "Enable or disable every copy of a skill by name. Disabling renames its SKILL.md; nothing is deleted.",
            schema: json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "enabled": {"type": "boolean"},
                    "project": {"type": "string"},
                },
                "required": ["name", "enabled"],
            }),
        },
        Tool {
            name: "ginka_checkpoints",
            description: "The points a workspace can be rewound to.",
            schema: json!({
                "type": "object",
                "properties": {"workspace": workspace},
                "required": ["workspace"],
            }),
        },
        Tool {
            name: "ginka_restore",
            description: "Put a workspace back to a checkpoint. Destructive: work since is removed, after being snapshotted.",
            schema: json!({
                "type": "object",
                "properties": {"checkpoint": {"type": "string"}},
                "required": ["checkpoint"],
            }),
        },
    ]
}

/// Turn a `tools/call` into the request it stands for.
///
/// An unknown tool or a missing argument is an error here rather than a
/// default: a call the caller got wrong is worth saying so about, and a
/// `ginka_commit` with no message that quietly did nothing would be worse.
pub fn request_for(tool: &str, arguments: &Value) -> Result<Request> {
    let text = |key: &str| -> Result<String> {
        arguments
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow!("{tool} needs a `{key}`"))
    };
    let maybe = |key: &str| -> Option<String> {
        arguments
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let flag = |key: &str| arguments.get(key).and_then(Value::as_bool).unwrap_or(false);
    let number = |key: &str| arguments.get(key).and_then(Value::as_u64);

    Ok(match tool {
        "ginka_projects" => Request::ListProjects,
        "ginka_workspaces" => Request::ListWorkspaces {
            project: maybe("project").map(ProjectName),
        },
        "ginka_workspace_create" => Request::CreateWorkspace {
            project: ProjectName(text("project")?),
            branch: text("branch")?,
            base: maybe("base"),
        },
        "ginka_workspace_archive" => Request::ArchiveWorkspace {
            workspace: WorkspaceId(text("workspace")?),
            archived: !flag("restore"),
        },
        "ginka_agents" => Request::ListAgents,
        "ginka_accounts" => Request::Accounts,
        "ginka_account_select" => Request::SelectAccount {
            id: ginka_protocol::AccountId(text("account")?),
        },
        "ginka_sessions" => Request::ListSessions {
            workspace: maybe("workspace").map(WorkspaceId),
            origin: None,
        },
        "ginka_session_start" => Request::StartSession {
            workspace: WorkspaceId(text("workspace")?),
            agent: text("agent")?,
            prompt: text("prompt")?,
            model: maybe("model"),
            reasoning_effort: maybe("reasoning_effort"),
            service_tier: maybe("service_tier"),
            account: maybe("account").map(ginka_protocol::AccountId),
            access_mode: match maybe("access") {
                Some(word) => Some(ginka_protocol::AccessMode::parse(&word).ok_or_else(|| {
                    anyhow!("{tool}: access is read-only, ask or auto, not {word:?}")
                })?),
                None => None,
            },
            origin: None,
        },
        "ginka_fan_out" => Request::FanOut {
            project: ProjectName(text("project")?),
            branch_prefix: text("prefix")?,
            base: maybe("base"),
            prompt: text("prompt")?,
            attempts: arguments
                .get("agents")
                .and_then(Value::as_array)
                .map(|agents| {
                    agents
                        .iter()
                        .filter_map(Value::as_str)
                        .map(|agent| match agent.split_once(':') {
                            Some((agent, model)) => ginka_protocol::rpc::Attempt {
                                agent: agent.to_string(),
                                model: Some(model.to_string()),
                                account: None,
                            },
                            None => ginka_protocol::rpc::Attempt {
                                agent: agent.to_string(),
                                model: None,
                                account: None,
                            },
                        })
                        .collect()
                })
                .filter(|attempts: &Vec<_>| !attempts.is_empty())
                .ok_or_else(|| anyhow!("{tool} needs at least one entry in `agents`"))?,
        },
        "ginka_session_fork" => Request::ForkSession {
            session: SessionId(text("session")?),
            after: number("after"),
            agent: maybe("agent"),
            model: maybe("model"),
            account: maybe("account").map(ginka_protocol::AccountId),
        },
        "ginka_session_send" => Request::SendMessage {
            session: SessionId(text("session")?),
            text: text("text")?,
        },
        "ginka_session_queue" => Request::QueuedMessages {
            session: SessionId(text("session")?),
        },
        "ginka_queue_edit" => Request::EditQueuedMessage {
            session: SessionId(text("session")?),
            id: number("id").ok_or_else(|| anyhow!("{tool} needs an `id`"))?,
            text: text("text")?,
        },
        "ginka_queue_remove" => Request::RemoveQueuedMessage {
            session: SessionId(text("session")?),
            id: number("id").ok_or_else(|| anyhow!("{tool} needs an `id`"))?,
        },
        "ginka_queue_move" => Request::MoveQueuedMessage {
            session: SessionId(text("session")?),
            id: number("id").ok_or_else(|| anyhow!("{tool} needs an `id`"))?,
            index: number("index")
                .and_then(|index| u32::try_from(index).ok())
                .ok_or_else(|| anyhow!("{tool} needs an `index`"))?,
        },
        "ginka_queue_send_now" => Request::SendQueuedMessageNow {
            session: SessionId(text("session")?),
            id: number("id").ok_or_else(|| anyhow!("{tool} needs an `id`"))?,
        },
        "ginka_session_edit" => Request::EditPrompt {
            session: SessionId(text("session")?),
            seq: number("seq").ok_or_else(|| anyhow!("{tool} needs a `seq`"))?,
            text: text("text")?,
        },
        "ginka_queue_add" => Request::QueueMessage {
            session: SessionId(text("session")?),
            text: text("text")?,
        },
        "ginka_queue_interrupt" => Request::InterruptWithQueuedMessage {
            session: SessionId(text("session")?),
            id: number("id").ok_or_else(|| anyhow!("{tool} needs an `id`"))?,
        },
        "ginka_queue_pause" => Request::SetQueuePaused {
            session: SessionId(text("session")?),
            paused: flag("paused"),
        },
        "ginka_session_compact" => Request::CompactSession {
            session: SessionId(text("session")?),
        },
        "ginka_session_respond" => Request::RespondToAgent {
            session: SessionId(text("session")?),
            request_id: text("request_id")?,
            response: text("response")?,
        },
        "ginka_session_options" => Request::UpdateSessionOptions {
            session: SessionId(text("session")?),
            model: maybe("model"),
            reasoning_effort: maybe("reasoning_effort"),
            service_tier: maybe("service_tier"),
        },
        "ginka_session_cancel" => Request::CancelSession {
            session: SessionId(text("session")?),
        },
        "ginka_transcript" => Request::SessionTranscript {
            session: SessionId(text("session")?),
            after: number("after"),
            limit: number("limit").map(|limit| limit as u32),
        },
        "ginka_changes" => Request::WorkspaceChanges {
            workspace: WorkspaceId(text("workspace")?),
            source: match maybe("since_checkpoint") {
                _ if maybe("commit").is_some() => ginka_protocol::model::ChangeSource::Commit {
                    commit: maybe("commit").unwrap_or_default(),
                },
                Some(checkpoint) => ginka_protocol::model::ChangeSource::SinceCheckpoint {
                    checkpoint: CheckpointId(checkpoint),
                },
                None if flag("staged") => ginka_protocol::model::ChangeSource::Staged,
                None if flag("unstaged") => ginka_protocol::model::ChangeSource::Unstaged,
                None => ginka_protocol::model::ChangeSource::Uncommitted,
            },
        },
        "ginka_quick_commands" => Request::ListQuickCommands {
            project: maybe("project").map(ProjectName),
        },
        "ginka_quick_run" => Request::RunQuickCommand {
            workspace: WorkspaceId(text("workspace")?),
            // A string id; a number is read as one, which is what an agent
            // that copied it from a listing may hand back.
            id: maybe("id")
                .or_else(|| number("id").map(|id| id.to_string()))
                .ok_or_else(|| anyhow!("{tool} needs an `id`"))?,
            rows: 24,
            cols: 100,
        },
        "ginka_notes" => Request::ListNotes {
            project: maybe("project").map(ProjectName),
        },
        "ginka_note_save" => Request::SaveNote {
            id: maybe("id"),
            project: maybe("project").map(ProjectName),
            title: maybe("title").unwrap_or_default(),
            body: text("body")?,
        },
        "ginka_stage_hunk" => Request::StageHunk {
            workspace: WorkspaceId(text("workspace")?),
            path: text("path")?,
            header: text("header")?,
            staged: arguments
                .get("staged")
                .and_then(Value::as_bool)
                .ok_or_else(|| anyhow!("ginka_stage_hunk needs `staged`"))?,
        },
        "ginka_revert_hunk" => Request::RevertHunk {
            workspace: WorkspaceId(text("workspace")?),
            path: text("path")?,
            header: text("header")?,
        },
        "ginka_history" => Request::WorkspaceHistory {
            workspace: WorkspaceId(text("workspace")?),
            limit: number("limit").map(|limit| limit as u32),
        },
        "ginka_commit" => Request::Commit {
            workspace: WorkspaceId(text("workspace")?),
            message: text("message")?,
            all: !flag("staged_only"),
        },
        "ginka_cron_jobs" => Request::ListCronJobs {
            project: text("project").ok().map(ProjectName),
        },
        "ginka_cron_save" => Request::SaveCronJob {
            id: arguments.get("id").and_then(Value::as_i64),
            project: ProjectName(text("project")?),
            workspace: text("workspace").ok().map(WorkspaceId),
            name: text("name")?,
            schedule: text("schedule")?,
            via: ginka_protocol::model::CronVia::parse(&text("via")?)
                .ok_or_else(|| anyhow!("{tool}: `via` is chat or terminal"))?,
            agent: text("agent").ok(),
            body: text("body")?,
            enabled: arguments
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        },
        "ginka_cron_remove" => Request::RemoveCronJob {
            id: arguments
                .get("id")
                .and_then(Value::as_i64)
                .ok_or_else(|| anyhow!("{tool} needs an `id`"))?,
        },
        "ginka_cron_run" => Request::RunCronJob {
            id: arguments
                .get("id")
                .and_then(Value::as_i64)
                .ok_or_else(|| anyhow!("{tool} needs an `id`"))?,
        },
        "ginka_project_label" => Request::SetProjectLabel {
            project: ProjectName(text("project")?),
            label: text("label")?,
        },
        "ginka_project_move" => Request::MoveProject {
            project: ProjectName(text("project")?),
            index: number("index").ok_or_else(|| anyhow!("{tool} needs an `index`"))? as u32,
        },
        "ginka_workspace_merge" => Request::MergeWorkspace {
            workspace: WorkspaceId(text("workspace")?),
            into: text("into").ok(),
            message: text("message").ok(),
        },
        "ginka_push" => Request::Push {
            workspace: WorkspaceId(text("workspace")?),
        },
        "ginka_pull" => Request::Pull {
            workspace: WorkspaceId(text("workspace")?),
        },
        "ginka_sync" => Request::Sync {
            workspace: WorkspaceId(text("workspace")?),
        },
        "ginka_files" => Request::WorkspaceFiles {
            workspace: WorkspaceId(text("workspace")?),
            query: maybe("query"),
            limit: number("limit").map(|limit| limit as u32),
        },
        "ginka_search" => Request::SearchContent {
            workspace: WorkspaceId(text("workspace")?),
            query: text("query")?,
            limit: None,
        },
        "ginka_project_search" => Request::SearchProject {
            project: ProjectName(text("project")?),
            query: text("query")?,
            limit: number("limit").map(|limit| limit as u32),
        },
        "ginka_read_file" => Request::ReadFile {
            workspace: WorkspaceId(text("workspace")?),
            path: text("path")?,
        },
        "ginka_write_file" => Request::WriteFile {
            workspace: WorkspaceId(text("workspace")?),
            path: text("path")?,
            text: text("text")?,
            expected_revision: text("expected_revision")?,
        },
        "ginka_branches" => Request::ListBranches {
            workspace: WorkspaceId(text("workspace")?),
        },
        "ginka_checkout" => Request::CheckoutBranch {
            workspace: WorkspaceId(text("workspace")?),
            branch: text("branch")?,
            create: flag("create"),
        },
        "ginka_skills" => Request::ListSkills {
            project: maybe("project").map(ProjectName),
        },
        "ginka_skill_enable" => Request::SetSkillEnabled {
            name: text("name")?,
            enabled: arguments
                .get("enabled")
                .and_then(Value::as_bool)
                .ok_or_else(|| anyhow!("{tool} needs `enabled`"))?,
            project: maybe("project").map(ProjectName),
        },
        "ginka_checkpoints" => Request::ListCheckpoints {
            workspace: WorkspaceId(text("workspace")?),
        },
        "ginka_restore" => Request::RestoreCheckpoint {
            checkpoint: CheckpointId(text("checkpoint")?),
        },
        other => return Err(anyhow!("no tool called {other}")),
    })
}

/// What `initialize` answers with.
pub fn server_info() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {"tools": {"listChanged": false}},
        "serverInfo": {"name": "ginka", "version": env!("CARGO_PKG_VERSION")},
    })
}

/// What `tools/list` answers with.
pub fn tool_list() -> Value {
    json!({
        "tools": tools()
            .into_iter()
            .map(|tool| json!({
                "name": tool.name,
                "description": tool.description,
                "inputSchema": tool.schema,
            }))
            .collect::<Vec<_>>(),
    })
}

/// A tool result carrying `text`.
///
/// The protocol's own response, as JSON: an agent reading this is the same
/// audience as `ginka --json`, and inventing a second shape for it would be a
/// second thing to keep in step.
pub fn tool_result(text: String) -> Value {
    json!({"content": [{"type": "text", "text": text}]})
}

/// A tool result that says the call failed.
///
/// `isError` rather than a JSON-RPC error: the call reached the tool and the
/// tool has something to say, and the spec keeps protocol errors for the
/// messages that never got that far.
pub fn tool_failure(why: String) -> Value {
    json!({"content": [{"type": "text", "text": why}], "isError": true})
}

/// A JSON-RPC reply carrying `result`.
pub fn reply(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

/// A JSON-RPC reply carrying an error.
pub fn error(id: Value, code: i64, message: String) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// The JSON-RPC error code for a method this server does not have.
pub const METHOD_NOT_FOUND: i64 = -32601;
/// The JSON-RPC error code for a message that is not valid JSON.
pub const PARSE_ERROR: i64 = -32700;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_names_the_request_it_stands_for() {
        // The rule the module exists for: a capability an agent has is one a
        // person has, because both go through the same request.
        for tool in tools() {
            let arguments = json!({
                "project": "comet",
                "branch": "harbor",
                "workspace": "comet/harbor",
                "session": "s-1",
                "request_id": "ask-1",
                "id": 7,
                "seq": 3,
                "index": 0,
                "response": "yes",
                "agent": "claude",
                "account": "claude-work",
                "prompt": "go",
                "text": "more",
                "message": "a commit",
                "body": "# Release\nsteps",
                "query": "needle",
                "path": "src/main.rs",
                "header": "@@ -1 +1 @@",
                "staged": true,
                "expected_revision": "abc123",
                "checkpoint": "c-1",
                "prefix": "attempt",
                "agents": ["claude", "codex:gpt-5"],
                "name": "release-notes",
                "enabled": false,
                "label": "Work",
                "schedule": "@daily",
                "via": "terminal",
            });
            request_for(tool.name, &arguments)
                .unwrap_or_else(|error| panic!("{}: {error}", tool.name));
        }
    }

    #[test]
    fn a_missing_argument_is_an_error_rather_than_a_default() {
        // A commit with no message that quietly did nothing is worse than one
        // that says what it needed.
        let error = request_for("ginka_commit", &json!({"workspace": "comet/harbor"})).unwrap_err();
        assert!(error.to_string().contains("message"), "{error}");
    }

    #[test]
    fn an_unknown_tool_says_so() {
        let error = request_for("ginka_launch_missiles", &json!({})).unwrap_err();
        assert!(
            error.to_string().contains("ginka_launch_missiles"),
            "{error}"
        );
    }

    #[test]
    fn changes_can_be_asked_for_three_ways() {
        use ginka_protocol::model::ChangeSource;
        let uncommitted = request_for("ginka_changes", &json!({"workspace": "w"})).unwrap();
        let staged =
            request_for("ginka_changes", &json!({"workspace": "w", "staged": true})).unwrap();
        let since = request_for(
            "ginka_changes",
            &json!({"workspace": "w", "since_checkpoint": "c-1"}),
        )
        .unwrap();
        assert!(matches!(
            uncommitted,
            Request::WorkspaceChanges {
                source: ChangeSource::Uncommitted,
                ..
            }
        ));
        assert!(matches!(
            staged,
            Request::WorkspaceChanges {
                source: ChangeSource::Staged,
                ..
            }
        ));
        assert!(matches!(
            since,
            Request::WorkspaceChanges {
                source: ChangeSource::SinceCheckpoint { .. },
                ..
            }
        ));
    }

    #[test]
    fn a_file_catalogue_limit_crosses_the_mcp_boundary() {
        let request =
            request_for("ginka_files", &json!({"workspace": "w", "limit": 2001})).unwrap();
        assert!(matches!(
            request,
            Request::WorkspaceFiles {
                limit: Some(2001),
                ..
            }
        ));
    }

    #[test]
    fn a_project_search_crosses_the_mcp_boundary() {
        let request = request_for(
            "ginka_project_search",
            &json!({"project": "comet", "query": "needle", "limit": 17}),
        )
        .unwrap();
        assert_eq!(
            request,
            Request::SearchProject {
                project: ProjectName("comet".into()),
                query: "needle".into(),
                limit: Some(17),
            }
        );
    }

    #[test]
    fn a_workspace_archive_can_be_reversed() {
        let archive = request_for(
            "ginka_workspace_archive",
            &json!({"workspace": "comet/harbor"}),
        )
        .unwrap();
        let restore = request_for(
            "ginka_workspace_archive",
            &json!({"workspace": "comet/harbor", "restore": true}),
        )
        .unwrap();
        assert!(matches!(
            archive,
            Request::ArchiveWorkspace { archived: true, .. }
        ));
        assert!(matches!(
            restore,
            Request::ArchiveWorkspace {
                archived: false,
                ..
            }
        ));
    }

    #[test]
    fn session_model_options_cross_the_mcp_boundary() {
        let request = request_for(
            "ginka_session_start",
            &json!({
                "workspace": "comet/harbor",
                "agent": "codex",
                "prompt": "go",
                "model": "gpt-next",
                "reasoning_effort": "high",
                "service_tier": "priority"
            }),
        )
        .unwrap();
        assert!(matches!(
            request,
            Request::StartSession {
                reasoning_effort: Some(ref effort),
                service_tier: Some(ref tier),
                ..
            } if effort == "high" && tier == "priority"
        ));
    }

    #[test]
    fn existing_session_options_cross_the_mcp_boundary() {
        let request = request_for(
            "ginka_session_options",
            &json!({
                "session": "session-1",
                "model": "gpt-next",
                "reasoning_effort": "high",
                "service_tier": "priority"
            }),
        )
        .unwrap();
        assert!(matches!(
            request,
            Request::UpdateSessionOptions {
                model: Some(ref model),
                reasoning_effort: Some(ref effort),
                service_tier: Some(ref tier),
                ..
            } if model == "gpt-next" && effort == "high" && tier == "priority"
        ));
    }

    #[test]
    fn an_interaction_response_crosses_the_mcp_boundary() {
        let request = request_for(
            "ginka_session_respond",
            &json!({
                "session": "session-1",
                "request_id": "ask-1",
                "response": "SQLite"
            }),
        )
        .unwrap();
        assert!(matches!(
            request,
            Request::RespondToAgent {
                ref request_id,
                ref response,
                ..
            } if request_id == "ask-1" && response == "SQLite"
        ));
    }

    #[test]
    fn a_file_save_crosses_the_mcp_boundary_with_its_revision() {
        let request = request_for(
            "ginka_write_file",
            &json!({
                "workspace": "comet/harbor",
                "path": "src/main.rs",
                "text": "fn main() {}\n",
                "expected_revision": "abc123"
            }),
        )
        .unwrap();
        assert!(matches!(
            request,
            Request::WriteFile {
                ref expected_revision,
                ..
            } if expected_revision == "abc123"
        ));
    }

    #[test]
    fn the_tool_list_is_the_shape_a_client_reads() {
        let listed = tool_list();
        let tools = listed["tools"].as_array().unwrap();
        assert!(!tools.is_empty());
        for tool in tools {
            assert!(tool["name"].as_str().unwrap().starts_with("ginka_"));
            assert!(!tool["description"].as_str().unwrap().is_empty());
            assert_eq!(tool["inputSchema"]["type"], "object");
        }
    }

    #[test]
    fn a_failed_call_is_a_result_rather_than_a_protocol_error() {
        // The call reached the tool; the spec keeps protocol errors for the
        // messages that never got that far.
        let failure = tool_failure("no such workspace".into());
        assert_eq!(failure["isError"], true);
        assert_eq!(failure["content"][0]["text"], "no such workspace");
    }
}
