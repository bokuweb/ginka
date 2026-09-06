//! The Slack adapter: Socket Mode in, the Web API out.
//!
//! Everything Slack-shaped stops here (`docs/connectors.md` §3.2). The
//! connector above it sees [`Inbound`]s and a [`ChatTransport`]; this module
//! sees envelopes, `mrkdwn`, `Retry-After` and `chat.postMessage`.
//!
//! Socket Mode is the only transport: the daemon dials out, so it keeps
//! binding loopback and nothing else. The connection runs on a thread of its
//! own — a blocking socket, like a driver's reader — and hands what it reads
//! to the runner over a channel.

use anyhow::{Context, Result, anyhow, bail};
use ginka_core::connector::decide::{Inbound, InboundFile};
use ginka_core::connector::text::{Names, strip_mention, unescape};
use ginka_core::connector::transport::{ChatTransport, Glyph};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Where the Web API lives.
const API: &str = "https://slack.com/api/";

/// How long one Web API call may take. Long enough for an upload, short
/// enough that a hung call does not hold a turn's reply forever.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// The Web API, with the bot token.
///
/// `Sync` by construction so the runner and the request handler can share
/// one: `ureq::Agent` is a connection pool behind an `Arc`.
pub struct SlackApi {
    agent: ureq::Agent,
    bot_token: String,
    /// Display names by member id, so a thread is quoted with names rather
    /// than ids. Bounded by the number of people who ever speak.
    names: Mutex<HashMap<String, String>>,
    /// Channel names by id, for the prompt's attribution.
    channels: Mutex<HashMap<String, String>>,
}

impl SlackApi {
    /// An API client for a bot token.
    pub fn new(bot_token: String) -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(CALL_TIMEOUT))
            // 429 and 5xx are answers to read, not errors to unwrap.
            .http_status_as_error(false)
            .build();
        Self {
            agent: ureq::Agent::new_with_config(config),
            bot_token,
            names: Mutex::new(HashMap::new()),
            channels: Mutex::new(HashMap::new()),
        }
    }

    /// Call one Web API method with a JSON body.
    ///
    /// A rate limit is honoured once: the call sleeps for `Retry-After` and
    /// tries again. Anything else that is not `ok: true` is an error naming
    /// the method and Slack's reason, which is what a person needs to fix
    /// the app's scopes.
    pub fn call(&self, method: &str, body: Value) -> Result<Value> {
        let mut retried = false;
        loop {
            let mut response = self
                .agent
                .post(&format!("{API}{method}"))
                .header("Authorization", &format!("Bearer {}", self.bot_token))
                .header("Content-Type", "application/json; charset=utf-8")
                .send_json(&body)
                .with_context(|| format!("calling {method}"))?;
            let status = response.status().as_u16();
            if status == 429 && !retried {
                let wait = response
                    .headers()
                    .get("retry-after")
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse::<u64>().ok())
                    .unwrap_or(1)
                    .min(30);
                std::thread::sleep(Duration::from_secs(wait));
                retried = true;
                continue;
            }
            let value: Value = response
                .body_mut()
                .read_json()
                .with_context(|| format!("reading {method}'s answer"))?;
            if value.get("ok").and_then(Value::as_bool) != Some(true) {
                let reason = value
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("no reason given");
                bail!("{method} failed: {reason} (HTTP {status})");
            }
            return Ok(value);
        }
    }

    /// Who the bot is: its member id and the workspace's name.
    pub fn auth_test(&self) -> Result<(String, String)> {
        let value = self.call("auth.test", json!({}))?;
        let user = value
            .get("user_id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("auth.test did not say who the bot is"))?;
        let team = value
            .get("team")
            .and_then(Value::as_str)
            .unwrap_or_default();
        Ok((user.to_string(), team.to_string()))
    }

    /// A Socket Mode URL, from the app-level token.
    ///
    /// The one call made with the app token rather than the bot token.
    pub fn connections_open(&self, app_token: &str) -> Result<String> {
        let mut response = self
            .agent
            .post(&format!("{API}apps.connections.open"))
            .header("Authorization", &format!("Bearer {app_token}"))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .send_empty()
            .context("calling apps.connections.open")?;
        let value: Value = response
            .body_mut()
            .read_json()
            .context("reading apps.connections.open's answer")?;
        if value.get("ok").and_then(Value::as_bool) != Some(true) {
            bail!(
                "apps.connections.open failed: {}",
                value
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("no reason given")
            );
        }
        value
            .get("url")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow!("apps.connections.open answered with no url"))
    }

    /// The replies of a thread, oldest first, as `(member id, text)`.
    pub fn thread_replies(
        &self,
        channel: &str,
        thread: &str,
        limit: usize,
    ) -> Result<Vec<(String, String)>> {
        let value = self.call(
            "conversations.replies",
            json!({ "channel": channel, "ts": thread, "limit": limit }),
        )?;
        let messages = value
            .get("messages")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(messages
            .iter()
            .filter_map(|message| {
                let user = message.get("user").and_then(Value::as_str)?;
                let text = message.get("text").and_then(Value::as_str)?;
                Some((user.to_string(), text.to_string()))
            })
            .collect())
    }

    /// A channel's name, for the prompt. The id when Slack will not say.
    pub fn channel_name(&self, id: &str) -> String {
        if let Some(name) = self.channels.lock().unwrap().get(id) {
            return name.clone();
        }
        let name = self
            .call("conversations.info", json!({ "channel": id }))
            .ok()
            .and_then(|value| {
                value
                    .get("channel")?
                    .get("name")?
                    .as_str()
                    .map(str::to_string)
            })
            .unwrap_or_else(|| id.to_string());
        self.channels
            .lock()
            .unwrap()
            .insert(id.to_string(), name.clone());
        name
    }

    /// Fetch a file the bot may read, with its token.
    pub fn download(&self, url: &str) -> Result<Vec<u8>> {
        let mut response = self
            .agent
            .get(url)
            .header("Authorization", &format!("Bearer {}", self.bot_token))
            .call()
            .with_context(|| format!("downloading {url}"))?;
        if response.status().as_u16() >= 400 {
            bail!("downloading {url}: HTTP {}", response.status());
        }
        Ok(response.body_mut().read_to_vec()?)
    }
}

impl Names for SlackApi {
    /// `users.info`, cached: the display name, else the real name, else the
    /// handle. `None` only when Slack will not say.
    fn user_name(&self, id: &str) -> Option<String> {
        if let Some(name) = self.names.lock().unwrap().get(id) {
            return Some(name.clone());
        }
        let value = self.call("users.info", json!({ "user": id })).ok()?;
        let user = value.get("user")?;
        let profile = user.get("profile");
        let name = profile
            .and_then(|p| p.get("display_name"))
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .or_else(|| {
                profile
                    .and_then(|p| p.get("real_name"))
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty())
            })
            .or_else(|| user.get("name").and_then(Value::as_str))?
            .to_string();
        self.names
            .lock()
            .unwrap()
            .insert(id.to_string(), name.clone());
        Some(name)
    }
}

/// Our reactions in Slack's vocabulary. Mapped once, here.
pub fn glyph_name(glyph: Glyph) -> &'static str {
    match glyph {
        Glyph::Working => "eyes",
        Glyph::Done => "white_check_mark",
        Glyph::Failed => "x",
        Glyph::Waiting => "question",
        Glyph::Stopped => "black_square_for_stop",
        Glyph::Queued => "hourglass_flowing_sand",
    }
}

impl SlackApi {
    /// `chat.postMessage` into a thread. Answers with the new message's ts.
    pub fn post_message(&self, channel: &str, thread: &str, text: &str) -> Result<String> {
        let value = self.call(
            "chat.postMessage",
            json!({ "channel": channel, "thread_ts": thread, "text": text }),
        )?;
        value
            .get("ts")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow!("chat.postMessage answered with no ts"))
    }

    /// `chat.update`.
    pub fn update_message(&self, channel: &str, message: &str, text: &str) -> Result<()> {
        self.call(
            "chat.update",
            json!({ "channel": channel, "ts": message, "text": text }),
        )?;
        Ok(())
    }

    /// `chat.delete`.
    pub fn delete_message(&self, channel: &str, message: &str) -> Result<()> {
        self.call("chat.delete", json!({ "channel": channel, "ts": message }))?;
        Ok(())
    }

    /// `reactions.add`. A reaction that is already there is the state that
    /// was asked for.
    pub fn add_reaction(&self, channel: &str, message: &str, glyph: Glyph) -> Result<()> {
        match self.call(
            "reactions.add",
            json!({ "channel": channel, "timestamp": message, "name": glyph_name(glyph) }),
        ) {
            Ok(_) => Ok(()),
            Err(error) if error.to_string().contains("already_reacted") => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// `reactions.remove`.
    pub fn remove_reaction(&self, channel: &str, message: &str, glyph: Glyph) -> Result<()> {
        match self.call(
            "reactions.remove",
            json!({ "channel": channel, "timestamp": message, "name": glyph_name(glyph) }),
        ) {
            Ok(_) => Ok(()),
            Err(error) if error.to_string().contains("no_reaction") => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// The three-step upload Slack replaced `files.upload` with: ask for a
    /// URL, put the bytes there, then say which thread it belongs to.
    pub fn upload_file(&self, channel: &str, thread: &str, name: &str, bytes: &[u8]) -> Result<()> {
        let ticket = self.call(
            "files.getUploadURLExternal",
            json!({ "filename": name, "length": bytes.len() }),
        )?;
        let url = ticket
            .get("upload_url")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("files.getUploadURLExternal answered with no url"))?;
        let id = ticket
            .get("file_id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("files.getUploadURLExternal answered with no file id"))?;
        let response = self
            .agent
            .post(url)
            .header("Content-Type", "application/octet-stream")
            .send(bytes)
            .context("uploading the file's bytes")?;
        if response.status().as_u16() >= 400 {
            bail!("uploading {name}: HTTP {}", response.status());
        }
        self.call(
            "files.completeUploadExternal",
            json!({
                "files": [{ "id": id, "title": name }],
                "channel_id": channel,
                "thread_ts": thread,
            }),
        )?;
        Ok(())
    }
}

/// The transport, for anything that owns the client outright. The runner
/// shares one behind an `Arc` and goes through the same `&self` methods.
impl ChatTransport for SlackApi {
    fn post(&mut self, channel: &str, thread: &str, text: &str) -> Result<String> {
        self.post_message(channel, thread, text)
    }

    fn edit(&mut self, channel: &str, message: &str, text: &str) -> Result<()> {
        self.update_message(channel, message, text)
    }

    fn delete(&mut self, channel: &str, message: &str) -> Result<()> {
        self.delete_message(channel, message)
    }

    fn react(&mut self, channel: &str, message: &str, glyph: Glyph) -> Result<()> {
        self.add_reaction(channel, message, glyph)
    }

    fn unreact(&mut self, channel: &str, message: &str, glyph: Glyph) -> Result<()> {
        self.remove_reaction(channel, message, glyph)
    }

    fn upload(&mut self, channel: &str, thread: &str, name: &str, bytes: &[u8]) -> Result<()> {
        self.upload_file(channel, thread, name, bytes)
    }
}

// ---------------------------------------------------------------------------
// Socket Mode.

/// What the socket thread reports.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SocketStatus {
    pub connected: bool,
    /// Unix seconds when the current connection came up.
    pub since: Option<i64>,
    /// The last thing that went wrong, kept until the next success.
    pub last_error: Option<String>,
    /// `message` events seen since start, for the setup diagnostic.
    pub messages: u64,
    /// `app_mention` events seen since start. Mentions with no messages
    /// means the `message.*` subscriptions are missing.
    pub mentions: u64,
}

/// One event from the Events API, as the socket thread hands it over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    /// The `event` object of an `events_api` envelope.
    pub event: Value,
}

/// Shared between the socket thread and the runner.
pub struct SocketMode {
    pub status: Mutex<SocketStatus>,
    /// Set to stop the thread at its next read.
    pub stop: AtomicBool,
    /// Called whenever `status` changes, so the daemon can push it.
    on_change: Box<dyn Fn() + Send + Sync>,
}

/// The longest pause between reconnect attempts.
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// How long a read waits before checking whether it should stop.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

impl SocketMode {
    pub fn new(on_change: Box<dyn Fn() + Send + Sync>) -> Self {
        Self {
            status: Mutex::new(SocketStatus::default()),
            stop: AtomicBool::new(false),
            on_change,
        }
    }

    fn update(&self, change: impl FnOnce(&mut SocketStatus)) {
        change(&mut self.status.lock().unwrap());
        (self.on_change)();
    }

    /// Run the connection until told to stop, reconnecting with backoff.
    ///
    /// Blocking: meant for a thread of its own. Every `events_api` envelope
    /// is acknowledged before it is handed over, inside Slack's three-second
    /// window, whatever the runner does with it afterwards.
    pub fn run(
        self: Arc<Self>,
        api: Arc<SlackApi>,
        app_token: String,
        events: async_channel::Sender<Envelope>,
    ) {
        let mut backoff = Duration::from_secs(1);
        while !self.stop.load(Ordering::SeqCst) {
            match self.session(&api, &app_token, &events) {
                Ok(()) => backoff = Duration::from_secs(1),
                Err(error) => {
                    tracing::warn!(%error, "slack socket dropped");
                    self.update(|status| {
                        status.connected = false;
                        status.since = None;
                        status.last_error = Some(error.to_string());
                    });
                }
            }
            if self.stop.load(Ordering::SeqCst) {
                break;
            }
            // Jittered so a fleet of daemons does not reconnect in step.
            let jitter = Duration::from_millis(u64::from(uuid::Uuid::new_v4().as_bytes()[0]) * 4);
            std::thread::sleep(backoff + jitter);
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
        self.update(|status| {
            status.connected = false;
            status.since = None;
        });
    }

    /// One connection, from open to close.
    fn session(
        &self,
        api: &SlackApi,
        app_token: &str,
        events: &async_channel::Sender<Envelope>,
    ) -> Result<()> {
        let url = api.connections_open(app_token)?;
        let (mut socket, _) = tungstenite::connect(&url).context("connecting to Socket Mode")?;
        set_read_timeout(&mut socket, READ_TIMEOUT);
        self.update(|status| {
            status.connected = true;
            status.since = Some(now());
            status.last_error = None;
        });
        tracing::info!("slack socket connected");

        loop {
            if self.stop.load(Ordering::SeqCst) {
                let _ = socket.close(None);
                return Ok(());
            }
            let message = match socket.read() {
                Ok(message) => message,
                Err(tungstenite::Error::Io(error))
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let text = match message {
                tungstenite::Message::Text(text) => text.to_string(),
                tungstenite::Message::Close(_) => bail!("slack closed the socket"),
                _ => continue,
            };
            let value: Value = match serde_json::from_str(&text) {
                Ok(value) => value,
                Err(error) => {
                    tracing::debug!(%error, "an envelope would not parse");
                    continue;
                }
            };
            // The ack first, whatever the envelope is: Slack redelivers
            // anything not acknowledged within three seconds, and the
            // runner's work is not on that clock.
            if let Some(id) = value.get("envelope_id").and_then(Value::as_str) {
                socket
                    .send(tungstenite::Message::Text(
                        json!({ "envelope_id": id }).to_string().into(),
                    ))
                    .context("acknowledging an envelope")?;
            }
            match value.get("type").and_then(Value::as_str) {
                Some("events_api") => {
                    let Some(event) = value.pointer("/payload/event").cloned() else {
                        continue;
                    };
                    let kind = event
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    self.update(|status| match kind {
                        "message" => status.messages += 1,
                        "app_mention" => status.mentions += 1,
                        _ => {}
                    });
                    if events.try_send(Envelope { event }).is_err() {
                        tracing::warn!("the runner is not reading; dropping an event");
                    }
                }
                Some("disconnect") => {
                    // Slack asks nicely before it goes; a fresh URL is wanted.
                    let _ = socket.close(None);
                    return Ok(());
                }
                _ => {}
            }
        }
    }
}

fn set_read_timeout(
    socket: &mut tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
    timeout: Duration,
) {
    use tungstenite::stream::MaybeTlsStream;
    let stream = match socket.get_mut() {
        MaybeTlsStream::Plain(stream) => stream,
        MaybeTlsStream::Rustls(stream) => stream.get_mut(),
        _ => return,
    };
    let _ = stream.set_read_timeout(Some(timeout));
}

/// Unix seconds.
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Events.

/// Read a `message` event into what the policy needs.
///
/// `None` for events that are not messages at all — `app_mention` is the
/// same message delivered a second time under another name, and is counted
/// by the socket thread rather than handled here. Edits and bot posts are
/// still returned, flagged, so the policy can say why it ignored them.
pub fn parse_message(event: &Value, bot_user: &str, names: &dyn Names) -> Option<Inbound> {
    if event.get("type").and_then(Value::as_str) != Some("message") {
        return None;
    }
    let channel = event.get("channel").and_then(Value::as_str)?.to_string();
    let subtype = event
        .get("subtype")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let is_edit = matches!(subtype, "message_changed" | "message_deleted");
    // An edit carries the message inside `message`; the outer object is the
    // edit. The ts of the original is what a dedup key wants.
    let body = if is_edit {
        event.get("message").unwrap_or(event)
    } else {
        event
    };
    let ts = body
        .get("ts")
        .or_else(|| event.get("ts"))
        .and_then(Value::as_str)?
        .to_string();
    let thread_ts = body
        .get("thread_ts")
        .and_then(Value::as_str)
        .map(str::to_string);
    let user = body
        .get("user")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let from_bot = body.get("bot_id").is_some() || subtype == "bot_message" || user == bot_user;
    let raw = body.get("text").and_then(Value::as_str).unwrap_or_default();
    let (stripped, mentions_bot) = strip_mention(raw, bot_user);
    let text = unescape(&stripped, names);
    let files = body
        .get("files")
        .and_then(Value::as_array)
        .map(|files| {
            files
                .iter()
                .filter_map(|file| {
                    Some(InboundFile {
                        id: file.get("id")?.as_str()?.to_string(),
                        name: file
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("file")
                            .to_string(),
                        url: file.get("url_private_download")?.as_str()?.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let is_root = thread_ts.as_deref().is_none_or(|thread| thread == ts);
    Some(Inbound {
        connector: ginka_core::connector::SLACK.to_string(),
        channel,
        thread: thread_ts.unwrap_or_else(|| ts.clone()),
        message: ts,
        sender: user,
        text,
        mentions_bot,
        is_root,
        files,
        is_edit,
        from_bot,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ginka_core::connector::text::NoNames;

    #[test]
    fn a_root_mention_is_read_with_the_mention_stripped() {
        let event = json!({
            "type": "message", "channel": "C1", "user": "UALICE",
            "text": "<@UBOT> fix the &lt;parser&gt;", "ts": "1.0"
        });
        let inbound = parse_message(&event, "UBOT", &NoNames).expect("a message");
        assert_eq!(inbound.text, "fix the <parser>");
        assert!(inbound.mentions_bot);
        assert!(inbound.is_root);
        assert_eq!(inbound.thread, "1.0");
        assert!(!inbound.from_bot);
    }

    #[test]
    fn a_reply_keys_on_its_thread_and_a_root_with_replies_on_itself() {
        let reply = json!({
            "type": "message", "channel": "C1", "user": "UALICE",
            "text": "more", "ts": "1.5", "thread_ts": "1.0"
        });
        let inbound = parse_message(&reply, "UBOT", &NoNames).unwrap();
        assert!(!inbound.is_root);
        assert_eq!(
            (inbound.thread.as_str(), inbound.message.as_str()),
            ("1.0", "1.5")
        );

        let root = json!({
            "type": "message", "channel": "C1", "user": "UALICE",
            "text": "start", "ts": "1.0", "thread_ts": "1.0"
        });
        assert!(parse_message(&root, "UBOT", &NoNames).unwrap().is_root);
    }

    #[test]
    fn bots_and_edits_are_flagged_and_mentions_are_not_messages() {
        let bot =
            json!({"type": "message", "channel": "C1", "bot_id": "B1", "text": "hi", "ts": "2.0"});
        assert!(parse_message(&bot, "UBOT", &NoNames).unwrap().from_bot);
        let own =
            json!({"type": "message", "channel": "C1", "user": "UBOT", "text": "hi", "ts": "2.0"});
        assert!(parse_message(&own, "UBOT", &NoNames).unwrap().from_bot);
        let edit = json!({
            "type": "message", "subtype": "message_changed", "channel": "C1", "ts": "9.9",
            "message": {"user": "UALICE", "text": "edited", "ts": "1.0"}
        });
        let inbound = parse_message(&edit, "UBOT", &NoNames).unwrap();
        assert!(inbound.is_edit);
        assert_eq!(
            inbound.message, "1.0",
            "the original's ts, for the dedup key"
        );
        let mention = json!({"type": "app_mention", "channel": "C1", "user": "UALICE", "text": "<@UBOT> x", "ts": "3.0"});
        assert!(parse_message(&mention, "UBOT", &NoNames).is_none());
    }

    #[test]
    fn files_are_carried_by_id_name_and_url() {
        let event = json!({
            "type": "message", "subtype": "file_share", "channel": "C1", "user": "UALICE",
            "text": "<@UBOT> look", "ts": "1.0",
            "files": [{"id": "F1", "name": "log.txt", "url_private_download": "https://f/F1"},
                      {"id": "F2", "name": "no-url.txt"}]
        });
        let inbound = parse_message(&event, "UBOT", &NoNames).unwrap();
        assert_eq!(inbound.files.len(), 1);
        assert_eq!(inbound.files[0].name, "log.txt");
    }

    #[test]
    fn every_glyph_has_an_emoji() {
        for glyph in [
            Glyph::Working,
            Glyph::Done,
            Glyph::Failed,
            Glyph::Waiting,
            Glyph::Stopped,
            Glyph::Queued,
        ] {
            assert!(!glyph_name(glyph).is_empty());
        }
    }
}
