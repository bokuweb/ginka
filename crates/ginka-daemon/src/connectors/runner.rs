//! The Slack runner: messages in through the service, events out to the
//! thread.
//!
//! A fourth client of the protocol that happens to live in-process
//! (`docs/connectors.md` §2): everything it does to a session is a
//! [`Request`] on the same handler the CLI and the window use, and
//! everything it learns about a session arrives on the same event stream.
//! What it adds is the thread-to-session bookkeeping, the delivery ledger,
//! and the two loops that drive them.

use super::slack::{Envelope, SlackApi, SocketMode, parse_message};
use crate::hub::Hub;
use anyhow::{Context, Result, anyhow};
use ginka_core::Paths;
use ginka_core::connector::config::{Binding, Progress, SlackSettings, WorktreeMode};
use ginka_core::connector::decide::{Decision, IgnoreReason, Inbound, Lookup, RateLimiter, decide};
use ginka_core::connector::deliver::{FooterText, ThreadContext, apply};
use ginka_core::connector::fold::{Outbound, TurnState};
use ginka_core::connector::prompt::{MAX_QUOTED, PromptParts, Quoted, compose};
use ginka_core::connector::secrets;
use ginka_core::connector::text::{Control, Names};
use ginka_core::connector::transport::Glyph;
use ginka_core::connector::{ConnectorControl, ConnectorsSettings, SLACK, ledger};
use ginka_core::service::{EventSink, Service};
use ginka_core::settings::DaemonSettings;
use ginka_protocol::event::DaemonEvent;
use ginka_protocol::model::{
    ChangeSource, ConnectorState, SessionOrigin, SessionState, TranscriptPayload,
};
use ginka_protocol::rpc::{Request, Response};
use ginka_protocol::{ProjectName, SessionId, WorkspaceId};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};

/// One turn the runner is carrying back to a thread.
struct Follow {
    context: ThreadContext,
    turn: TurnState,
    /// Index into the settings' bindings, for the concurrency count.
    binding: usize,
    workspace: WorkspaceId,
}

/// A message waiting behind a binding's concurrency cap.
struct Pending {
    inbound: Inbound,
    binding: usize,
    quote_thread: bool,
}

/// Everything the loops and the control handle share.
struct Inner {
    settings: RwLock<SlackSettings>,
    /// What is wrong with the settings, if anything. Non-empty means the
    /// socket is not run.
    problems: RwLock<Vec<String>>,
    /// Why the socket is not running when it is not the settings: no
    /// tokens, or a token file that is half written.
    not_running: RwLock<Option<String>>,
    api: Arc<SlackApi>,
    socket: Arc<SocketMode>,
    bot_user: RwLock<Option<String>>,
    hub: Arc<Hub>,
    service: Arc<Mutex<Service>>,
    /// The runner's own connection, for the ledger.
    conn: ledger::Ledger,
    following: Mutex<HashMap<SessionId, Follow>>,
    waiting: Mutex<VecDeque<Pending>>,
    /// Live turns per binding index.
    live: Mutex<HashMap<usize, usize>>,
    limiter: Mutex<RateLimiter>,
}

/// The Slack connector as the daemon holds it.
pub struct SlackConnector {
    inner: Arc<Inner>,
}

impl SlackConnector {
    /// Build the connector and, when it can run, start it.
    ///
    /// It can run when the settings validate and both tokens are found.
    /// Otherwise it is still registered, so `ginka slack status` can say
    /// what is missing rather than "not found".
    pub fn start(
        paths: &Paths,
        settings: &DaemonSettings,
        hub: Arc<Hub>,
        service: Arc<Mutex<Service>>,
    ) -> Result<Arc<Self>> {
        let slack = settings.connectors.slack.clone().unwrap_or_default();
        let problems = slack.validate();
        let tokens = match secrets::slack_tokens(&paths.slack_secrets()) {
            Ok(Some(tokens)) => Ok(tokens),
            Ok(None) => Err(format!(
                "no tokens: put {} and {} in {} or the environment",
                secrets::BOT_TOKEN_VAR,
                secrets::APP_TOKEN_VAR,
                paths.slack_secrets().display()
            )),
            Err(error) => Err(error.to_string()),
        };
        let bot_token = tokens
            .as_ref()
            .map(|tokens| tokens.bot.clone())
            .unwrap_or_default();

        let weak: Arc<OnceLock<Weak<Inner>>> = Arc::new(OnceLock::new());
        let socket = {
            let weak = weak.clone();
            Arc::new(SocketMode::new(Box::new(move || {
                if let Some(inner) = weak.get().and_then(Weak::upgrade) {
                    inner.push_state();
                }
            })))
        };
        let conn = ledger::Ledger::open(&paths.database())?;
        let inner = Arc::new(Inner {
            settings: RwLock::new(slack.clone()),
            problems: RwLock::new(problems.clone()),
            not_running: RwLock::new(tokens.as_ref().err().cloned()),
            api: Arc::new(SlackApi::new(bot_token)),
            socket,
            bot_user: RwLock::new(None),
            hub,
            service,
            conn,
            following: Mutex::new(HashMap::new()),
            waiting: Mutex::new(VecDeque::new()),
            live: Mutex::new(HashMap::new()),
            limiter: Mutex::new(RateLimiter::default()),
        });
        let _ = weak.set(Arc::downgrade(&inner));

        if let Some(mode) = secrets::is_too_open(&paths.slack_secrets())
            && mode
        {
            tracing::warn!(
                path = %paths.slack_secrets().display(),
                "the Slack token file is readable by others; chmod 600 it"
            );
        }

        for problem in &problems {
            tracing::warn!(problem, "the Slack connector cannot run");
        }
        match tokens {
            Ok(tokens) if slack.enabled && problems.is_empty() => inner.clone().run(tokens.app),
            Ok(_) => tracing::info!("the Slack connector is configured but not running"),
            Err(why) => tracing::info!(why, "the Slack connector is not running"),
        }
        Ok(Arc::new(Self { inner }))
    }
}

impl ConnectorControl for SlackConnector {
    fn id(&self) -> &'static str {
        SLACK
    }

    fn state(&self) -> ConnectorState {
        self.inner.state()
    }

    fn reload(&self, settings: &ConnectorsSettings) {
        let slack = settings.slack.clone().unwrap_or_default();
        *self.inner.problems.write().unwrap() = slack.validate();
        *self.inner.settings.write().unwrap() = slack;
    }

    fn test(&self, channel: &str) -> Result<()> {
        let posted = self.inner.api.call(
            "chat.postMessage",
            serde_json::json!({
                "channel": channel,
                "text": "Ginka can post here. This message will be removed.",
            }),
        )?;
        let ts = posted
            .get("ts")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow!("chat.postMessage answered with no ts"))?;
        self.inner.api.call(
            "chat.delete",
            serde_json::json!({ "channel": channel, "ts": ts }),
        )?;
        Ok(())
    }
}

impl Inner {
    /// Start the socket thread and the two loops.
    fn run(self: Arc<Self>, app_token: String) {
        let (sender, receiver) = async_channel::bounded::<Envelope>(256);

        // The socket thread: who the bot is first, then the connection.
        {
            let inner = self.clone();
            std::thread::Builder::new()
                .name("ginka-slack".into())
                .spawn(move || {
                    let mut backoff = std::time::Duration::from_secs(1);
                    loop {
                        if inner.socket.stop.load(std::sync::atomic::Ordering::SeqCst) {
                            return;
                        }
                        match inner.api.auth_test() {
                            Ok((user, team)) => {
                                tracing::info!(team, "slack connector signed in");
                                *inner.bot_user.write().unwrap() = Some(user);
                                break;
                            }
                            Err(error) => {
                                inner.socket.status.lock().unwrap().last_error =
                                    Some(error.to_string());
                                inner.push_state();
                                std::thread::sleep(backoff);
                                backoff = (backoff * 2).min(std::time::Duration::from_secs(60));
                            }
                        }
                    }
                    inner.redeliver();
                    inner
                        .socket
                        .clone()
                        .run(inner.api.clone(), app_token, sender);
                })
                .expect("a thread for the slack socket");
        }

        // Inbound: one envelope at a time, on the blocking pool.
        {
            let inner = self.clone();
            smol::spawn(async move {
                while let Ok(envelope) = receiver.recv().await {
                    let inner = inner.clone();
                    smol::unblock(move || inner.handle(envelope)).await;
                }
            })
            .detach();
        }

        // Outbound: the daemon's own event stream, filtered to followed
        // sessions.
        {
            let inner = self.clone();
            let events = self.hub.subscribe();
            smol::spawn(async move {
                while let Some(entry) = events.next().await {
                    let inner = inner.clone();
                    smol::unblock(move || inner.on_daemon_event(entry.event)).await;
                }
            })
            .detach();
        }
    }

    /// What a client sees.
    fn state(&self) -> ConnectorState {
        let settings = self.settings.read().unwrap();
        let socket = self.socket.status.lock().unwrap().clone();
        let problems = self.problems.read().unwrap();
        let mut last_error = None;
        if !problems.is_empty() {
            last_error = Some(problems.join("; "));
        } else if let Some(why) = self.not_running.read().unwrap().as_ref() {
            last_error = Some(why.clone());
        } else if let Some(error) = socket.last_error {
            last_error = Some(error);
        } else if socket.mentions > 0 && socket.messages == 0 {
            last_error = Some(
                "mentions arrive but no messages do: the app needs the message.channels and \
                 message.groups event subscriptions"
                    .to_string(),
            );
        }
        ConnectorState {
            id: SLACK.to_string(),
            enabled: settings.enabled,
            connected: socket.connected,
            since: socket.since,
            last_error,
            bindings: settings.bindings.iter().map(Binding::to_wire).collect(),
        }
    }

    fn push_state(&self) {
        self.hub.emit(DaemonEvent::ConnectorStateChanged {
            state: self.state(),
        });
    }

    fn ask(&self, request: Request) -> Result<Response> {
        let mut service = self.service.lock().unwrap_or_else(|e| e.into_inner());
        service
            .handle(request)
            .map_err(|error| anyhow!("{}", error.message))
    }

    fn now() -> i64 {
        unix_now()
    }

    // -- inbound -----------------------------------------------------------

    fn handle(&self, envelope: Envelope) {
        let Some(bot_user) = self.bot_user.read().unwrap().clone() else {
            return;
        };
        let Some(inbound) = parse_message(&envelope.event, &bot_user, self.api.as_ref()) else {
            return;
        };
        let settings = self.settings.read().unwrap().clone();
        let decision = decide(&inbound, &settings, &View { inner: self });
        tracing::debug!(
            ?decision,
            channel = inbound.channel,
            message = inbound.message,
            "slack message"
        );
        if let Decision::Ignore(reason) = decision {
            if reason == IgnoreReason::NotApprover {
                self.note(&inbound, "Only an approver can answer that.");
            }
            return;
        }
        let fresh = self.conn.with(|conn| {
            ledger::mark_seen(conn, SLACK, &inbound.channel, &inbound.message, Self::now())
                .unwrap_or(true)
        });
        if !fresh {
            return;
        }
        if let Err(error) = self.act(inbound.clone(), decision, &settings) {
            tracing::warn!(%error, "could not act on a slack message");
            self.react(&inbound, Glyph::Failed);
            self.note(&inbound, &format!("Could not do that: {error}"));
        }
    }

    fn act(&self, inbound: Inbound, decision: Decision, settings: &SlackSettings) -> Result<()> {
        match decision {
            Decision::Ignore(_) => Ok(()),
            Decision::Start {
                binding,
                quote_thread,
            } => {
                if !self.admit(&inbound.sender, settings) {
                    self.note(
                        &inbound,
                        &format!(
                            "You have used your {} turns for this hour.",
                            settings.turns_per_hour
                        ),
                    );
                    return Ok(());
                }
                let cap = settings.bindings[binding].max_concurrent as usize;
                let busy = *self.live.lock().unwrap().get(&binding).unwrap_or(&0);
                if busy >= cap {
                    self.react(&inbound, Glyph::Queued);
                    self.waiting.lock().unwrap().push_back(Pending {
                        inbound,
                        binding,
                        quote_thread,
                    });
                    return Ok(());
                }
                self.start_turn(inbound, binding, quote_thread, settings)
            }
            Decision::Continue { session } => {
                if !self.admit(&inbound.sender, settings) {
                    self.note(
                        &inbound,
                        &format!(
                            "You have used your {} turns for this hour.",
                            settings.turns_per_hour
                        ),
                    );
                    return Ok(());
                }
                let text = self.prompt_for(&inbound, false)?;
                self.ask(Request::SendMessage {
                    session: session.clone(),
                    text,
                })?;
                let mut following = self.following.lock().unwrap();
                match following.get_mut(&session) {
                    Some(follow) => {
                        // The latest message carries the status.
                        let old =
                            std::mem::replace(&mut follow.context.trigger, inbound.message.clone());
                        self.with_transport(|transport| {
                            let _ = transport.unreact(&inbound.channel, &old, Glyph::Working);
                            let _ =
                                transport.react(&inbound.channel, &inbound.message, Glyph::Working);
                        });
                    }
                    None => {
                        let (binding, workspace) =
                            self.binding_and_workspace(&session, settings)?;
                        let follow = self.follow(&inbound, binding, workspace, settings);
                        let begin = follow.turn.begin();
                        following.insert(session.clone(), follow);
                        drop(following);
                        self.deliver(&session, begin);
                    }
                }
                Ok(())
            }
            Decision::Control { session, command } => match command {
                Control::Stop => {
                    self.ask(Request::CancelSession { session })?;
                    self.react(&inbound, Glyph::Stopped);
                    Ok(())
                }
                Control::Status => {
                    let line = self.status_line(&session)?;
                    self.note(&inbound, &line);
                    Ok(())
                }
                Control::New => {
                    self.ask(Request::CloseSessionOrigin {
                        session: session.clone(),
                    })?;
                    self.following.lock().unwrap().remove(&session);
                    self.react(&inbound, Glyph::Done);
                    self.note(
                        &inbound,
                        "The next message in this thread starts a new session.",
                    );
                    Ok(())
                }
            },
            Decision::Verdict {
                session,
                request_id,
                answer,
            } => {
                let agent_request_id = self
                    .following
                    .lock()
                    .unwrap()
                    .get(&session)
                    .and_then(|follow| follow.turn.agent_request_id(&request_id))
                    .unwrap_or(&request_id)
                    .to_string();
                self.ask(Request::RespondToAgent {
                    session: session.clone(),
                    request_id: agent_request_id,
                    response: answer.as_response(),
                })?;
                if let Some(follow) = self.following.lock().unwrap().get_mut(&session) {
                    follow.turn.close_request(&request_id);
                }
                self.react(&inbound, Glyph::Done);
                Ok(())
            }
        }
    }

    fn admit(&self, sender: &str, settings: &SlackSettings) -> bool {
        self.limiter
            .lock()
            .unwrap()
            .admit(sender, Self::now(), settings.turns_per_hour)
    }

    /// Start a session for a root message, or a mention into a thread.
    fn start_turn(
        &self,
        inbound: Inbound,
        binding_index: usize,
        quote_thread: bool,
        settings: &SlackSettings,
    ) -> Result<()> {
        let binding = settings.bindings[binding_index].clone();
        let workspace = self.workspace_for(&binding, &inbound)?;
        let prompt = self.prompt_for(&inbound, quote_thread)?;
        let session = match self.ask(Request::StartSession {
            workspace: workspace.clone(),
            agent: binding.agent.clone(),
            prompt,
            model: binding.model.clone(),
            reasoning_effort: None,
            service_tier: None,
            account: binding.account.clone(),
            access_mode: Some(binding.access_mode),
            origin: Some(inbound.origin()),
        })? {
            Response::Session { session } => session,
            other => anyhow::bail!("unexpected answer to a start: {other:?}"),
        };
        *self.live.lock().unwrap().entry(binding_index).or_insert(0) += 1;
        let follow = self.follow(&inbound, binding_index, workspace, settings);
        let begin = follow.turn.begin();
        self.following
            .lock()
            .unwrap()
            .insert(session.id.clone(), follow);
        self.deliver(&session.id, begin);
        Ok(())
    }

    fn follow(
        &self,
        inbound: &Inbound,
        binding: usize,
        workspace: WorkspaceId,
        settings: &SlackSettings,
    ) -> Follow {
        let binding_settings = &settings.bindings[binding];
        Follow {
            context: ThreadContext {
                channel: inbound.channel.clone(),
                thread: inbound.thread.clone(),
                trigger: inbound.message.clone(),
                progress: None,
            },
            turn: TurnState::new(
                binding_settings.progress == Progress::Edit,
                binding_settings.cleanup_progress,
            ),
            binding,
            workspace,
        }
    }

    /// The binding and workspace of a session the runner did not start in
    /// this daemon run: a reply in a thread from before a restart.
    fn binding_and_workspace(
        &self,
        session: &SessionId,
        settings: &SlackSettings,
    ) -> Result<(usize, WorkspaceId)> {
        let sessions = match self.ask(Request::ListSessions {
            workspace: None,
            origin: None,
        })? {
            Response::Sessions { sessions } => sessions,
            other => anyhow::bail!("unexpected answer: {other:?}"),
        };
        let stored = sessions
            .into_iter()
            .find(|stored| &stored.id == session)
            .ok_or_else(|| anyhow!("session {session} is gone"))?;
        let channel = stored
            .origin
            .as_ref()
            .map(|origin| origin.channel.clone())
            .unwrap_or_default();
        let (index, _) = settings
            .binding_for(&channel)
            .ok_or_else(|| anyhow!("channel {channel} is no longer bound"))?;
        Ok((index, stored.workspace))
    }

    /// Where a binding's conversation runs, making the worktree when the
    /// binding wants one per thread.
    fn workspace_for(&self, binding: &Binding, inbound: &Inbound) -> Result<WorkspaceId> {
        if let Some(workspace) = &binding.workspace {
            return Ok(workspace.clone());
        }
        let project = binding
            .project
            .clone()
            .ok_or_else(|| anyhow!("the binding names nowhere to run"))?;
        match binding.worktree {
            WorktreeMode::Shared => self.project_checkout(&project),
            WorktreeMode::PerThread => {
                let branch = format!("slack-{}", inbound.thread.replace('.', "-"));
                match self.ask(Request::CreateWorkspace {
                    project: project.clone(),
                    branch: branch.clone(),
                    base: None,
                }) {
                    Ok(Response::Workspace { workspace }) => Ok(workspace.worktree.workspace_id()),
                    Ok(other) => anyhow::bail!("unexpected answer: {other:?}"),
                    // The thread was told to start fresh: its worktree exists.
                    Err(_) => self.workspace_on_branch(&project, &branch),
                }
            }
        }
    }

    /// The project's own checkout: the worktree at the project's path, or
    /// the first one there is.
    fn project_checkout(&self, project: &ProjectName) -> Result<WorkspaceId> {
        let path = match self.ask(Request::ListProjects)? {
            Response::Projects { projects } => projects
                .into_iter()
                .find(|found| &found.name == project)
                .map(|found| found.path),
            _ => None,
        };
        let workspaces = match self.ask(Request::ListWorkspaces {
            project: Some(project.clone()),
        })? {
            Response::Workspaces { workspaces } => workspaces,
            other => anyhow::bail!("unexpected answer: {other:?}"),
        };
        workspaces
            .iter()
            .find(|summary| path.as_ref() == Some(&summary.worktree.path))
            .or_else(|| workspaces.first())
            .map(|summary| summary.worktree.workspace_id())
            .ok_or_else(|| anyhow!("project {project} has no worktree to run in"))
    }

    fn workspace_on_branch(&self, project: &ProjectName, branch: &str) -> Result<WorkspaceId> {
        match self.ask(Request::ListWorkspaces {
            project: Some(project.clone()),
        })? {
            Response::Workspaces { workspaces } => workspaces
                .iter()
                .find(|summary| summary.worktree.branch == branch)
                .map(|summary| summary.worktree.workspace_id())
                .ok_or_else(|| anyhow!("could not make or find a worktree on {branch}")),
            other => anyhow::bail!("unexpected answer: {other:?}"),
        }
    }

    /// The prompt for a message: attributed, with its files stored and,
    /// when asked, the thread so far quoted.
    fn prompt_for(&self, inbound: &Inbound, quote_thread: bool) -> Result<String> {
        let mut attachments = Vec::new();
        for file in &inbound.files {
            let bytes = self
                .api
                .download(&file.url)
                .with_context(|| format!("fetching {}", file.name))?;
            use base64::Engine as _;
            match self.ask(Request::UploadAttachment {
                name: file.name.clone(),
                data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
            })? {
                Response::Attachment { attachment } => attachments.push(attachment.reference),
                other => anyhow::bail!("unexpected answer: {other:?}"),
            }
        }
        let mut quoted = Vec::new();
        if quote_thread {
            for (user, text) in self
                .api
                .thread_replies(&inbound.channel, &inbound.thread, MAX_QUOTED + 1)
                .unwrap_or_default()
            {
                quoted.push(Quoted {
                    who: self.api.user_name(&user).unwrap_or(user),
                    text: ginka_core::connector::text::unescape(&text, self.api.as_ref()),
                });
            }
            // The message that asked is the request, not context.
            quoted.retain(|message| message.text != inbound.text);
        }
        Ok(compose(&PromptParts {
            who: self
                .api
                .user_name(&inbound.sender)
                .unwrap_or_else(|| inbound.sender.clone()),
            channel: self.api.channel_name(&inbound.channel),
            text: inbound.text.clone(),
            attachments,
            quoted,
        }))
    }

    fn status_line(&self, session: &SessionId) -> Result<String> {
        let sessions = match self.ask(Request::ListSessions {
            workspace: None,
            origin: None,
        })? {
            Response::Sessions { sessions } => sessions,
            other => anyhow::bail!("unexpected answer: {other:?}"),
        };
        let stored = sessions
            .into_iter()
            .find(|stored| &stored.id == session)
            .ok_or_else(|| anyhow!("session {session} is gone"))?;
        Ok(format!(
            "{} · {} · workspace {}{}",
            stored.agent,
            stored.state.as_str(),
            stored.workspace,
            stored
                .summary
                .map(|summary| format!(" · {summary}"))
                .unwrap_or_default()
        ))
    }

    // -- outbound ----------------------------------------------------------

    fn on_daemon_event(&self, event: DaemonEvent) {
        match event {
            DaemonEvent::SessionEvent { session, entry } => {
                let TranscriptPayload::Agent { event } = entry.payload else {
                    return;
                };
                let outs = {
                    let mut following = self.following.lock().unwrap();
                    let Some(follow) = following.get_mut(&session) else {
                        return;
                    };
                    follow.turn.on_event(&event, Self::now())
                };
                self.deliver(&session, outs);
                self.finish_if_done(&session);
            }
            DaemonEvent::SessionStateChanged { session, state } => {
                let summary = if state == SessionState::Failed {
                    self.status_line(&session).ok()
                } else {
                    None
                };
                let outs = {
                    let mut following = self.following.lock().unwrap();
                    let Some(follow) = following.get_mut(&session) else {
                        return;
                    };
                    follow.turn.on_state(state, summary.as_deref())
                };
                self.deliver(&session, outs);
                self.finish_if_done(&session);
            }
            _ => {}
        }
    }

    /// Send a followed turn's outbounds, through the ledger where it
    /// matters.
    fn deliver(&self, session: &SessionId, outs: Vec<Outbound>) {
        for out in outs {
            let (mut context, workspace) = {
                let following = self.following.lock().unwrap();
                let Some(follow) = following.get(session) else {
                    return;
                };
                (follow.context.clone(), follow.workspace.clone())
            };
            let footer = match &out {
                Outbound::Footer { turn } => Some(self.footer(&workspace, *turn)),
                _ => None,
            };
            let ledgered = matches!(
                out,
                Outbound::Reply { .. } | Outbound::Note { .. } | Outbound::Question { .. }
            );
            let row = if ledgered {
                self.conn.with(|conn| {
                    ledger::record(
                        conn,
                        SLACK,
                        &context.channel,
                        &context.thread,
                        &serde_json::to_string(&out).unwrap_or_default(),
                        Self::now(),
                    )
                    .ok()
                })
            } else {
                None
            };
            let result = self
                .with_transport(|transport| apply(transport, &mut context, &out, footer.as_ref()));
            match result {
                Ok(()) => {
                    if let Some(id) = row {
                        self.conn
                            .with(|conn| ledger::delivered(conn, id, Self::now()).ok());
                    }
                }
                Err(error) => tracing::warn!(%error, ?out, "could not post to slack"),
            }
            if let Some(follow) = self.following.lock().unwrap().get_mut(session) {
                follow.context.progress = context.progress;
            }
        }
    }

    fn with_transport<T>(
        &self,
        run: impl FnOnce(&mut dyn ginka_core::connector::ChatTransport) -> T,
    ) -> T {
        // The API is shared and `ChatTransport` takes `&mut self` for the
        // scripted double's sake; the Slack client keeps no per-call state,
        // so a short-lived clone of the handle is a transport of its own.
        let mut handle = ApiHandle(self.api.clone());
        run(&mut handle)
    }

    fn footer(&self, workspace: &WorkspaceId, turn: u32) -> FooterText {
        let changed_files = match self.ask(Request::WorkspaceChanges {
            workspace: workspace.clone(),
            source: ChangeSource::Uncommitted,
            context_lines: None,
        }) {
            Ok(Response::Changes { changes }) => changes.files.len(),
            _ => 0,
        };
        FooterText {
            changed_files,
            checkpoint_turn: turn,
            workspace: workspace.0.clone(),
        }
    }

    fn finish_if_done(&self, session: &SessionId) {
        let binding = {
            let mut following = self.following.lock().unwrap();
            let done = following
                .get(session)
                .is_some_and(|follow| follow.turn.is_done());
            if !done {
                return;
            }
            following.remove(session).map(|follow| follow.binding)
        };
        let Some(binding) = binding else { return };
        {
            let mut live = self.live.lock().unwrap();
            let count = live.entry(binding).or_insert(1);
            *count = count.saturating_sub(1);
        }
        // Whoever was waiting for this binding goes next.
        let next = {
            let mut waiting = self.waiting.lock().unwrap();
            let position = waiting
                .iter()
                .position(|pending| pending.binding == binding);
            position.and_then(|at| waiting.remove(at))
        };
        if let Some(pending) = next {
            let settings = self.settings.read().unwrap().clone();
            if let Err(error) = self.act(
                pending.inbound.clone(),
                Decision::Start {
                    binding: pending.binding,
                    quote_thread: pending.quote_thread,
                },
                &settings,
            ) {
                tracing::warn!(%error, "could not start a queued turn");
                self.note(&pending.inbound, &format!("Could not start that: {error}"));
            }
        }
    }

    /// Post what an earlier daemon run owed and never sent.
    fn redeliver(&self) {
        let owed = self.conn.with(|conn| {
            let _ = ledger::prune_deliveries(conn, Self::now());
            ledger::undelivered(conn, SLACK, Self::now()).unwrap_or_default()
        });
        for delivery in owed {
            let Ok(out) = serde_json::from_str::<Outbound>(&delivery.payload) else {
                continue;
            };
            let out = match out {
                Outbound::Reply { markdown } => Outbound::Reply {
                    markdown: format!(
                        "_Recovered reply, may repeat an earlier one._\n\n{markdown}"
                    ),
                },
                Outbound::Note { text } => Outbound::Note {
                    text: format!("Recovered reply, may repeat an earlier one: {text}"),
                },
                other => other,
            };
            let mut context = ThreadContext {
                channel: delivery.channel.clone(),
                thread: delivery.thread.clone(),
                trigger: String::new(),
                progress: None,
            };
            let result =
                self.with_transport(|transport| apply(transport, &mut context, &out, None));
            match result {
                Ok(()) => {
                    self.conn
                        .with(|conn| ledger::delivered(conn, delivery.id, Self::now()).ok());
                }
                Err(error) => {
                    tracing::warn!(%error, "could not recover a reply");
                    self.conn
                        .with(|conn| ledger::attempted(conn, delivery.id).ok());
                }
            }
        }
    }

    fn react(&self, inbound: &Inbound, glyph: Glyph) {
        self.with_transport(|transport| {
            if let Err(error) = transport.react(&inbound.channel, &inbound.message, glyph) {
                tracing::debug!(%error, "could not react");
            }
        });
    }

    fn note(&self, inbound: &Inbound, text: &str) {
        self.with_transport(|transport| {
            if let Err(error) = transport.post(&inbound.channel, &inbound.thread, text) {
                tracing::debug!(%error, "could not post a note");
            }
        });
    }
}

/// A shared API client as a transport.
struct ApiHandle(Arc<SlackApi>);

impl ginka_core::connector::ChatTransport for ApiHandle {
    fn post(&mut self, channel: &str, thread: &str, text: &str) -> Result<String> {
        SlackApi::post_message(&self.0, channel, thread, text)
    }
    fn edit(&mut self, channel: &str, message: &str, text: &str) -> Result<()> {
        SlackApi::update_message(&self.0, channel, message, text)
    }
    fn delete(&mut self, channel: &str, message: &str) -> Result<()> {
        SlackApi::delete_message(&self.0, channel, message)
    }
    fn react(&mut self, channel: &str, message: &str, glyph: Glyph) -> Result<()> {
        SlackApi::add_reaction(&self.0, channel, message, glyph)
    }
    fn unreact(&mut self, channel: &str, message: &str, glyph: Glyph) -> Result<()> {
        SlackApi::remove_reaction(&self.0, channel, message, glyph)
    }
    fn upload(&mut self, channel: &str, thread: &str, name: &str, bytes: &[u8]) -> Result<()> {
        SlackApi::upload_file(&self.0, channel, thread, name, bytes)
    }
}

/// The policy's view of the world, answered through the service.
struct View<'a> {
    inner: &'a Inner,
}

impl Lookup for View<'_> {
    fn session_for(&self, origin: &SessionOrigin) -> Option<SessionId> {
        match self.inner.ask(Request::ListSessions {
            workspace: None,
            origin: Some(origin.clone()),
        }) {
            Ok(Response::Sessions { sessions }) => sessions.into_iter().next().map(|s| s.id),
            _ => None,
        }
    }

    fn open_requests(&self, session: &SessionId) -> Vec<String> {
        self.inner
            .following
            .lock()
            .unwrap()
            .get(session)
            .map(|follow| follow.turn.open_requests().to_vec())
            .unwrap_or_default()
    }

    fn seen(&self, channel: &str, message: &str) -> bool {
        self.inner
            .conn
            .with(|conn| ledger::seen(conn, SLACK, channel, message).unwrap_or(false))
    }
}

/// Unix seconds.
fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}
