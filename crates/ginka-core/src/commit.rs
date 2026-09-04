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
use ginka_protocol::provider::{ProviderKind, SessionOptions};
use std::path::Path;

use crate::git::Git;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitMessage {
    pub subject: String,
    pub body: Option<String>,
}

impl CommitMessage {
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
