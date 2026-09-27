//! The coding-agent CLIs on this machine, and handing one a question.
//!
//! e1 borrows an agent CLI already installed, signed in and configured, and
//! starts it on what the reader is looking at — the failing step of a log, a
//! hunk of a diff, an issue nobody has picked up. Nothing here talks directly
//! to a model or holds a credential. A CLI child runs one structured turn at
//! a time and its session id carries the conversation between turns in the
//! app's far-right chat pane. The older terminal hand-off remains available
//! as a small public primitive, but the application surface no longer uses it.
//!
//! Finding the binary is the fiddly part, and for one reason: a window
//! opened from Finder inherits `launchd`'s environment, not the `PATH` the
//! reader's shell builds. Every one of these CLIs installs somewhere that
//! only the shell knows about, so the login shell is asked what it thinks
//! `PATH` is and a few well-known directories are tried besides.

use e1_github::RepoId;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long a CLI has to answer `--version` before it is taken as installed
/// but mute. It is asked once, off the window's thread, but a hung binary
/// must not hold the answer for the others.
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the login shell has to print its `PATH`.
const SHELL_TIMEOUT: Duration = Duration::from_secs(3);

/// A coding-agent CLI e1 knows how to start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Anthropic's Claude Code.
    Claude,
    /// OpenAI's Codex CLI.
    Codex,
    /// Cursor's agent CLI.
    Cursor,
    /// Google's Gemini CLI.
    Gemini,
    /// OpenCode.
    OpenCode,
    /// Sourcegraph's Amp.
    Amp,
}

impl Kind {
    /// Every one, in the order a picker lists them.
    pub const ALL: [Kind; 6] = [
        Kind::Claude,
        Kind::Codex,
        Kind::Cursor,
        Kind::Gemini,
        Kind::OpenCode,
        Kind::Amp,
    ];

    /// The word this kind is stored as, in settings and anywhere else it
    /// outlives the window.
    pub fn id(self) -> &'static str {
        match self {
            Kind::Claude => "claude",
            Kind::Codex => "codex",
            Kind::Cursor => "cursor",
            Kind::Gemini => "gemini",
            Kind::OpenCode => "opencode",
            Kind::Amp => "amp",
        }
    }

    /// Read [`Kind::id`] back.
    pub fn parse(id: &str) -> Option<Self> {
        Kind::ALL.into_iter().find(|kind| kind.id() == id)
    }

    /// What the picker calls it.
    pub fn label(self) -> &'static str {
        match self {
            Kind::Claude => "Claude Code",
            Kind::Codex => "Codex",
            Kind::Cursor => "Cursor",
            Kind::Gemini => "Gemini",
            Kind::OpenCode => "OpenCode",
            Kind::Amp => "Amp",
        }
    }

    /// Whether this CLI has a verified structured-output and resume contract.
    pub fn supports_chat(self) -> bool {
        matches!(self, Kind::Claude | Kind::Codex | Kind::Cursor)
    }

    /// The executables to look for, best name first. Some CLIs ship under
    /// more than one name, and the agent is not always the one the editor
    /// installs as `cursor`.
    pub fn programs(self) -> &'static [&'static str] {
        match self {
            Kind::Claude => &["claude"],
            Kind::Codex => &["codex"],
            Kind::Cursor => &["cursor-agent"],
            Kind::Gemini => &["gemini"],
            Kind::OpenCode => &["opencode"],
            Kind::Amp => &["amp"],
        }
    }

    /// Places this CLI installs itself that are commonly not on `PATH`.
    pub fn extra_locations(self, home: &Path) -> Vec<PathBuf> {
        match self {
            // The installer offers a self-contained copy that is linked
            // into nothing.
            Kind::Claude => vec![home.join(".claude/local/claude")],
            _ => Vec::new(),
        }
    }

    /// The models this CLI is known to take, best-known first, as
    /// suggestions for a picker.
    ///
    /// Not a closed set. What reaches the command line is whatever string
    /// [`Tuning`] carries, because vendors rename models between releases
    /// and a list compiled into a build goes stale; these are the names
    /// each CLI's own `--help` gives, and a name it has never heard of is
    /// still passed through. Empty means e1 has no suggestion to offer,
    /// not that the CLI has no models.
    pub fn models(self) -> &'static [Choice] {
        const CLAUDE: &[Choice] = &[
            Choice::new("fable", "Fable"),
            Choice::new("opus", "Opus"),
            Choice::new("sonnet", "Sonnet"),
        ];
        const CODEX: &[Choice] = &[
            Choice::new("gpt-5.6-sol", "Sol"),
            Choice::new("gpt-5.6-terra", "Terra"),
            Choice::new("gpt-5.6-luna", "Luna"),
        ];
        const CURSOR: &[Choice] = &[
            Choice::new("gpt-5", "GPT-5"),
            Choice::new("sonnet-4", "Sonnet 4"),
            Choice::new("sonnet-4-thinking", "Sonnet 4 thinking"),
        ];
        match self {
            Kind::Claude => CLAUDE,
            Kind::Codex => CODEX,
            Kind::Cursor => CURSOR,
            Kind::Gemini | Kind::OpenCode | Kind::Amp => &[],
        }
    }

    /// How much thinking this CLI can be asked for, least first, when it
    /// can be asked at all.
    ///
    /// Cursor has no such flag — its thinking models say so in their names
    /// — and the rest do not take one either, so for those this is empty
    /// and the row is not drawn.
    pub fn efforts(self) -> &'static [Choice] {
        const CLAUDE: &[Choice] = &[
            Choice::new("low", "Low"),
            Choice::new("medium", "Medium"),
            Choice::new("high", "High"),
            Choice::new("xhigh", "X-high"),
            Choice::new("max", "Max"),
        ];
        const CODEX: &[Choice] = &[
            Choice::new("minimal", "Minimal"),
            Choice::new("low", "Low"),
            Choice::new("medium", "Medium"),
            Choice::new("high", "High"),
            Choice::new("xhigh", "X-high"),
        ];
        match self {
            Kind::Claude => CLAUDE,
            Kind::Codex => CODEX,
            Kind::Cursor | Kind::Gemini | Kind::OpenCode | Kind::Amp => &[],
        }
    }

    /// The arguments that start an interactive session already holding the
    /// question, and asking it of the model the reader picked.
    ///
    /// Claude Code, Codex and Cursor all take the question as the one
    /// positional argument, which is checked; the rest follow their
    /// documented shape. A CLI that has changed its mind about any of this
    /// is a line to edit here.
    ///
    /// A `tuning` with nothing in it adds nothing to the command line,
    /// which is the point: the CLI then starts on whatever the reader
    /// configured for it, exactly as it did before e1 could ask for
    /// anything else.
    pub fn arguments(self, prompt: &str, tuning: &Tuning) -> Vec<String> {
        let prompt = prompt.to_string();
        let model = tuning.model();
        let effort = tuning.effort();
        let flag = |name: &str, value: Option<&str>| -> Vec<String> {
            value
                .map(|value| vec![name.to_string(), value.to_string()])
                .unwrap_or_default()
        };
        match self {
            Kind::Claude => {
                let mut arguments = flag("--model", model);
                arguments.extend(flag("--effort", effort));
                arguments.push(prompt);
                arguments
            }
            Kind::Codex => {
                let mut arguments = flag("-m", model);
                // Codex has no flag for the effort: it is a setting, and
                // `-c` overrides one for this session. The value is parsed
                // as TOML, so the name goes in quoted.
                if let Some(effort) = effort {
                    arguments.push("-c".into());
                    arguments.push(format!("model_reasoning_effort=\"{effort}\""));
                }
                arguments.push(prompt);
                arguments
            }
            Kind::Cursor => {
                let mut arguments = flag("--model", model);
                arguments.push(prompt);
                arguments
            }
            Kind::Gemini => {
                let mut arguments = flag("-m", model);
                arguments.push("-i".into());
                arguments.push(prompt);
                arguments
            }
            Kind::OpenCode => {
                let mut arguments = vec!["run".to_string()];
                arguments.extend(flag("--model", model));
                arguments.push(prompt);
                arguments
            }
            Kind::Amp => vec![prompt],
        }
    }
}

/// One thing a picker offers: what the CLI is passed, and what the reader
/// is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choice {
    /// What goes on the command line.
    pub id: &'static str,
    /// What the picker calls it.
    pub label: &'static str,
}

impl Choice {
    /// One of them, as the tables above write them.
    pub const fn new(id: &'static str, label: &'static str) -> Self {
        Self { id, label }
    }
}

/// Which model an ask is put to, and how much thinking it is allowed.
///
/// Both are strings rather than enumerations, and both are optional: a
/// model named in `~/.e1/app.json` that this build has never heard of is
/// passed through unchanged, and nothing at all means nothing is added to
/// the command line — the CLI's own configuration decides, which is what
/// most readers will want most of the time.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tuning {
    /// What the CLI calls the model: `opus`, `gpt-5.6-sol`.
    pub model: Option<String>,
    /// What it calls the effort level: `high`, `xhigh`.
    pub effort: Option<String>,
}

impl Tuning {
    /// The model, when one was picked and it is not blank.
    pub fn model(&self) -> Option<&str> {
        Self::said(&self.model)
    }

    /// The effort level, when one was picked and it is not blank.
    pub fn effort(&self) -> Option<&str> {
        Self::said(&self.effort)
    }

    /// Nothing said and nothing but spaces said are the same answer: a
    /// hand-edited settings file is where the blank comes from.
    fn said(value: &Option<String>) -> Option<&str> {
        value.as_deref().map(str::trim).filter(|v| !v.is_empty())
    }
}

/// A CLI that is on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    /// Which one.
    pub kind: Kind,
    /// Where its executable is.
    pub program: PathBuf,
    /// What `--version` said, when it answered in time.
    pub version: Option<String>,
}

impl Agent {
    /// The name and, when it gave one, the version.
    pub fn label(&self) -> String {
        match &self.version {
            Some(version) => format!("{} {version}", self.kind.label()),
            None => self.kind.label().to_string(),
        }
    }
}

/// Every directory worth looking in, most authoritative first, without
/// repeats.
///
/// Split from [`discover`] so it can be tested: the inherited `PATH`, what
/// the login shell says, and the places these CLIs install themselves.
pub fn directories(
    inherited: Option<&OsStr>,
    from_shell: Option<&str>,
    home: Option<&Path>,
) -> Vec<PathBuf> {
    let mut all: Vec<PathBuf> = Vec::new();
    let mut push = |directory: PathBuf| {
        if !directory.as_os_str().is_empty() && !all.contains(&directory) {
            all.push(directory);
        }
    };
    for directory in inherited.into_iter().flat_map(std::env::split_paths) {
        push(directory);
    }
    for directory in from_shell.into_iter().flat_map(std::env::split_paths) {
        push(directory);
    }
    push(PathBuf::from("/opt/homebrew/bin"));
    push(PathBuf::from("/usr/local/bin"));
    if let Some(home) = home {
        for suffix in [
            ".local/bin",
            ".bun/bin",
            ".cargo/bin",
            ".volta/bin",
            ".npm-global/bin",
            ".yarn/bin",
            ".deno/bin",
        ] {
            push(home.join(suffix));
        }
    }
    all
}

/// The CLIs in these directories, without asking any of them anything.
pub fn find_in(directories: &[PathBuf], home: Option<&Path>) -> Vec<Agent> {
    Kind::ALL
        .into_iter()
        .filter_map(|kind| {
            let on_path = directories.iter().flat_map(|directory| {
                kind.programs()
                    .iter()
                    .map(|program| directory.join(program))
            });
            let extra = home
                .map(|home| kind.extra_locations(home))
                .unwrap_or_default();
            let program = on_path.chain(extra).find(|path| is_executable(path))?;
            Some(Agent {
                kind,
                program,
                version: None,
            })
        })
        .collect()
}

/// Find every CLI on this machine and ask each what version it is.
///
/// Blocking, and slow enough to matter: it starts a login shell and one
/// process per CLI found. Call it off the window's thread.
pub fn discover() -> Vec<Agent> {
    let home = dirs::home_dir();
    let directories = directories(
        std::env::var_os("PATH").as_deref(),
        login_shell_path().as_deref(),
        home.as_deref(),
    );
    find_in(&directories, home.as_deref())
        .into_iter()
        .map(|agent| Agent {
            version: version_of(&agent.program),
            ..agent
        })
        .collect()
}

/// What the login shell says `PATH` is.
///
/// A window opened from Finder never sees it otherwise, and that is where
/// every one of these CLIs is installed.
pub fn login_shell_path() -> Option<String> {
    /// Printed either side of the value, so that anything an interactive
    /// startup file writes to stdout can be cut away.
    const MARK: &str = "__E1_PATH__";
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|shell| !shell.is_empty())?;
    let mut command = Command::new(shell);
    // `-l` reads the login files and `-i` the interactive ones, because
    // plenty of people extend `PATH` only in the interactive one.
    command.args(["-lic", &format!("printf '{MARK}%s{MARK}' \"$PATH\"")]);
    let output = output_of(command, SHELL_TIMEOUT)?;
    let (_, rest) = output.split_once(MARK)?;
    let (value, _) = rest.split_once(MARK)?;
    (!value.is_empty()).then(|| value.to_string())
}

/// The version `program --version` reports.
pub fn version_of(program: &Path) -> Option<String> {
    let mut command = Command::new(program);
    command.arg("--version");
    version_in(&output_of(command, VERSION_TIMEOUT)?)
}

/// The version out of a `--version` line.
///
/// Every vendor wraps it in different prose — `2.1.238 (Claude Code)`,
/// `codex-cli 0.142.5` — and the number is the part anyone reads. A line
/// with no number in it is worth nothing to a picker, so it comes back as
/// nothing.
fn version_in(text: &str) -> Option<String> {
    text.split_whitespace()
        .map(|word| {
            // The leading run of digits and dots: what follows is a build
            // suffix, a codename or a bracket, and none of those are the
            // version.
            let start = word.trim_start_matches(|c: char| !c.is_ascii_digit());
            let end = start
                .find(|c: char| !c.is_ascii_digit() && c != '.')
                .unwrap_or(start.len());
            start[..end].trim_end_matches('.')
        })
        .find(|word| word.contains('.'))
        .map(str::to_string)
}

/// Run something and read its output, giving up after `timeout`.
fn output_of(mut command: Command, timeout: Duration) -> Option<String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                return None;
            }
            Err(_) => return None,
        }
    }
    let output = child.wait_with_output().ok()?;
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// What the reader is asking about.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ask {
    /// The repository it is in.
    pub repo: Option<RepoId>,
    /// What is on screen, in a few words: `#12 Group a job's log`.
    pub subject: String,
    /// Where it is on the web, for an agent that can fetch it.
    pub url: Option<String>,
    /// Where the excerpt came from: a file and its lines, a job's log.
    pub source: Option<String>,
    /// The lines the reader picked out.
    pub excerpt: Option<String>,
    /// What they typed into the box.
    pub question: String,
    /// What GitHub calls the thing on screen: the numbers and ids an agent
    /// needs to fetch the rest for itself. Name and value, in the order
    /// they are worth reading.
    pub facts: Vec<(String, String)>,
}

impl Ask {
    /// The whole prompt, as the CLI will receive it.
    ///
    /// Ordered the way a person would say it: what we are looking at, where
    /// it is, the part in question, then the question. The excerpt is fenced
    /// because it is very often code or a log and an agent should not have
    /// to guess where it ends.
    pub fn prompt(&self) -> String {
        let mut prompt = String::new();
        if let Some(repo) = &self.repo {
            prompt.push_str(&format!("In {repo}"));
            if !self.subject.is_empty() {
                prompt.push_str(&format!(", {}", self.subject));
            }
            prompt.push_str(".\n");
        } else if !self.subject.is_empty() {
            prompt.push_str(&format!("{}.\n", self.subject));
        }
        if let Some(url) = &self.url {
            prompt.push_str(&format!("{url}\n"));
        }
        if !self.facts.is_empty() {
            prompt.push_str("\nContext:\n");
            for (name, value) in &self.facts {
                prompt.push_str(&format!("- {name}: {value}\n"));
            }
        }
        if let Some(excerpt) = &self.excerpt {
            let source = self.source.as_deref().unwrap_or("The part in question");
            prompt.push_str(&format!(
                "\n{source}:\n\n```\n{}\n```\n",
                excerpt.trim_end()
            ));
        }
        let question = self.question.trim();
        prompt.push_str(&format!(
            "\n{}\n",
            if question.is_empty() {
                "What is going on here, and what would you do about it?"
            } else {
                question
            }
        ));
        prompt
    }
}

/// The shell script that starts a session, as it is written to disk.
///
/// The prompt travels inside the script as a quoted argument rather than in
/// a file the CLI is told to read: every one of these CLIs takes a prompt
/// as an argument, and none of them agree on how to take a file.
pub fn script(agent: &Agent, workdir: Option<&Path>, prompt: &str, tuning: &Tuning) -> String {
    let quote = |value: &str| format!("'{}'", value.replace('\'', r"'\''"));
    let mut script = String::from("#!/bin/sh\n");
    script.push_str(&format!("# Started by e1 for {}\n", agent.kind.label()));
    if let Some(workdir) = workdir {
        script.push_str(&format!(
            "cd {} || exit 1\n",
            quote(&workdir.to_string_lossy())
        ));
    }
    script.push_str(&format!("exec {}", quote(&agent.program.to_string_lossy())));
    for argument in agent.kind.arguments(prompt, tuning) {
        script.push(' ');
        script.push_str(&quote(&argument));
    }
    script.push('\n');
    script
}

/// Write the session's script, ready to be run.
///
/// Split from [`start`] so that everything up to opening the terminal can
/// be checked without opening one.
pub fn write_script(
    agent: &Agent,
    ask: &Ask,
    tuning: &Tuning,
    workdir: Option<&Path>,
    directory: &Path,
) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(directory)?;
    let at = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let path = directory.join(format!("ask-{at}-{}.sh", agent.kind.id()));
    std::fs::write(&path, script(agent, workdir, &ask.prompt(), tuning))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(path)
}

/// Write the session's script and open a terminal on it.
///
/// A terminal, rather than a process this window owns: these CLIs are
/// full-screen programs that ask questions, and a session the reader can
/// keep talking to is the point. What e1 does is start it in the right
/// place with the right first message.
pub fn start(
    agent: &Agent,
    ask: &Ask,
    tuning: &Tuning,
    workdir: Option<&Path>,
    directory: &Path,
) -> std::io::Result<PathBuf> {
    let path = write_script(agent, ask, tuning, workdir, directory)?;
    Command::new("open")
        .arg("-a")
        .arg("Terminal")
        .arg(&path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(path)
}

/// One answer from a CLI-backed chat turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatReply {
    /// Markdown returned for the assistant bubble.
    pub text: String,
    /// The CLI's durable conversation identifier, when it supplied one.
    pub session: Option<String>,
}

/// Arguments for one headless chat turn.
///
/// Interactive terminal arguments and chat arguments are deliberately
/// separate. A full-screen TUI owns a terminal; the pane needs structured
/// stdout and a session id it can pass back on the next turn.
pub fn chat_arguments(
    kind: Kind,
    prompt: &str,
    tuning: &Tuning,
    session: Option<&str>,
) -> anyhow::Result<Vec<String>> {
    let mut arguments = match kind {
        Kind::Codex => {
            let mut arguments = vec!["exec".into()];
            if let Some(session) = session {
                arguments.extend(["resume".into(), session.into()]);
            }
            arguments.push("--json".into());
            if let Some(model) = tuning.model() {
                arguments.extend(["-m".into(), model.into()]);
            }
            if let Some(effort) = tuning.effort() {
                arguments.extend(["-c".into(), format!("model_reasoning_effort=\"{effort}\"")]);
            }
            arguments
        }
        Kind::Claude => {
            let mut arguments = vec!["-p".into(), "--output-format".into(), "json".into()];
            if let Some(session) = session {
                arguments.extend(["--resume".into(), session.into()]);
            }
            if let Some(model) = tuning.model() {
                arguments.extend(["--model".into(), model.into()]);
            }
            if let Some(effort) = tuning.effort() {
                arguments.extend(["--effort".into(), effort.into()]);
            }
            arguments
        }
        Kind::Cursor => {
            let mut arguments = vec!["-p".into(), "--output-format".into(), "json".into()];
            if let Some(session) = session {
                arguments.extend(["--resume".into(), session.into()]);
            }
            if let Some(model) = tuning.model() {
                arguments.extend(["--model".into(), model.into()]);
            }
            arguments
        }
        _ => anyhow::bail!(
            "{} does not expose a supported chat output yet",
            kind.label()
        ),
    };
    arguments.push(prompt.into());
    Ok(arguments)
}

/// Run one turn through an installed CLI and keep its session for the next.
///
/// This blocks and must be called on GPUI's background executor. The child
/// inherits the user's CLI configuration and credentials, but not a terminal;
/// stdout is structured data owned by the pane and stderr becomes a visible
/// error instead of disappearing into the application log.
pub fn chat(
    agent: &Agent,
    prompt: &str,
    tuning: &Tuning,
    session: Option<&str>,
    workdir: Option<&Path>,
) -> anyhow::Result<ChatReply> {
    let arguments = chat_arguments(agent.kind, prompt, tuning, session)?;
    let mut command = Command::new(&agent.program);
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(workdir) = workdir {
        command.current_dir(workdir);
    }
    let output = command.output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        anyhow::bail!(
            "{} exited with {}{}",
            agent.kind.label(),
            output.status,
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        );
    }
    parse_chat_output(agent.kind, &stdout)
}

/// Read the stable parts of the CLIs' JSON without coupling the view to all
/// of their progress-event variants.
pub fn parse_chat_output(kind: Kind, output: &str) -> anyhow::Result<ChatReply> {
    let values: Vec<serde_json::Value> = output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    let whole = serde_json::from_str::<serde_json::Value>(output).ok();
    let values: Vec<&serde_json::Value> = if values.is_empty() {
        whole.iter().collect()
    } else {
        values.iter().collect()
    };

    let first_string = |names: &[&str]| {
        values.iter().find_map(|value| {
            names
                .iter()
                .find_map(|name| value.get(*name).and_then(|value| value.as_str()))
                .map(str::to_string)
        })
    };
    let session = first_string(&["thread_id", "session_id", "chat_id", "chatId"]);
    let text = match kind {
        Kind::Codex => values
            .iter()
            .filter_map(|value| {
                let item = value.get("item")?;
                (item.get("type")?.as_str()? == "agent_message")
                    .then(|| item.get("text")?.as_str().map(str::to_string))?
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => first_string(&["result", "text", "response", "message"]).unwrap_or_default(),
    };
    if text.trim().is_empty() {
        anyhow::bail!("{} returned no assistant message", kind.label());
    }
    Ok(ChatReply { text, session })
}

/// Where a repository is checked out, if it is, under any of these roots.
///
/// `ghq` and its imitators all lay a checkout out the same way, so the
/// lookup is a path join rather than a search: a session started in the
/// wrong directory is worse than one started in the home directory, which
/// is what happens when this finds nothing.
pub fn checkout_in(repo: &RepoId, roots: &[PathBuf]) -> Option<PathBuf> {
    roots
        .iter()
        .flat_map(|root| {
            [
                root.join("github.com").join(&repo.owner).join(&repo.name),
                root.join(&repo.owner).join(&repo.name),
            ]
        })
        .find(|candidate| candidate.join(".git").exists())
}

/// The roots worth looking under: what `ghq` says, then the usual places.
pub fn checkout_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let mut command = Command::new("ghq");
    command.arg("root");
    if let Some(output) = output_of(command, SHELL_TIMEOUT) {
        roots.extend(
            output
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(PathBuf::from),
        );
    }
    if let Some(home) = dirs::home_dir() {
        for suffix in ["ghq", "src", "Documents/GitHub", "Projects"] {
            roots.push(home.join(suffix));
        }
    }
    roots
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_search_path_is_the_shell_s_first_and_has_no_repeats() {
        let home = PathBuf::from("/home/someone");
        let found = directories(
            Some(OsStr::new("/usr/bin:/opt/homebrew/bin")),
            Some("/opt/homebrew/bin:/home/someone/.local/bin"),
            Some(&home),
        );
        assert_eq!(found[0], PathBuf::from("/usr/bin"));
        assert_eq!(found[1], PathBuf::from("/opt/homebrew/bin"));
        assert_eq!(found[2], PathBuf::from("/home/someone/.local/bin"));
        assert_eq!(
            found.iter().filter(|d| d.ends_with("homebrew/bin")).count(),
            1,
            "a directory named twice is searched once"
        );
        assert!(
            found.contains(&home.join(".bun/bin")),
            "the well-known places are tried even off PATH"
        );
    }

    #[test]
    fn only_executables_count_as_installed() {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("claude"), "#!/bin/sh\n").unwrap();
        std::fs::write(bin.join("codex"), "not runnable").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(bin.join("claude"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        let found = find_in(&[bin], None);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, Kind::Claude);
        assert_eq!(found[0].label(), "Claude Code");
    }

    #[test]
    fn a_version_is_the_number_and_not_the_prose_around_it() {
        assert_eq!(
            version_in("2.1.238 (Claude Code)").as_deref(),
            Some("2.1.238")
        );
        assert_eq!(version_in("codex-cli 0.142.5").as_deref(), Some("0.142.5"));
        assert_eq!(
            version_in("2025.09.18-7ae6800").as_deref(),
            Some("2025.09.18"),
            "a build suffix is not part of the number"
        );
        assert_eq!(version_in("no idea").as_deref(), None);
    }

    #[test]
    fn every_cli_takes_the_prompt_somewhere() {
        for kind in Kind::ALL {
            let arguments = kind.arguments("why did this fail?", &Tuning::default());
            assert!(
                arguments
                    .iter()
                    .any(|argument| argument == "why did this fail?"),
                "{} drops the prompt",
                kind.label()
            );
        }
    }

    #[test]
    fn nothing_picked_puts_nothing_on_the_command_line() {
        for kind in Kind::ALL {
            let bare = kind.arguments("why?", &Tuning::default());
            let blank = kind.arguments(
                "why?",
                &Tuning {
                    model: Some("  ".into()),
                    effort: Some(String::new()),
                },
            );
            assert_eq!(
                bare,
                blank,
                "{} reads a blank choice as a choice",
                kind.label()
            );
        }
    }

    #[test]
    fn a_model_and_an_effort_reach_the_command_line() {
        let tuning = Tuning {
            model: Some("opus".into()),
            effort: Some("xhigh".into()),
        };
        assert_eq!(
            Kind::Claude.arguments("why?", &tuning),
            ["--model", "opus", "--effort", "xhigh", "why?"]
        );
        // Codex has no flag for the effort, and the override it does have
        // is parsed as TOML.
        assert_eq!(
            Kind::Codex.arguments(
                "why?",
                &Tuning {
                    model: Some("gpt-5.6-sol".into()),
                    effort: Some("high".into()),
                }
            ),
            [
                "-m",
                "gpt-5.6-sol",
                "-c",
                "model_reasoning_effort=\"high\"",
                "why?"
            ]
        );
        // Cursor takes a model and nothing else; the effort is dropped
        // rather than guessed at.
        assert_eq!(
            Kind::Cursor.arguments("why?", &tuning),
            ["--model", "opus", "why?"]
        );
        assert_eq!(
            Kind::OpenCode.arguments("why?", &tuning),
            ["run", "--model", "opus", "why?"]
        );
    }

    #[test]
    fn a_model_this_build_never_heard_of_is_passed_through() {
        let tuning = Tuning {
            model: Some("gpt-9-whatever-comes-next".into()),
            ..Tuning::default()
        };
        assert!(
            Kind::Codex
                .arguments("why?", &tuning)
                .contains(&"gpt-9-whatever-comes-next".to_string()),
            "the suggestions are suggestions, not a closed set"
        );
    }

    #[test]
    fn a_cli_offered_an_effort_is_a_cli_that_takes_one() {
        for kind in Kind::ALL {
            for effort in kind.efforts() {
                let arguments = kind.arguments(
                    "why?",
                    &Tuning {
                        effort: Some(effort.id.to_string()),
                        ..Tuning::default()
                    },
                );
                assert!(
                    arguments
                        .iter()
                        .any(|argument| argument.contains(effort.id)),
                    "{} offers {} and then drops it",
                    kind.label(),
                    effort.id
                );
            }
            for model in kind.models() {
                let arguments = kind.arguments(
                    "why?",
                    &Tuning {
                        model: Some(model.id.to_string()),
                        ..Tuning::default()
                    },
                );
                assert!(
                    arguments.iter().any(|argument| argument == model.id),
                    "{} offers {} and then drops it",
                    kind.label(),
                    model.id
                );
            }
        }
    }

    #[test]
    fn a_prompt_says_what_where_and_which_part() {
        let ask = Ask {
            repo: RepoId::parse("bokuweb/e1"),
            subject: "#12 Group a job's log by its steps".into(),
            url: Some("https://github.com/bokuweb/e1/pull/12".into()),
            source: Some("From the log of the job \"test\"".into()),
            excerpt: Some("error[E0425]: cannot find value\n".into()),
            question: "why is this failing?".into(),
            facts: vec![("pull request".into(), "#12".into())],
        };
        let prompt = ask.prompt();
        assert!(prompt.starts_with("In bokuweb/e1, #12 Group a job's log by its steps.\n"));
        assert!(prompt.contains("- pull request: #12\n"), "{prompt}");
        assert!(prompt.contains("https://github.com/bokuweb/e1/pull/12"));
        assert!(prompt.contains("```\nerror[E0425]: cannot find value\n```"));
        assert!(prompt.trim_end().ends_with("why is this failing?"));
    }

    #[test]
    fn a_prompt_with_nothing_typed_still_asks_something() {
        let ask = Ask {
            subject: "the log of the job \"build\"".into(),
            excerpt: Some("boom".into()),
            ..Ask::default()
        };
        let prompt = ask.prompt();
        assert!(prompt.contains("boom"));
        assert!(prompt.trim_end().ends_with('?'), "{prompt}");
    }

    #[test]
    fn the_script_goes_to_the_checkout_and_quotes_what_it_passes() {
        let agent = Agent {
            kind: Kind::Claude,
            program: PathBuf::from("/usr/local/bin/claude"),
            version: None,
        };
        let script = script(
            &agent,
            Some(Path::new("/src/it's here")),
            "why did 'this' fail?",
            &Tuning::default(),
        );
        assert!(script.starts_with("#!/bin/sh\n"));
        assert!(script.contains(r"cd '/src/it'\''s here' || exit 1"));
        assert!(script.contains(r"exec '/usr/local/bin/claude' 'why did '\''this'\'' fail?'"));
    }

    #[test]
    fn a_session_with_no_checkout_starts_where_it_is() {
        let agent = Agent {
            kind: Kind::OpenCode,
            program: PathBuf::from("/opt/homebrew/bin/opencode"),
            version: None,
        };
        let script = script(&agent, None, "hello", &Tuning::default());
        assert!(!script.contains("cd "));
        assert!(script.contains("'run' 'hello'"), "{script}");
    }

    #[test]
    fn a_written_script_is_runnable_and_carries_the_whole_prompt() {
        let directory = tempfile::tempdir().unwrap();
        let agent = Agent {
            kind: Kind::Codex,
            program: PathBuf::from("/usr/local/bin/codex"),
            version: Some("0.142.5".into()),
        };
        let ask = Ask {
            repo: RepoId::parse("bokuweb/e1"),
            subject: "the log of the job \"test\"".into(),
            excerpt: Some("test result: FAILED".into()),
            question: "why?".into(),
            facts: vec![
                ("workflow run".into(), "912".into()),
                ("job".into(), "test (2)".into()),
            ],
            ..Ask::default()
        };
        let tuning = Tuning {
            model: Some("gpt-5.6-sol".into()),
            effort: Some("high".into()),
        };
        let path = write_script(&agent, &ask, &tuning, None, directory.path()).unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("test result: FAILED"));
        assert!(written.contains("- workflow run: 912"));
        assert!(written.contains("- job: test (2)"));
        assert!(written.contains("why?"));
        assert!(
            written.contains(r#"'-m' 'gpt-5.6-sol'"#),
            "the model is quoted onto the command line: {written}"
        );
        assert!(
            written.contains(r#"'-c' 'model_reasoning_effort="high"'"#),
            "and so is the effort, TOML quotes and all: {written}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert!(mode & 0o100 != 0, "the script has to be runnable");
        }
    }

    #[test]
    fn a_checkout_is_found_by_its_shape() {
        let root = tempfile::tempdir().unwrap();
        let repo = RepoId::parse("bokuweb/e1").unwrap();
        let checkout = root.path().join("github.com/bokuweb/e1");
        std::fs::create_dir_all(checkout.join(".git")).unwrap();
        let roots = vec![root.path().to_path_buf()];
        assert_eq!(checkout_in(&repo, &roots), Some(checkout));
        let other = RepoId::parse("bokuweb/ginka").unwrap();
        assert_eq!(
            checkout_in(&other, &roots),
            None,
            "not every repo is cloned"
        );
    }

    #[test]
    fn chat_turns_start_and_resume_each_cli_session() {
        let tuning = Tuning {
            model: Some("gpt-5.6-sol".into()),
            effort: Some("high".into()),
        };
        assert_eq!(
            chat_arguments(Kind::Codex, "first", &tuning, None).unwrap(),
            [
                "exec",
                "--json",
                "-m",
                "gpt-5.6-sol",
                "-c",
                "model_reasoning_effort=\"high\"",
                "first"
            ]
        );
        assert_eq!(
            chat_arguments(Kind::Codex, "again", &tuning, Some("thread-1")).unwrap(),
            [
                "exec",
                "resume",
                "thread-1",
                "--json",
                "-m",
                "gpt-5.6-sol",
                "-c",
                "model_reasoning_effort=\"high\"",
                "again"
            ]
        );
        assert!(
            chat_arguments(Kind::Claude, "again", &tuning, Some("session-1"))
                .unwrap()
                .windows(2)
                .any(|pair| pair == ["--resume", "session-1"])
        );
        assert!(
            chat_arguments(Kind::Cursor, "again", &tuning, Some("chat-1"))
                .unwrap()
                .windows(2)
                .any(|pair| pair == ["--resume", "chat-1"])
        );
        assert!(chat_arguments(Kind::Gemini, "no", &tuning, None).is_err());
        assert!(Kind::Claude.supports_chat());
        assert!(Kind::Codex.supports_chat());
        assert!(Kind::Cursor.supports_chat());
        assert!(!Kind::Gemini.supports_chat());
    }

    #[test]
    fn cli_json_becomes_one_chat_reply_and_a_resumable_session() {
        let codex = concat!(
            r#"{"type":"thread.started","thread_id":"abc"}"#,
            "\n",
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"First"}}"#,
            "\n",
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"Second"}}"#,
        );
        assert_eq!(
            parse_chat_output(Kind::Codex, codex).unwrap(),
            ChatReply {
                text: "First\n\nSecond".into(),
                session: Some("abc".into()),
            }
        );
        assert_eq!(
            parse_chat_output(
                Kind::Claude,
                r#"{"type":"result","result":"Done","session_id":"def"}"#,
            )
            .unwrap(),
            ChatReply {
                text: "Done".into(),
                session: Some("def".into()),
            }
        );
        assert!(parse_chat_output(Kind::Codex, r#"{"type":"turn.completed"}"#).is_err());
    }
}
