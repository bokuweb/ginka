//! What a list row and a detail header draw, precomputed.
//!
//! Rows are built once when the data lands, not on every frame: a
//! `uniform_list` asks for its visible rows on every scroll, and formatting
//! a date or parsing a label colour there is work that shows up as jank.

use crate::theme::{Colors, ThemeAppearance, parse_hex};
use crate::time::age;
use chrono::{DateTime, Utc};
use e1_github::{CheckState, Item, Notification, Project, RepoId, State, SubjectKind};
use gpui::{Hsla, SharedString};

/// The mark a row leads with, which is its state.
///
/// A glyph and a colour together, never one alone (`docs/ui.md` §1.6): the
/// colour is what the eye reads first and the shape is what the colour-blind
/// reader reads instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Glyph {
    /// An open pull.
    PullOpen,
    /// A draft pull.
    PullDraft,
    /// A merged pull.
    PullMerged,
    /// A pull closed without merging.
    PullClosed,
    /// An open issue.
    IssueOpen,
    /// A closed issue.
    IssueClosed,
    /// An inbox row about something that is not a pull or an issue.
    Bell,
    /// A GitHub Project.
    Project,
}

/// Which token a glyph is painted with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// `status.done`: open.
    Open,
    /// `text.muted`: a draft, or anything neutral.
    Muted,
    /// `accent`: merged, or a closed issue.
    Accent,
    /// `status.error`: closed without merging.
    Error,
}

impl Role {
    /// Resolve against the installed tokens.
    pub fn color(self, colors: &Colors) -> Hsla {
        match self {
            Self::Open => colors.status_done,
            Self::Muted => colors.text_muted,
            Self::Accent => colors.accent,
            Self::Error => colors.status_error,
        }
    }
}

impl Glyph {
    /// The glyph for an item's state.
    pub fn for_item(item: &Item) -> Self {
        match (item.is_pull(), item.state()) {
            (true, State::Open) => Self::PullOpen,
            (true, State::Draft) => Self::PullDraft,
            (true, State::Merged) => Self::PullMerged,
            (true, State::Closed) => Self::PullClosed,
            (false, State::Closed) => Self::IssueClosed,
            (false, _) => Self::IssueOpen,
        }
    }

    /// The glyph for a notification, which only knows the subject's kind.
    pub fn for_subject(kind: &SubjectKind) -> Self {
        match kind {
            SubjectKind::PullRequest => Self::PullOpen,
            SubjectKind::Issue => Self::IssueOpen,
            _ => Self::Bell,
        }
    }

    /// The icon, as an asset path.
    pub fn icon(self) -> &'static str {
        use crate::assets::icon;
        match self {
            Self::PullOpen => icon::PULL_REQUEST,
            Self::PullDraft => icon::PULL_REQUEST_DRAFT,
            Self::PullMerged => icon::MERGE,
            Self::PullClosed => icon::PULL_REQUEST_CLOSED,
            Self::IssueOpen => icon::CIRCLE_DOT,
            Self::IssueClosed => icon::CIRCLE_CHECK,
            Self::Bell => icon::BELL,
            Self::Project => icon::PROJECT,
        }
    }

    /// The colour role.
    pub fn role(self) -> Role {
        match self {
            Self::PullOpen | Self::IssueOpen => Role::Open,
            Self::PullDraft | Self::Bell | Self::Project => Role::Muted,
            Self::PullMerged | Self::IssueClosed => Role::Accent,
            Self::PullClosed => Role::Error,
        }
    }

    /// The locale key for the state's word, for the detail header.
    pub fn label_key(self) -> &'static str {
        match self {
            Self::PullOpen | Self::IssueOpen | Self::Bell | Self::Project => "state.open",
            Self::PullDraft => "state.draft",
            Self::PullMerged => "state.merged",
            Self::PullClosed | Self::IssueClosed => "state.closed",
        }
    }
}

/// A label, ready to draw as a chip.
#[derive(Debug, Clone, PartialEq)]
pub struct LabelChip {
    /// The text.
    pub name: SharedString,
    /// The label's own colour, for the text and, at a fraction, the fill.
    pub color: Hsla,
}

impl LabelChip {
    /// From the wire's six hex digits. An unparsable colour falls back to
    /// the muted text colour rather than dropping the label.
    pub fn new(name: &str, color: &str, fallback: Hsla) -> Self {
        Self {
            name: name.to_string().into(),
            color: parse_hex(color).unwrap_or(fallback),
        }
    }

    /// The chip's fill and its text, for the theme in force.
    ///
    /// A label's colour is chosen by whoever made it, against GitHub's
    /// background and not ours, so half of them are unreadable if taken as
    /// written: `enhancement` is a pale cyan that vanishes on a light
    /// ground, and a deep blue vanishes on a dark one. The hue is what
    /// carries the meaning, so the hue is kept and the lightness is moved
    /// until the text reads — which is what GitHub itself does.
    pub fn paint(&self, appearance: ThemeAppearance) -> (Hsla, Hsla) {
        let mut fill = self.color;
        let mut text = self.color;
        match appearance {
            ThemeAppearance::Light => {
                fill.l = 0.88;
                fill.a = 1.0;
                text.l = text.l.min(0.32);
                text.s = text.s.max(0.35);
            }
            ThemeAppearance::Dark => {
                fill = self.color.opacity(0.22);
                text.l = text.l.max(0.66);
            }
        }
        (fill, text)
    }
}

/// One row of the centre list, and the head of the detail panel.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemRow {
    /// What opening the row asks for. `None` for an inbox row about
    /// something with no number, which opens on the web instead.
    pub key: Option<(RepoId, u64)>,
    /// The state mark.
    pub glyph: Glyph,
    /// The title.
    pub title: SharedString,
    /// `#12`, or empty for a subject without a number.
    pub number: SharedString,
    /// `owner/name`, for lists that span repositories.
    pub repo: SharedString,
    /// Author and age, or reason and age for an inbox row.
    pub meta: SharedString,
    /// The comment count, when known and non-zero.
    pub comments: Option<u64>,
    /// Up to three labels.
    pub labels: Vec<LabelChip>,
    /// An unread inbox row.
    pub unread: bool,
    /// Where it lives on the web, when known.
    pub html_url: Option<String>,
    /// The author's picture, when the row has an author.
    pub avatar_url: Option<String>,
    /// How the checks stand on a pull, once the store knows.
    pub check: Option<CheckState>,
}

/// How many labels a row shows. Three is what fits beside the meta line at
/// the sidebar's default width; the rest are on the detail.
const LABELS_PER_ROW: usize = 3;

impl ItemRow {
    /// A row for a GitHub Project. It has no native item detail yet, so
    /// opening it follows the project's own GitHub URL.
    pub fn from_project(project: &Project) -> Self {
        Self {
            key: None,
            glyph: Glyph::Project,
            title: project.title.clone().into(),
            number: format!("#{}", project.number).into(),
            repo: project.owner.clone().into(),
            meta: rust_i18n::t!(if project.closed {
                "state.closed"
            } else {
                "state.open"
            })
            .to_string()
            .into(),
            comments: None,
            labels: Vec::new(),
            unread: false,
            html_url: Some(project.html_url.clone()),
            avatar_url: None,
            check: None,
        }
    }

    /// A row for a pull or an issue.
    pub fn from_item(item: &Item, now: DateTime<Utc>, muted: Hsla) -> Self {
        Self {
            key: Some((item.repo.clone(), item.number)),
            glyph: Glyph::for_item(item),
            title: item.title.clone().into(),
            number: format!("#{}", item.number).into(),
            repo: item.repo.to_string().into(),
            meta: format!("{} · {}", item.author.login, age(now, item.updated_at)).into(),
            comments: item.comments.filter(|count| *count > 0),
            labels: item
                .labels
                .iter()
                .take(LABELS_PER_ROW)
                .map(|label| LabelChip::new(&label.name, &label.color, muted))
                .collect(),
            unread: false,
            html_url: Some(item.html_url.clone()),
            avatar_url: Some(item.author.avatar_url.clone()),
            check: None,
        }
    }

    /// The same row, with the checks on it.
    pub fn with_check(mut self, check: Option<CheckState>) -> Self {
        self.check = check;
        self
    }

    /// A row for an inbox entry: the reason where the author would be.
    pub fn from_notification(notification: &Notification, now: DateTime<Utc>) -> Self {
        let key = notification
            .number
            .filter(|_| {
                matches!(
                    notification.kind,
                    SubjectKind::PullRequest | SubjectKind::Issue
                )
            })
            .map(|number| (notification.repo.clone(), number));
        Self {
            key,
            glyph: Glyph::for_subject(&notification.kind),
            title: notification.title.clone().into(),
            number: notification
                .number
                .map(|number| format!("#{number}"))
                .unwrap_or_default()
                .into(),
            repo: notification.repo.to_string().into(),
            meta: format!(
                "{} · {}",
                reason(&notification.reason),
                age(now, notification.updated_at)
            )
            .into(),
            comments: None,
            labels: Vec::new(),
            unread: notification.unread,
            html_url: notification.html_url(),
            avatar_url: None,
            check: None,
        }
    }
}

/// GitHub's reason, in words.
fn reason(reason: &str) -> String {
    let key = match reason {
        "review_requested" => "inbox.reason.review_requested",
        "mention" | "team_mention" => "inbox.reason.mention",
        "assign" => "inbox.reason.assign",
        "author" => "inbox.reason.author",
        "comment" => "inbox.reason.comment",
        "subscribed" => "inbox.reason.subscribed",
        "state_change" => "inbox.reason.state_change",
        "ci_activity" => "inbox.reason.ci_activity",
        _ => "inbox.reason.other",
    };
    rust_i18n::t!(key).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use e1_github::{Kind, Label, Status, User};

    fn item() -> Item {
        Item {
            repo: RepoId::new("bokuweb", "e1"),
            number: 7,
            node_id: String::new(),
            title: "Draw the inbox".into(),
            kind: Kind::Pull {
                draft: false,
                merged: false,
            },
            status: Status::Open,
            author: User {
                login: "alice".into(),
                avatar_url: String::new(),
            },
            created_at: Utc::now(),
            updated_at: Utc::now() - Duration::hours(2),
            comments: Some(3),
            labels: (0..5)
                .map(|n| Label {
                    name: format!("l{n}"),
                    color: "d73a4a".into(),
                    description: None,
                })
                .collect(),
            assignees: Vec::new(),
            requested_reviewers: Vec::new(),
            html_url: "https://github.com/bokuweb/e1/pull/7".into(),
            body: String::new(),
        }
    }

    #[test]
    fn a_row_is_built_once_with_everything_formatted() {
        rust_i18n::set_locale("en");
        let row = ItemRow::from_item(&item(), Utc::now(), gpui::black());
        assert_eq!(row.key, Some((RepoId::new("bokuweb", "e1"), 7)));
        assert_eq!(row.glyph, Glyph::PullOpen);
        assert_eq!(row.number.as_ref(), "#7");
        assert_eq!(row.meta.as_ref(), "alice · 2h");
        assert_eq!(row.comments, Some(3));
        assert_eq!(
            row.labels.len(),
            LABELS_PER_ROW,
            "the rest are on the detail"
        );
    }

    #[test]
    fn a_zero_comment_count_is_not_shown() {
        let mut item = item();
        item.comments = Some(0);
        let row = ItemRow::from_item(&item, Utc::now(), gpui::black());
        assert_eq!(row.comments, None);
    }

    #[test]
    fn a_project_row_keeps_its_owner_state_and_web_destination() {
        rust_i18n::set_locale("en");
        let project = Project {
            id: "PVT_1".into(),
            owner: "bokuweb".into(),
            title: "Roadmap".into(),
            number: 3,
            closed: true,
            html_url: "https://github.com/users/bokuweb/projects/3".into(),
        };
        let row = ItemRow::from_project(&project);
        assert_eq!(row.glyph, Glyph::Project);
        assert_eq!(row.repo.as_ref(), "bokuweb");
        assert_eq!(row.number.as_ref(), "#3");
        assert_eq!(row.meta.as_ref(), "Closed");
        assert_eq!(row.html_url.as_deref(), Some(project.html_url.as_str()));
        assert_eq!(row.key, None, "projects open on the web, not as items");
    }

    #[test]
    fn every_state_has_a_distinct_glyph_and_the_glyphs_do_not_rely_on_colour_alone() {
        let mut item = item();
        assert_eq!(Glyph::for_item(&item), Glyph::PullOpen);
        item.kind = Kind::Pull {
            draft: true,
            merged: false,
        };
        assert_eq!(Glyph::for_item(&item), Glyph::PullDraft);
        item.kind = Kind::Pull {
            draft: false,
            merged: true,
        };
        item.status = Status::Closed;
        assert_eq!(Glyph::for_item(&item), Glyph::PullMerged);
        item.kind = Kind::Pull {
            draft: false,
            merged: false,
        };
        assert_eq!(Glyph::for_item(&item), Glyph::PullClosed);
        item.kind = Kind::Issue;
        assert_eq!(Glyph::for_item(&item), Glyph::IssueClosed);
        item.status = Status::Open;
        assert_eq!(Glyph::for_item(&item), Glyph::IssueOpen);

        // Same colour role, different icon: merged and closed issue share
        // the accent, so the shape is what tells them apart.
        assert_eq!(Glyph::PullMerged.role(), Glyph::IssueClosed.role());
        assert_ne!(Glyph::PullMerged.icon(), Glyph::IssueClosed.icon());
    }

    #[test]
    fn an_inbox_row_opens_only_what_has_a_number_and_a_page() {
        rust_i18n::set_locale("en");
        let mut notification = Notification {
            id: "1".into(),
            unread: true,
            reason: "review_requested".into(),
            updated_at: Utc::now() - Duration::minutes(5),
            repo: RepoId::new("bokuweb", "e1"),
            title: "Fix".into(),
            kind: SubjectKind::PullRequest,
            number: Some(4),
        };
        let row = ItemRow::from_notification(&notification, Utc::now());
        assert_eq!(row.key, Some((RepoId::new("bokuweb", "e1"), 4)));
        assert_eq!(row.meta.as_ref(), "review requested · 5m");
        assert!(row.unread);

        notification.kind = SubjectKind::Release;
        notification.number = None;
        let row = ItemRow::from_notification(&notification, Utc::now());
        assert_eq!(row.key, None);
        assert_eq!(row.glyph, Glyph::Bell);
        assert_eq!(row.number.as_ref(), "");
    }

    #[test]
    fn a_label_with_a_bad_colour_keeps_its_name() {
        let chip = LabelChip::new("bug", "not-a-colour", gpui::black());
        assert_eq!(chip.name.as_ref(), "bug");
        assert_eq!(chip.color, gpui::black());
        let bug = LabelChip::new("bug", "d73a4a", gpui::black());
        let (dark_fill, dark_text) = bug.paint(ThemeAppearance::Dark);
        assert!(dark_fill.a < 0.3, "a tint over the dark glass");
        assert!(dark_text.l >= 0.66, "light enough to read on the dark");
        // A pale label is darkened until it reads on the light theme.
        let pale = LabelChip::new("enhancement", "a2eeef", gpui::black());
        let (light_fill, light_text) = pale.paint(ThemeAppearance::Light);
        assert_eq!(light_fill.a, 1.0);
        assert!(light_text.l <= 0.32, "dark enough to read on the light");
        assert!(
            (light_text.h - light_fill.h).abs() < f32::EPSILON,
            "same hue"
        );
    }
}
