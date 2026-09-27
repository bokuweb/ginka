//! Project layout decisions that do not need a window.
//!
//! Saved views name their grouping field, while each item carries sparse
//! values. This module turns those two pieces into board columns and roadmap
//! spans before `e1-views` builds any GPUI elements.

use chrono::{Duration, NaiveDate};
use e1_github::{ProjectBoard, ProjectPage, ProjectValue, ProjectView};

/// Merge a following Project page into the board already on screen.
///
/// GitHub returns metadata only on the first request in the native client,
/// but accepting it on a later page keeps this independent of that transport
/// detail and makes alternate [`e1_github::GitHub`] sources straightforward.
pub fn append_page(board: &mut ProjectBoard, mut page: ProjectPage) {
    board.items.append(&mut page.board.items);
    if !page.board.fields.is_empty() {
        board.fields = page.board.fields;
        board.views = page.board.views;
    }
}

/// One ordered Kanban column and the project-item indices inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardColumn {
    /// The option id used to move a card into this column.
    pub option_id: Option<String>,
    /// The selected value, or `None` for items with no value.
    pub name: Option<String>,
    /// Indices into [`ProjectBoard::items`].
    pub items: Vec<usize>,
}

/// Build columns from the saved view's vertical grouping field.
pub fn board_columns(board: &ProjectBoard, view: &ProjectView) -> Vec<BoardColumn> {
    let field_id = view.vertical_group_by.as_deref().or_else(|| {
        board
            .fields
            .iter()
            .find(|field| field.name.eq_ignore_ascii_case("status"))
            .map(|field| field.id.as_str())
    });
    let options = field_id
        .and_then(|id| board.fields.iter().find(|field| field.id == id))
        .map(|field| field.options.clone())
        .unwrap_or_default();
    let mut columns: Vec<BoardColumn> = options
        .into_iter()
        .map(|option| BoardColumn {
            option_id: Some(option.id),
            name: Some(option.name),
            items: Vec::new(),
        })
        .collect();
    let mut ungrouped = Vec::new();
    for index in visible_item_indices(board, view) {
        let item = &board.items[index];
        let value = field_id.and_then(|id| {
            item.fields
                .iter()
                .find_map(|field| (field.field_id == id).then_some(&field.value))
        });
        let name = match value {
            Some(ProjectValue::SingleSelect(name))
            | Some(ProjectValue::Text(name))
            | Some(ProjectValue::Number(name)) => Some(name.clone()),
            Some(ProjectValue::Iteration { title, .. }) => Some(title.clone()),
            _ => None,
        };
        if let Some(name) = name {
            if let Some(column) = columns
                .iter_mut()
                .find(|column| column.name.as_deref() == Some(name.as_str()))
            {
                column.items.push(index);
            } else {
                columns.push(BoardColumn {
                    option_id: None,
                    name: Some(name),
                    items: vec![index],
                });
            }
        } else {
            ungrouped.push(index);
        }
    }
    if !ungrouped.is_empty() {
        columns.push(BoardColumn {
            option_id: None,
            name: None,
            items: ungrouped,
        });
    }
    columns
}

/// Item indices that satisfy the saved view's common GitHub filter clauses.
/// Unknown clauses are left to GitHub's web view instead of hiding rows here.
pub fn visible_item_indices(board: &ProjectBoard, view: &ProjectView) -> Vec<usize> {
    let Some(filter) = view.filter.as_deref() else {
        return (0..board.items.len()).collect();
    };
    let clauses = split_filter(filter);
    board
        .items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            clauses
                .iter()
                .all(|clause| matches_clause(item, clause))
                .then_some(index)
        })
        .collect()
}

fn split_filter(filter: &str) -> Vec<String> {
    let mut clauses = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for character in filter.chars() {
        match character {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    clauses.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        clauses.push(current);
    }
    clauses
}

fn matches_clause(item: &e1_github::ProjectItem, clause: &str) -> bool {
    let (negative, clause) = clause
        .strip_prefix('-')
        .map_or((false, clause), |clause| (true, clause));
    let matched = if let Some((key, value)) = clause.split_once(':') {
        let key = key.to_ascii_lowercase();
        let value = value.trim_matches('"').to_ascii_lowercase();
        match key.as_str() {
            "is" => match value.as_str() {
                "open" => item.state.as_deref() == Some("OPEN"),
                "closed" => item.state.as_deref() == Some("CLOSED"),
                "issue" => item.kind == e1_github::ProjectItemKind::Issue,
                "pr" => item.kind == e1_github::ProjectItemKind::PullRequest,
                _ => true,
            },
            "repo" | "repository" => item
                .repo
                .as_ref()
                .is_some_and(|repo| repo.to_string().to_ascii_lowercase().contains(&value)),
            "status" | "milestone" | "assignee" | "label" | "iteration" => item
                .fields
                .iter()
                .filter(|field| {
                    let field = field.field_name.to_ascii_lowercase();
                    field == key
                        || (key == "assignee" && field == "assignees")
                        || (key == "label" && field == "labels")
                })
                .any(|field| project_value_matches(&field.value, &value)),
            _ => return true,
        }
    } else {
        item.title
            .to_ascii_lowercase()
            .contains(&clause.to_ascii_lowercase())
    };
    if negative { !matched } else { matched }
}

fn project_value_matches(value: &ProjectValue, expected: &str) -> bool {
    match value {
        ProjectValue::SingleSelect(value)
        | ProjectValue::Text(value)
        | ProjectValue::Number(value)
        | ProjectValue::Date(value) => value.to_ascii_lowercase() == expected,
        ProjectValue::Iteration { title, .. } => title.to_ascii_lowercase() == expected,
        ProjectValue::Names(values) => values
            .iter()
            .any(|value| value.to_ascii_lowercase() == expected),
    }
}

/// One Project item placed on a roadmap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoadmapRow {
    /// Index into [`ProjectBoard::items`].
    pub item: usize,
    /// First day occupied by the item.
    pub start: NaiveDate,
    /// Last day occupied by the item, inclusive.
    pub end: NaiveDate,
}

/// The date extent and rows of a native roadmap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Roadmap {
    /// Earliest visible day.
    pub start: NaiveDate,
    /// Latest visible day.
    pub end: NaiveDate,
    /// Dated items in project order.
    pub rows: Vec<RoadmapRow>,
    /// Items with no date or iteration value.
    pub undated: Vec<usize>,
}

/// Place Project items on their date or iteration spans.
pub fn roadmap(board: &ProjectBoard, view: &ProjectView) -> Option<Roadmap> {
    let mut rows = Vec::new();
    let mut undated = Vec::new();
    for index in visible_item_indices(board, view) {
        let item = &board.items[index];
        let dates: Vec<(&str, NaiveDate)> = item
            .fields
            .iter()
            .filter_map(|field| match &field.value {
                ProjectValue::Date(date) => NaiveDate::parse_from_str(date, "%Y-%m-%d")
                    .ok()
                    .map(|date| (field.field_name.as_str(), date)),
                _ => None,
            })
            .collect();
        let iteration = item.fields.iter().find_map(|field| match &field.value {
            ProjectValue::Iteration {
                start_date,
                duration,
                ..
            } => NaiveDate::parse_from_str(start_date, "%Y-%m-%d")
                .ok()
                .map(|start| {
                    (
                        start,
                        start + Duration::days((*duration).saturating_sub(1) as i64),
                    )
                }),
            _ => None,
        });
        let named = |words: &[&str]| {
            dates.iter().find_map(|(name, date)| {
                let name = name.to_ascii_lowercase();
                words
                    .iter()
                    .any(|word| name.contains(word))
                    .then_some(*date)
            })
        };
        let span = if dates.is_empty() {
            iteration
        } else {
            let start =
                named(&["start", "begin", "開始"]).or_else(|| dates.first().map(|(_, d)| *d));
            let end = named(&["target", "end", "due", "期限", "終了"])
                .or_else(|| dates.get(1).map(|(_, d)| *d))
                .or(start);
            start.zip(end).map(|(start, end)| {
                if start <= end {
                    (start, end)
                } else {
                    (end, start)
                }
            })
        };
        if let Some((start, end)) = span {
            rows.push(RoadmapRow {
                item: index,
                start,
                end,
            });
        } else {
            undated.push(index);
        }
    }
    let start = rows.iter().map(|row| row.start).min()?;
    let end = rows.iter().map(|row| row.end).max()?;
    Some(Roadmap {
        start,
        end,
        rows,
        undated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use e1_github::{
        Project, ProjectField, ProjectFieldOption, ProjectFieldValue, ProjectItem, ProjectItemKind,
        ProjectViewLayout,
    };

    fn board() -> ProjectBoard {
        let item = |id: &str, status: Option<&str>, value: ProjectValue| ProjectItem {
            id: id.into(),
            title: id.into(),
            kind: ProjectItemKind::DraftIssue,
            repo: None,
            number: None,
            state: None,
            status: status.map(str::to_string),
            fields: status
                .map(|status| ProjectFieldValue {
                    field_id: "status".into(),
                    field_name: "Status".into(),
                    value: ProjectValue::SingleSelect(status.into()),
                })
                .into_iter()
                .chain([ProjectFieldValue {
                    field_id: "when".into(),
                    field_name: "When".into(),
                    value,
                }])
                .collect(),
            html_url: None,
            archived: false,
        };
        ProjectBoard {
            project: Project {
                id: "p".into(),
                owner: "o".into(),
                title: "P".into(),
                number: 1,
                closed: false,
                html_url: String::new(),
            },
            items: vec![
                item("a", Some("Doing"), ProjectValue::Date("2026-09-10".into())),
                item(
                    "b",
                    None,
                    ProjectValue::Iteration {
                        title: "Sprint".into(),
                        start_date: "2026-09-01".into(),
                        duration: 7,
                    },
                ),
            ],
            fields: vec![ProjectField {
                id: "status".into(),
                name: "Status".into(),
                data_type: "SINGLE_SELECT".into(),
                options: ["Todo", "Doing", "Done"]
                    .into_iter()
                    .enumerate()
                    .map(|(index, name)| ProjectFieldOption {
                        id: index.to_string(),
                        name: name.into(),
                        color: "GRAY".into(),
                    })
                    .collect(),
            }],
            views: Vec::new(),
        }
    }

    #[test]
    fn board_keeps_option_order_and_adds_ungrouped_last() {
        let board = board();
        let view = ProjectView {
            id: "v".into(),
            name: "Board".into(),
            number: 1,
            layout: ProjectViewLayout::Board,
            filter: None,
            group_by: None,
            vertical_group_by: Some("status".into()),
            visible_fields: Vec::new(),
        };
        let columns = board_columns(&board, &view);
        assert_eq!(
            columns
                .iter()
                .map(|column| column.name.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("Todo"), Some("Doing"), Some("Done"), None]
        );
        assert_eq!(columns[1].items, vec![0]);
        assert_eq!(columns[3].items, vec![1]);
    }

    #[test]
    fn following_project_pages_append_items_without_erasing_metadata() {
        let mut first = board();
        let expected_fields = first.fields.clone();
        let expected_views = first.views.clone();
        let mut following = board();
        following.items = vec![following.items.remove(0)];
        following.fields.clear();
        following.views.clear();

        append_page(
            &mut first,
            e1_github::ProjectPage {
                board: following,
                next_cursor: None,
            },
        );

        assert_eq!(first.items.len(), 3);
        assert_eq!(first.fields, expected_fields);
        assert_eq!(first.views, expected_views);
    }

    #[test]
    fn roadmap_uses_dates_and_iteration_spans() {
        let view = ProjectView {
            id: "v".into(),
            name: "Roadmap".into(),
            number: 1,
            layout: ProjectViewLayout::Roadmap,
            filter: None,
            group_by: None,
            vertical_group_by: None,
            visible_fields: Vec::new(),
        };
        let roadmap = roadmap(&board(), &view).unwrap();
        assert_eq!(roadmap.start.to_string(), "2026-09-01");
        assert_eq!(roadmap.end.to_string(), "2026-09-10");
        assert_eq!(roadmap.rows.len(), 2);
    }

    #[test]
    fn saved_filters_apply_supported_positive_and_negative_fields() {
        let board = board();
        let view = ProjectView {
            id: "v".into(),
            name: "Doing".into(),
            number: 1,
            layout: ProjectViewLayout::Board,
            filter: Some("status:Doing -is:closed".into()),
            group_by: None,
            vertical_group_by: Some("status".into()),
            visible_fields: Vec::new(),
        };
        assert_eq!(visible_item_indices(&board, &view), vec![0]);
    }
}
