//! Generating a commit message, and making the commit.
//!
//! A subject line is a fixed classification over a diff that is already in the
//! prompt. It does not need — or benefit from — the model the session runs on,
//! so generation is pinned to the cheapest tier each provider exposes and to
//! the lowest effort that tier accepts, whatever the session selected. The
//! diff is capped for the same reason: past a point it stops informing the
//! subject and starts costing money and latency. See `docs/roadmap.md` §3.3
//! N9.

use anyhow::{Context, Result, bail};
use ginka_protocol::provider::{AccessMode, ProviderKind, SessionOptions};
use std::path::Path;
use std::time::Duration;

use crate::driver::{AgentDriver, ParseState, SessionSpec};
use crate::git::Git;

/// How long a model is given to write a subject line. Generous: a first
/// turn on a cold CLI takes a while to start, and a subject that arrives late
/// is still worth more than none.
pub const GENERATE_TIMEOUT: Duration = Duration::from_secs(90);

/// How much diff goes into the prompt. A subject describes the shape of a
/// change, and the shape is visible long before the last hunk.
pub const MAX_DIFF_BYTES: usize = 96 * 1024;

/// The model and effort a commit subject is generated on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageModel {
    /// `None` means "whatever the CLI defaults to": some providers expose no
    /// cheap tier by name, and guessing an id that does not exist is worse
    /// than letting the CLI choose.
    pub model: Option<String>,
    /// Reasoning effort to ask for; `None` where the provider has no such knob.
    pub reasoning_effort: Option<String>,
}

/// The cheap tier for a provider. Deliberately ignores `session`: it is taken
/// only to make the signature say out loud that the session's own model is
/// *not* what runs here.
pub fn message_model(provider: ProviderKind, _session: &SessionOptions) -> MessageModel {
    let (model, effort) = match provider {
        ProviderKind::Claude => (Some("haiku"), Some("low")),
        ProviderKind::Codex => (None, Some("minimal")),
        ProviderKind::Gemini => (Some("flash"), None),
        // No cheap tier we can name: take the CLI's default, at its lowest
        // effort where it has one.
        ProviderKind::Amp | ProviderKind::OpenCode | ProviderKind::Cursor => (None, Some("low")),
    };
    MessageModel {
        model: model.map(str::to_string),
        reasoning_effort: effort.map(str::to_string),
    }
}

/// What is about to be committed: the paths, and the diff as text.
///
/// `staged` describes only what is staged, which is what `Commit { all:
/// false }` will take; otherwise everything uncommitted, tracked or not. An
/// untracked file is named in the list but has no diff against `HEAD`, and
/// the list is what the prompt says is complete.
pub fn describe(worktree: &Path, staged: bool) -> Result<(Vec<String>, String)> {
    let git = Git::new(worktree);
    let status = git.run(&["status", "--porcelain", "--untracked-files=all"])?;
    let files: Vec<String> = status
        .lines()
        .filter(|line| line.len() > 3)
        .filter(|line| !staged || !line.starts_with(' ') && !line.starts_with('?'))
        .map(|line| line[3..].trim().to_string())
        .collect();
    if files.is_empty() {
        bail!("nothing to describe: the worktree is clean");
    }
    let diff = if staged {
        git.run(&["diff", "--cached"])?
    } else {
        git.run(&["diff", "HEAD"])?
    };
    Ok((files, diff))
}

/// Ask `driver` for a commit message over `files` and `diff`, and wait.
///
/// One turn on the provider's cheap tier, read-only, with no MCP servers
/// and nothing to resume: the drivers' own command and parser are what run
/// it, so a vendor's format change breaks this exactly where it breaks a
/// session (R6). Blocks for up to [`GENERATE_TIMEOUT`], so it belongs on a
/// thread of its own, not on the request path.
pub fn generate(
    driver: &dyn AgentDriver,
    worktree: &Path,
    env: &[(String, String)],
    files: &[String],
    diff: &str,
) -> Result<CommitMessage> {
    CommitMessage::parse(&ask_cheap(
        driver,
        worktree,
        env,
        &build_prompt(files, diff),
    )?)
}

/// Ask `driver` one question on its cheap tier, read-only, and answer with
/// what it said (§3.3 N9). Shared by commit messages and pull request
/// details: both are a fixed classification over a diff already in the
/// prompt, and both run off the service's lock.
pub fn ask_cheap(
    driver: &dyn AgentDriver,
    worktree: &Path,
    env: &[(String, String)],
    prompt: &str,
) -> Result<String> {
    use std::io::{BufRead as _, Write as _};

    let model = ProviderKind::parse(driver.id())
        .map(|provider| message_model(provider, &SessionOptions::default()).model)
        .unwrap_or_default();
    let prompt = prompt.to_string();
    let spec = SessionSpec::new(worktree, prompt.clone())
        .with_model(model)
        .with_access_mode(AccessMode::ReadOnly);
    let command = driver.start_command(&spec);

    let mut process = std::process::Command::new(&command.program);
    process.args(&command.args).current_dir(worktree);
    crate::agent::sanitize(&mut process);
    for (key, value) in command.env.iter().chain(env.iter()) {
        process.env(key, value);
    }
    let streamed = driver.supports_steer();
    let mut child = process
        .stdin(if streamed {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("starting {}", command.program))?;

    // The prompt, then a closed input: the one message this turn gets.
    if streamed
        && let Some(mut stdin) = child.stdin.take()
        && let Some(line) = driver.encode_user_message(&prompt)
    {
        let _ = writeln!(stdin, "{line}");
        let _ = stdin.flush();
        drop(stdin);
    }

    let stdout = child.stdout.take().context("no stdout to read")?;
    let (sender, lines) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if sender.send(line).is_err() {
                break;
            }
        }
    });

    let deadline = std::time::Instant::now() + GENERATE_TIMEOUT;
    let mut state = ParseState::default();
    let mut said = String::new();
    loop {
        let now = std::time::Instant::now();
        if now >= deadline {
            let _ = child.kill();
            bail!(
                "{} did not answer within {:?}",
                driver.display_name(),
                GENERATE_TIMEOUT
            );
        }
        match lines.recv_timeout(deadline - now) {
            Ok(line) => {
                for event in driver.parse_line(&line, &mut state) {
                    if let ginka_protocol::AgentEvent::TextDelta { text } = event {
                        said.push_str(&text);
                    }
                }
            }
            // The pipe closed: the agent has said all it will.
            Err(_) => break,
        }
    }
    let _ = child.wait();
    if said.trim().is_empty() {
        bail!(
            "{} said nothing this build could read ({} unrecognized lines)",
            driver.display_name(),
            state.unrecognized
        );
    }
    Ok(said)
}

/// The prompt a commit subject is generated from.
pub fn build_prompt(files: &[String], diff: &str) -> String {
    let mut prompt = String::from(
        "Write a git commit message for the change below.\n\
         Answer with the subject line only, in the imperative mood, under 72 \
         characters, with no quotes and no trailing period.\n\n\
         Files changed:\n",
    );
    for file in files {
        prompt.push_str("- ");
        prompt.push_str(file);
        prompt.push('\n');
    }

    prompt.push_str("\nDiff:\n");
    if diff.len() > MAX_DIFF_BYTES {
        let mut cut = MAX_DIFF_BYTES;
        while cut > 0 && !diff.is_char_boundary(cut) {
            cut -= 1;
        }
        prompt.push_str(&diff[..cut]);
        // Said out loud, because a model that thinks it has seen the whole
        // change will describe it as if it had.
        prompt.push_str("\n… diff truncated here; the file list above is complete.\n");
    } else {
        prompt.push_str(diff);
    }
    prompt
}

/// A commit message read from a model's answer, ready for git.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitMessage {
    /// One line, quotes and fences stripped, cut on a word to
    /// [`CommitMessage::MAX_SUBJECT_CHARS`]. Never empty.
    pub subject: String,
    /// Everything after the subject line, trimmed; `None` when the model gave
    /// only a subject.
    pub body: Option<String>,
}

impl CommitMessage {
    /// Longest subject kept, in characters; longer ones are cut on a word.
    pub const MAX_SUBJECT_CHARS: usize = 72;

    /// Read a model's answer into a commit message.
    ///
    /// Models wrap subjects in quotes, fence them in code blocks, and prefix
    /// them with "Subject:" no matter how the prompt asks. Unwrapping here
    /// keeps every driver from having to.
    pub fn parse(raw: &str) -> Result<Self> {
        let cleaned = strip_fences(raw.trim());
        let mut parts = cleaned.splitn(2, '\n');
        let first = parts.next().unwrap_or_default();
        let rest = parts.next().unwrap_or_default().trim();

        let subject = tidy_subject(first);
        if subject.is_empty() {
            bail!("the model returned no commit subject");
        }

        Ok(Self {
            subject,
            body: (!rest.is_empty()).then(|| rest.to_string()),
        })
    }

    /// The message as git takes it: subject, blank line, body.
    pub fn to_git_message(&self) -> String {
        match &self.body {
            Some(body) => format!("{}\n\n{}\n", self.subject, body),
            None => format!("{}\n", self.subject),
        }
    }
}

/// Stage everything and commit it.
pub fn commit_all(worktree: &Path, message: &CommitMessage) -> Result<String> {
    let git = Git::new(worktree);
    git.run(&["add", "-A"]).context("staging changes")?;

    let staged = git.query(&["diff", "--cached", "--quiet"]);
    // `--quiet` exits non-zero when there *are* staged changes, so a
    // successful run means there is nothing to commit.
    if matches!(staged, Ok(Some(_))) {
        bail!("nothing to commit: the worktree is clean");
    }

    git.run(&["commit", "-q", "-m", &message.to_git_message()])
        .context("committing")?;
    git.run(&["rev-parse", "HEAD"])
}

/// Drop a surrounding ``` fence, which models add even when asked not to.
fn strip_fences(text: &str) -> String {
    let trimmed = text.trim();
    let Some(inner) = trimmed.strip_prefix("```") else {
        return trimmed.to_string();
    };
    // The opening fence may carry a language tag on the same line.
    let inner = inner.split_once('\n').map_or("", |(_, rest)| rest);
    inner
        .trim_end()
        .strip_suffix("```")
        .unwrap_or(inner)
        .trim()
        .to_string()
}

fn tidy_subject(line: &str) -> String {
    let mut subject = line.trim();
    for prefix in ["Subject:", "subject:", "Commit message:"] {
        if let Some(rest) = subject.strip_prefix(prefix) {
            subject = rest.trim();
        }
    }
    let subject = subject
        .trim_matches('"')
        .trim_matches('\'')
        .trim_matches('`')
        .trim();

    // A single trailing period is noise; an ellipsis is meaning.
    let subject = match subject.strip_suffix('.') {
        Some(shorter) if !shorter.ends_with('.') => shorter,
        _ => subject,
    };

    truncate_on_word(subject.trim(), CommitMessage::MAX_SUBJECT_CHARS)
}

/// Cut to a word boundary, marking the cut. A subject that stops mid-word
/// reads like a bug in the app rather than a long message.
fn truncate_on_word(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let budget: String = text.chars().take(max_chars - 1).collect();
    let cut = match budget.rfind(char::is_whitespace) {
        Some(index) if index > max_chars / 2 => budget[..index].to_string(),
        _ => budget,
    };
    format!("{}…", cut.trim_end())
}
