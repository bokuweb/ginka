//! The commands a workspace offers after `/`.
//!
//! Read from the filesystem, where the agents themselves keep them:
//! `.claude/commands/**.md` in the project and the same under the user's home.
//! A command is a markdown file whose name is the command and whose frontmatter
//! says what it does.
//!
//! waku also asks each vendor's CLI for its own list, which is per-vendor and
//! changes with their versions. The files are the part every agent agrees on
//! and the part a user can read, so they are the part worth having first.

use ginka_protocol::model::{CommandScope, SlashCommand};
use std::path::{Path, PathBuf};

/// How deep a command directory is walked.
///
/// `commands/frontend/fix.md` is `/frontend/fix`; past a few levels it is a
/// directory tree, not a menu.
const MAX_DEPTH: usize = 3;

/// Every command a workspace offers, project first.
///
/// A project command shadows a user one of the same name: the workspace's own
/// definition is the specific one, and specificity wins.
pub fn discover(worktree: &Path, home: Option<&Path>) -> Vec<SlashCommand> {
    let mut found: Vec<SlashCommand> = Vec::new();
    collect(
        &worktree.join(".claude").join("commands"),
        CommandScope::Project,
        &mut found,
    );
    if let Some(home) = home {
        collect(
            &home.join(".claude").join("commands"),
            CommandScope::User,
            &mut found,
        );
    }

    // Project before user, then by name, and the first of each name wins.
    found.sort_by(|left, right| {
        left.scope
            .cmp(&right.scope)
            .then_with(|| left.name.cmp(&right.name))
    });
    found.dedup_by(|later, kept| later.name == kept.name);
    found.sort_by(|left, right| left.name.cmp(&right.name));
    found
}

/// The commands matching `query`, best first.
///
/// A plain prefix match rather than the fuzzy matcher used for files: a command
/// list is short and typed from the front, and `/t` offering `/frontend/fix`
/// because it contains a `t` is a menu that fights the user.
pub fn search(commands: &[SlashCommand], query: &str) -> Vec<SlashCommand> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return commands.to_vec();
    }
    let mut matched: Vec<SlashCommand> = commands
        .iter()
        .filter(|command| command.name.to_lowercase().contains(&query))
        .cloned()
        .collect();
    // What starts with what was typed comes first; the rest are still offered,
    // because `/fix` should find `frontend/fix`.
    matched.sort_by_key(|command| !command.name.to_lowercase().starts_with(&query));
    matched
}

/// Walk one command directory.
fn collect(root: &Path, scope: CommandScope, into: &mut Vec<SlashCommand>) {
    walk(root, root, scope, 0, into);
}

fn walk(root: &Path, at: &Path, scope: CommandScope, depth: usize, into: &mut Vec<SlashCommand>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(at) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(root, &path, scope, depth + 1, into);
            continue;
        }
        if path.extension().and_then(|extension| extension.to_str()) != Some("md") {
            continue;
        }
        if let Some(command) = read(root, &path, scope) {
            into.push(command);
        }
    }
}

/// Read one command file.
fn read(root: &Path, path: &Path, scope: CommandScope) -> Option<SlashCommand> {
    let name = path
        .strip_prefix(root)
        .ok()?
        .with_extension("")
        .to_string_lossy()
        .replace('\\', "/");
    if name.is_empty() {
        return None;
    }
    let body = std::fs::read_to_string(path).ok()?;
    let (description, argument_hint) = describe(&body);
    Some(SlashCommand {
        name,
        description,
        scope,
        argument_hint,
    })
}

/// What a command file says about itself.
///
/// The frontmatter's `description` and `argument-hint` where there is one,
/// falling back to the first heading or line of prose — a command with no
/// description at all is still worth offering, just with nothing beside it.
fn describe(body: &str) -> (String, Option<String>) {
    let mut description = None;
    let mut hint = None;

    if let Some(rest) = body.strip_prefix("---")
        && let Some((frontmatter, _)) = rest.split_once("\n---")
    {
        {
            for line in frontmatter.lines() {
                let Some((key, value)) = line.split_once(':') else {
                    continue;
                };
                let value = value.trim().trim_matches(['"', '\'']).to_string();
                match key.trim() {
                    "description" => description = Some(value),
                    "argument-hint" | "argument_hint" => hint = Some(value),
                    _ => {}
                }
            }
        }
    }

    let description = description.unwrap_or_else(|| {
        body.lines()
            .skip_while(|line| line.starts_with("---") || line.trim().is_empty())
            .find(|line| !line.trim().is_empty())
            .map(|line| line.trim_start_matches('#').trim().to_string())
            .unwrap_or_default()
    });
    (description, hint.filter(|hint| !hint.is_empty()))
}

/// The command files this build knows where to look for.
pub fn roots(worktree: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let mut roots = vec![worktree.join(".claude").join("commands")];
    roots.extend(home.map(|home| home.join(".claude").join("commands")));
    roots
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn a_command_is_a_file_and_its_frontmatter_says_what_it_does() {
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path().join("repo");
        write(
            &worktree.join(".claude/commands/review.md"),
            "---\ndescription: Review the diff\nargument-hint: <path>\n---\n\nDo the thing.\n",
        );

        let found = discover(&worktree, None);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "review");
        assert_eq!(found[0].description, "Review the diff");
        assert_eq!(found[0].argument_hint.as_deref(), Some("<path>"));
        assert_eq!(found[0].scope, CommandScope::Project);
    }

    #[test]
    fn a_command_with_no_frontmatter_is_still_offered() {
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path().join("repo");
        write(
            &worktree.join(".claude/commands/test.md"),
            "# Run the tests\n\nand report what failed.\n",
        );
        let found = discover(&worktree, None);
        assert_eq!(found[0].description, "Run the tests");
        assert_eq!(found[0].argument_hint, None);
    }

    #[test]
    fn a_command_in_a_directory_carries_the_directory_in_its_name() {
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path().join("repo");
        write(
            &worktree.join(".claude/commands/frontend/fix.md"),
            "Fix it\n",
        );
        assert_eq!(discover(&worktree, None)[0].name, "frontend/fix");
    }

    #[test]
    fn the_users_own_commands_are_offered_too() {
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path().join("repo");
        let home = dir.path().join("home");
        write(
            &worktree.join(".claude/commands/project.md"),
            "In the repo\n",
        );
        write(&home.join(".claude/commands/mine.md"), "Everywhere\n");

        let found = discover(&worktree, Some(&home));
        let names: Vec<&str> = found.iter().map(|command| command.name.as_str()).collect();
        assert_eq!(names, vec!["mine", "project"]);
        assert_eq!(
            found.iter().find(|c| c.name == "mine").unwrap().scope,
            CommandScope::User
        );
    }

    #[test]
    fn a_projects_command_shadows_the_users_command_of_the_same_name() {
        // The workspace's own definition is the specific one.
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path().join("repo");
        let home = dir.path().join("home");
        write(
            &worktree.join(".claude/commands/review.md"),
            "The project's review\n",
        );
        write(
            &home.join(".claude/commands/review.md"),
            "The user's review\n",
        );

        let found = discover(&worktree, Some(&home));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].scope, CommandScope::Project);
        assert_eq!(found[0].description, "The project's review");
    }

    #[test]
    fn a_workspace_with_no_commands_offers_none_rather_than_failing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(discover(&dir.path().join("nothing-here"), None).is_empty());
    }

    #[test]
    fn typing_narrows_the_list_from_the_front() {
        let commands = vec![
            SlashCommand {
                name: "frontend/fix".into(),
                description: String::new(),
                scope: CommandScope::Project,
                argument_hint: None,
            },
            SlashCommand {
                name: "fix".into(),
                description: String::new(),
                scope: CommandScope::Project,
                argument_hint: None,
            },
            SlashCommand {
                name: "test".into(),
                description: String::new(),
                scope: CommandScope::Project,
                argument_hint: None,
            },
        ];

        let found = search(&commands, "fix");
        assert_eq!(found[0].name, "fix", "what starts with it comes first");
        assert_eq!(
            found[1].name, "frontend/fix",
            "and the rest is still offered"
        );
        assert!(search(&commands, "zzz").is_empty());
        assert_eq!(search(&commands, "").len(), 3);
    }

    #[test]
    fn only_markdown_files_are_commands() {
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path().join("repo");
        write(&worktree.join(".claude/commands/real.md"), "yes\n");
        write(&worktree.join(".claude/commands/notes.txt"), "no\n");
        let found = discover(&worktree, None);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "real");
    }
}
