//! Text on its way in from Slack and on its way out to it.
//!
//! Slack `mrkdwn` is not Markdown, and a message is not a document: this is
//! where the agent's Markdown becomes something a thread renders, where the
//! reply is cut on a boundary a reader can follow, and where the small
//! vocabulary a thread can speak back — `stop`, `yes abcde` — is recognised.

use serde::{Deserialize, Serialize};

/// The most characters one reply message carries.
///
/// Slack's ceiling is 4,000 per `chat.postMessage`; a hundred below it leaves
/// room for the prefix a recovered reply gets.
pub const CHUNK_CHARS: usize = 3_900;

/// How many chunks a reply is posted as before the rest becomes a file.
pub const MAX_CHUNKS: usize = 4;

/// The exact replies that mean "post nothing".
///
/// Borrowed from Hermes: a prompt that says "only speak up if you find
/// something" needs a way to say it found nothing. The transcript keeps the
/// reply; the thread does not see it.
pub const SILENCE: &[&str] = &["[SILENT]", "SILENT", "NO_REPLY", "NO REPLY"];

/// Whether a reply asks not to be posted.
pub fn is_silent(reply: &str) -> bool {
    let trimmed = reply.trim();
    SILENCE
        .iter()
        .any(|token| trimmed.eq_ignore_ascii_case(token))
}

/// The few words a thread can say to the connector rather than to the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Control {
    /// Cancel the running turn.
    Stop,
    /// Post the session's state and last activity.
    Status,
    /// Close the thread's mapping, so the next message starts a fresh session.
    New,
}

/// A control word, when the whole message is exactly one.
///
/// Exactly: `stop` is a control, `stop what you are doing` is a prompt.
pub fn parse_control(text: &str) -> Option<Control> {
    match text.trim().to_ascii_lowercase().as_str() {
        "stop" => Some(Control::Stop),
        "status" => Some(Control::Status),
        "new" => Some(Control::New),
        _ => None,
    }
}

/// How a thread answers a question the agent asked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Answer {
    Yes,
    No,
    /// Free text, for a question with no options, or an option by name.
    Text(String),
}

impl Answer {
    /// What reaches the agent.
    pub fn as_response(&self) -> String {
        match self {
            Self::Yes => "yes".to_string(),
            Self::No => "no".to_string(),
            Self::Text(text) => text.clone(),
        }
    }
}

/// The letters a request id is drawn from: lowercase without `l`, which
/// reads as `1` or `I` when typed on a phone. Borrowed from Claude Code's
/// permission relay for the same reason.
const ID_ALPHABET: &[u8] = b"abcdefghijkmnopqrstuvwxyz";
/// Five letters: short enough to type, and 25^5 is enough that a guess is
/// not a plan.
pub const ID_LEN: usize = 5;

/// A fresh request id.
pub fn new_request_id() -> String {
    let bytes = uuid::Uuid::new_v4();
    bytes
        .as_bytes()
        .iter()
        .take(ID_LEN)
        .map(|byte| ID_ALPHABET[*byte as usize % ID_ALPHABET.len()] as char)
        .collect()
}

/// Whether a word has the shape of a request id.
fn looks_like_id(word: &str) -> bool {
    word.len() == ID_LEN
        && word
            .bytes()
            .all(|byte| ID_ALPHABET.contains(&byte.to_ascii_lowercase()))
}

/// A verdict, when the message is one: `yes abcde`, `no abcde`, or
/// `abcde: some text`.
///
/// The id is lowercased on the way out because a phone capitalises the
/// first word of a reply. Whether the id names an open request is the
/// caller's question; this only reads the shape.
pub fn parse_verdict(text: &str) -> Option<(String, Answer)> {
    let text = text.trim();
    let mut words = text.split_whitespace();
    let first = words.next()?;
    let second = words.next();
    let rest = words.next();
    match (first.to_ascii_lowercase().as_str(), second, rest) {
        ("yes", Some(id), None) if looks_like_id(id) => {
            Some((id.to_ascii_lowercase(), Answer::Yes))
        }
        ("no", Some(id), None) if looks_like_id(id) => Some((id.to_ascii_lowercase(), Answer::No)),
        _ => {
            let (id, answer) = text.split_once(':')?;
            let id = id.trim();
            let answer = answer.trim();
            if looks_like_id(id) && !answer.is_empty() {
                Some((id.to_ascii_lowercase(), Answer::Text(answer.to_string())))
            } else {
                None
            }
        }
    }
}

/// Take the bot's own mention out of a message.
///
/// Returns the text without it, and whether it was there. Only the bot's
/// mention is removed: other people's stay, unescaped, because "ask @alice"
/// is part of what was said.
pub fn strip_mention(text: &str, bot_user: &str) -> (String, bool) {
    let needle = format!("<@{bot_user}>");
    if !text.contains(&needle) {
        return (text.trim().to_string(), false);
    }
    let stripped = text.replace(&needle, " ");
    (collapse_spaces(&stripped), true)
}

/// Runs of spaces from a removed mention collapse; line breaks stay.
fn collapse_spaces(text: &str) -> String {
    text.lines()
        .map(|line| {
            line.split(' ')
                .filter(|w| !w.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// A member's display name, for unescaping mentions.
pub trait Names {
    /// The name to show for a member id, if known.
    fn user_name(&self, id: &str) -> Option<String>;
}

/// Names that are never known, for tests and for a connector with no cache.
pub struct NoNames;

impl Names for NoNames {
    fn user_name(&self, _id: &str) -> Option<String> {
        None
    }
}

/// Turn Slack's inbound escaping into plain text an agent can read.
///
/// `<@U…>` becomes `@name`, `<#C…|name>` becomes `#name`, `<url|label>`
/// becomes `label (url)`, and the three HTML entities are undone.
pub fn unescape(text: &str, names: &dyn Names) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('>') else {
            out.push('<');
            rest = after;
            continue;
        };
        let inner = &after[..end];
        out.push_str(&expand_entity(inner, names));
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn expand_entity(inner: &str, names: &dyn Names) -> String {
    if let Some(user) = inner.strip_prefix('@') {
        let (id, label) = user.split_once('|').unwrap_or((user, ""));
        let name = if label.is_empty() {
            names.user_name(id).unwrap_or_else(|| id.to_string())
        } else {
            label.to_string()
        };
        return format!("@{name}");
    }
    if let Some(channel) = inner.strip_prefix('#') {
        let (id, label) = channel.split_once('|').unwrap_or((channel, ""));
        return format!("#{}", if label.is_empty() { id } else { label });
    }
    if let Some(special) = inner.strip_prefix('!') {
        let (kind, label) = special.split_once('|').unwrap_or((special, ""));
        return format!("@{}", if label.is_empty() { kind } else { label });
    }
    match inner.split_once('|') {
        Some((url, label)) if !label.is_empty() => format!("{label} ({url})"),
        _ => inner.to_string(),
    }
}

/// The agent's Markdown as Slack `mrkdwn`.
///
/// Headings become bold lines, `**bold**` becomes `*bold*`, links become
/// `<url|label>`, list markers are kept, and fenced code is left exactly as
/// it was inside its fence — a diff or a stack trace must not be rewritten.
/// Tables are put in a code block, because Slack cannot draw one.
pub fn to_mrkdwn(markdown: &str) -> String {
    let mut out = Vec::new();
    let mut in_fence = false;
    let mut table: Vec<String> = Vec::new();
    for line in markdown.lines() {
        if line.trim_start().starts_with("```") {
            flush_table(&mut table, &mut out);
            in_fence = !in_fence;
            // Slack ignores a language tag and shows it as text.
            out.push("```".to_string());
            continue;
        }
        if in_fence {
            out.push(line.to_string());
            continue;
        }
        if line.trim_start().starts_with('|') {
            table.push(line.to_string());
            continue;
        }
        flush_table(&mut table, &mut out);
        out.push(inline_mrkdwn(line));
    }
    flush_table(&mut table, &mut out);
    if in_fence {
        out.push("```".to_string());
    }
    out.join("\n")
}

fn flush_table(table: &mut Vec<String>, out: &mut Vec<String>) {
    if table.is_empty() {
        return;
    }
    out.push("```".to_string());
    out.append(table);
    out.push("```".to_string());
}

fn inline_mrkdwn(line: &str) -> String {
    let trimmed = line.trim_start();
    if let Some(heading) = trimmed.strip_prefix('#') {
        let text = heading.trim_start_matches('#').trim();
        if !text.is_empty() {
            return format!("*{}*", inline_spans(text));
        }
    }
    let indent = &line[..line.len() - trimmed.len()];
    let body = if let Some(item) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
    {
        format!("• {}", inline_spans(item))
    } else {
        inline_spans(trimmed)
    };
    format!("{indent}{body}")
}

/// Bold, links and inline code, outside of fences.
fn inline_spans(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('`') {
            // Inline code is copied through untouched to its closing tick.
            match after.find('`') {
                Some(end) => {
                    out.push('`');
                    out.push_str(&after[..=end]);
                    rest = &after[end + 1..];
                }
                None => {
                    out.push('`');
                    rest = after;
                }
            }
            continue;
        }
        if let Some(after) = rest.strip_prefix("**")
            && let Some(end) = after.find("**")
        {
            out.push('*');
            out.push_str(&after[..end]);
            out.push('*');
            rest = &after[end + 2..];
            continue;
        }
        if let Some(after) = rest.strip_prefix('[')
            && let Some((label, tail)) = after.split_once("](")
            && let Some(end) = tail.find(')')
            && tail[..end].starts_with("http")
        {
            let url = &tail[..end];
            out.push_str(&format!("<{url}|{label}>"));
            rest = &tail[end + 1..];
            continue;
        }
        let mut chars = rest.chars();
        let ch = chars.next().expect("rest is not empty");
        out.push(ch);
        rest = chars.as_str();
    }
    out
}

/// Cut a reply into messages of at most `limit` characters.
///
/// Cuts land on a blank line where one is near, else on a line break, and
/// never inside a code fence: a fence that opens in one message and closes
/// in the next is two broken messages. A fence that has to be split is
/// closed at the cut and reopened after it.
pub fn chunk(text: &str, limit: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut in_fence = false;
    let mut current_fenced = false;
    for line in text.split_inclusive('\n') {
        let is_fence = line.trim_start().starts_with("```");
        if current.chars().count() + line.chars().count() > limit && !current.is_empty() {
            if current_fenced {
                current.push_str("```\n");
            }
            chunks.push(current.trim_end().to_string());
            current = String::new();
            if current_fenced {
                current.push_str("```\n");
            }
        }
        // One line longer than the limit is cut hard; it has no better place.
        if line.chars().count() > limit {
            let mut piece = String::new();
            for ch in line.chars() {
                if piece.chars().count() + 1 > limit {
                    chunks.push(std::mem::take(&mut piece));
                }
                piece.push(ch);
            }
            current.push_str(&piece);
        } else {
            current.push_str(line);
        }
        if is_fence {
            in_fence = !in_fence;
        }
        current_fenced = in_fence;
    }
    if !current.trim().is_empty() {
        chunks.push(current.trim_end().to_string());
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_control_is_exactly_one_word() {
        assert_eq!(parse_control(" Stop "), Some(Control::Stop));
        assert_eq!(parse_control("new"), Some(Control::New));
        assert_eq!(parse_control("stop what you are doing"), None);
    }

    #[test]
    fn a_verdict_carries_an_id_and_survives_a_phone_keyboard() {
        assert_eq!(
            parse_verdict("Yes ABCDE"),
            Some(("abcde".to_string(), Answer::Yes))
        );
        assert_eq!(
            parse_verdict("no abcde"),
            Some(("abcde".into(), Answer::No))
        );
        assert_eq!(
            parse_verdict("abcde: use the second option"),
            Some(("abcde".into(), Answer::Text("use the second option".into())))
        );
        assert_eq!(parse_verdict("yes"), None, "no id, no verdict");
        assert_eq!(parse_verdict("yes abcl1"), None, "l is not in the alphabet");
        assert_eq!(parse_verdict("yes abcde please"), None);
        assert_eq!(parse_verdict("hello: world"), None);
    }

    #[test]
    fn request_ids_are_five_letters_without_l() {
        for _ in 0..200 {
            let id = new_request_id();
            assert_eq!(id.len(), ID_LEN);
            assert!(looks_like_id(&id), "{id}");
            assert!(!id.contains('l'));
        }
    }

    #[test]
    fn only_the_bots_own_mention_is_stripped() {
        let (text, mentioned) = strip_mention("<@UBOT> fix it with <@UALICE>", "UBOT");
        assert!(mentioned);
        assert_eq!(text, "fix it with <@UALICE>");
        let (text, mentioned) = strip_mention("just talking", "UBOT");
        assert!(!mentioned);
        assert_eq!(text, "just talking");
    }

    #[test]
    fn slack_escaping_is_undone() {
        struct Alice;
        impl Names for Alice {
            fn user_name(&self, id: &str) -> Option<String> {
                (id == "UALICE").then(|| "alice".to_string())
            }
        }
        assert_eq!(
            unescape(
                "ask <@UALICE> in <#C1|general> about <https://x.y|the doc> &amp; 1 &lt; 2",
                &Alice
            ),
            "ask @alice in #general about the doc (https://x.y) & 1 < 2"
        );
        assert_eq!(unescape("<@UNOBODY>", &NoNames), "@UNOBODY");
        assert_eq!(unescape("<!here>", &NoNames), "@here");
        assert_eq!(
            unescape("a < b", &NoNames),
            "a < b",
            "a bare < is not an entity"
        );
    }

    #[test]
    fn markdown_becomes_mrkdwn_outside_fences_and_is_left_alone_inside() {
        let markdown = "## Result\n\n**Fixed** the [parser](https://example.com/p).\n- one\n- `two`\n\n```rust\nlet x = **not bold**;\n```\n";
        let out = to_mrkdwn(markdown);
        assert_eq!(
            out,
            "*Result*\n\n*Fixed* the <https://example.com/p|parser>.\n• one\n• `two`\n\n```\nlet x = **not bold**;\n```"
        );
    }

    #[test]
    fn a_table_becomes_a_code_block_because_slack_cannot_draw_one() {
        let out = to_mrkdwn("| a | b |\n|---|---|\n| 1 | 2 |\nafter");
        assert_eq!(out, "```\n| a | b |\n|---|---|\n| 1 | 2 |\n```\nafter");
    }

    #[test]
    fn an_unclosed_fence_is_closed_so_the_message_renders() {
        assert_eq!(to_mrkdwn("```\ncode"), "```\ncode\n```");
    }

    #[test]
    fn chunks_never_split_a_fence_open_from_its_close() {
        let text = "intro\n```\nline one\nline two\nline three\n```\noutro";
        let chunks = chunk(text, 24);
        for piece in &chunks {
            let fences = piece.matches("```").count();
            assert_eq!(fences % 2, 0, "unbalanced fence in {piece:?}");
        }
        assert_eq!(chunks.join("\n").matches("line").count(), 3);
    }

    #[test]
    fn a_short_reply_is_one_chunk_and_a_long_line_is_cut_hard() {
        assert_eq!(chunk("hello", 100), vec!["hello".to_string()]);
        let long = "x".repeat(250);
        let chunks = chunk(&long, 100);
        assert_eq!(chunks.len(), 3);
        assert!(chunks.iter().all(|c| c.chars().count() <= 100));
    }

    #[test]
    fn silence_is_exact() {
        assert!(is_silent("[SILENT]"));
        assert!(is_silent("  no_reply \n"));
        assert!(!is_silent("SILENT treatment is not an answer"));
    }
}
