//! The agents' own skills, as a library the user can manage.
//!
//! A skill is a directory holding a `SKILL.md` — reusable instructions any
//! coding agent can load. Every ecosystem keeps its own roots, and installers
//! and dotfiles routinely drop the same skill into several of them, so the
//! catalogue groups by name: one entry per skill, carrying every place it was
//! found. A toggle then applies to all of its copies, which is what people are
//! already doing by hand and getting wrong.
//!
//! Disabling renames `SKILL.md` to `SKILL.md.disabled`. Every tool finds
//! skills by that exact file name, so the rename hides the skill from all of
//! them at once while leaving the directory and its supporting files intact —
//! reversible, and nothing is deleted. See `docs/roadmap.md` §3.3 N11.
//!
//! Discovery reads directories, so it belongs on the background executor. The
//! mutations are one-shot user actions and are a rename per install.

use anyhow::{Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};

/// The file whose presence makes a directory a skill, and every tool looks for.
pub const SKILL_FILE: &str = "SKILL.md";
/// What `SKILL_FILE` is renamed to while the skill is turned off.
pub const DISABLED_SKILL_FILE: &str = "SKILL.md.disabled";

/// Upper bound on distinct skill names collected in one pass. Known names can
/// still collect duplicate installs after this limit, within the directory budget.
pub const DEFAULT_SCAN_CAP: usize = 5_000;

// Allow a full catalog in all three ecosystems under both home and one project.
// Extra copies and non-skill directories count too, bounding retained paths and
// file reads even when a root has many aliases for the same front-matter name.
const DIRECTORY_SCAN_CAP: usize = 30_000;

/// Only the head of a skill file is read, for its front matter.
const FRONT_MATTER_MAX_BYTES: usize = 8 * 1024;

pub use ginka_protocol::model::{Skill, SkillInstall, SkillScope};

/// One directory that holds skills.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillRoot {
    /// How this root is named in the UI — usually the ecosystem it belongs to.
    pub label: String,
    /// The directory whose immediate subdirectories are skills; a missing one is
    /// simply empty.
    pub path: PathBuf,
    /// Whether the root is the user's or a project's, carried onto each install.
    pub scope: SkillScope,
}

impl SkillRoot {
    /// A root as given; nothing is read until [`discover`].
    pub fn new(label: impl Into<String>, path: impl Into<PathBuf>, scope: SkillScope) -> Self {
        Self {
            label: label.into(),
            path: path.into(),
            scope,
        }
    }
}

/// Where an install's `SKILL.md` is while the skill is on.
pub fn enabled_path(install: &SkillInstall) -> PathBuf {
    install.directory.join(SKILL_FILE)
}

/// Where it is while the skill is off.
pub fn disabled_path(install: &SkillInstall) -> PathBuf {
    install.directory.join(DISABLED_SKILL_FILE)
}

/// The skills found across a set of roots, grouped by name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkillCatalog {
    /// Sorted by name, so the list does not reshuffle between scans.
    pub skills: Vec<Skill>,
    /// True when names or directories were omitted at a limit. Install lists
    /// can also be partial when the directory budget is exhausted.
    pub truncated: bool,
}

/// Every root the library reads by default.
///
/// The user's, under `home`, are the ecosystems' own directories — Claude
/// Code's, Codex's, and the shared `~/.agents/skills` several tools read —
/// and each project's are the same three under its checkout. A root that is
/// not there is skipped when read, so listing every ecosystem costs nothing
/// on a machine that has one.
pub fn default_roots(home: Option<&Path>, projects: &[(String, PathBuf)]) -> Vec<SkillRoot> {
    const ECOSYSTEMS: [(&str, &str); 3] = [
        ("claude", ".claude/skills"),
        ("codex", ".codex/skills"),
        ("agents", ".agents/skills"),
    ];
    let mut roots = Vec::new();
    if let Some(home) = home {
        for (label, relative) in ECOSYSTEMS {
            roots.push(SkillRoot::new(label, home.join(relative), SkillScope::User));
        }
    }
    for (name, path) in projects {
        for (_, relative) in ECOSYSTEMS {
            roots.push(SkillRoot::new(
                name.clone(),
                path.join(relative),
                SkillScope::Project,
            ));
        }
    }
    roots
}

/// Read every root with the default cap. Reads directories and front matter,
/// so run it off the UI thread; a missing root is empty, an unreadable one is an error.
pub fn discover(roots: &[SkillRoot]) -> Result<SkillCatalog> {
    discover_with_cap(roots, DEFAULT_SCAN_CAP)
}

/// [`discover`] with an explicit cap on distinct skill names. An install of a
/// name already collected never counts against the cap. At most 30,000 immediate
/// subdirectories are inspected across the roots, in root order and then lexical
/// path order; directory entries are enumerated to keep that selection stable.
/// Each skill file contributes at most 8 KiB of front matter.
pub fn discover_with_cap(roots: &[SkillRoot], cap: usize) -> Result<SkillCatalog> {
    discover_with_limits(roots, cap, DIRECTORY_SCAN_CAP)
}

fn discover_with_limits(
    roots: &[SkillRoot],
    cap: usize,
    directory_cap: usize,
) -> Result<SkillCatalog> {
    let mut grouped: BTreeMap<String, Skill> = BTreeMap::new();
    let mut truncated = false;
    let mut remaining_directories = directory_cap;

    for root in roots {
        let (directories, omitted) = skill_directories(&root.path, remaining_directories)?;
        truncated |= omitted;
        remaining_directories -= directories.len();
        for directory in directories {
            let Some(found) = read_skill(&directory) else {
                continue;
            };
            if grouped.len() >= cap && !grouped.contains_key(&found.name) {
                truncated = true;
                continue;
            }

            let install = SkillInstall {
                root_label: root.label.clone(),
                scope: root.scope,
                directory,
                enabled: found.enabled,
            };
            let entry = grouped.entry(found.name.clone()).or_insert_with(|| Skill {
                name: found.name.clone(),
                description: None,
                enabled: true,
                installs: Vec::new(),
            });
            // The first copy that describes itself wins; a duplicate install
            // with no front matter should not blank the description.
            if entry.description.is_none() {
                entry.description = found.description;
            }
            entry.enabled &= found.enabled;
            entry.installs.push(install);
        }
    }

    Ok(SkillCatalog {
        skills: grouped.into_values().collect(),
        truncated,
    })
}

/// Enable or disable every copy of a skill.
pub fn set_enabled(skill: &Skill, enabled: bool) -> Result<()> {
    for install in &skill.installs {
        let (from, to) = if enabled {
            (disabled_path(install), enabled_path(install))
        } else {
            (enabled_path(install), disabled_path(install))
        };
        if !from.exists() {
            // Already in the state that was asked for.
            continue;
        }
        std::fs::rename(&from, &to)
            .with_context(|| format!("renaming {} to {}", from.display(), to.display()))?;
    }
    Ok(())
}

/// Create one skill in the shared `.agents/skills` root. The directory must
/// be new, so an existing enabled or disabled skill is never overwritten.
pub fn create(root: &Path, name: &str, description: &str, body: &str) -> Result<PathBuf> {
    anyhow::ensure!(
        !name.is_empty()
            && name.len() <= 64
            && name.as_bytes()[0].is_ascii_alphanumeric()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'),
        "skill name must be a lowercase slug of at most 64 characters"
    );
    let description = description.trim();
    anyhow::ensure!(
        !description.is_empty() && description.len() <= 200 && !description.contains(['\n', '\r']),
        "skill description must be one line of at most 200 characters"
    );
    let body = body.trim();
    anyhow::ensure!(
        !body.is_empty() && body.len() <= 64 * 1024,
        "skill body must be 1–65536 bytes"
    );
    for path in [root.parent(), Some(root)].into_iter().flatten() {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) => anyhow::ensure!(
                !metadata.file_type().is_symlink(),
                "skill root component {} must not be a symlink",
                path.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("checking {}", path.display()));
            }
        }
    }
    std::fs::create_dir_all(root).with_context(|| format!("creating {}", root.display()))?;
    let directory = root.join(name);
    std::fs::create_dir(&directory)
        .with_context(|| format!("creating {} (skill may already exist)", directory.display()))?;
    let path = directory.join(SKILL_FILE);
    // A JSON string is also a valid YAML double-quoted scalar. Quoting keeps
    // colons, comment markers and quotation marks in the description intact.
    let description = serde_json::to_string(description)?;
    let contents = format!("---\nname: {name}\ndescription: {description}\n---\n\n{body}\n");
    if let Err(error) = std::fs::write(&path, contents) {
        let _ = std::fs::remove_dir(&directory);
        return Err(error).with_context(|| format!("writing {}", path.display()));
    }
    Ok(path)
}

#[cfg(test)]
mod creation_tests {
    use super::*;

    #[test]
    fn creates_discoverable_skill_without_replacing_an_existing_copy() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".agents/skills");
        let created = create(
            &root,
            "release-notes",
            "Write release notes",
            "Summarize changes.",
        )
        .unwrap();
        assert_eq!(created, root.join("release-notes/SKILL.md"));
        let catalog = discover(&[SkillRoot::new("agents", &root, SkillScope::User)]).unwrap();
        assert_eq!(catalog.skills[0].name, "release-notes");
        assert_eq!(
            catalog.skills[0].description.as_deref(),
            Some("Write release notes")
        );
        assert!(create(&root, "release-notes", "Changed", "Overwrite").is_err());
        assert!(
            std::fs::read_to_string(created)
                .unwrap()
                .contains("Summarize changes.")
        );
    }

    #[test]
    fn rejects_path_traversal_and_front_matter_injection() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["../outside", "a/b", "-bad", "", "a b"] {
            assert!(
                create(dir.path(), name, "Description", "Body").is_err(),
                "{name}"
            );
        }
        assert!(create(dir.path(), "safe", "fine\nname: injected", "Body").is_err());
        assert!(create(dir.path(), "safe", "Description", "  ").is_err());
        assert!(!dir.path().join("safe").exists());
    }

    #[test]
    fn quoted_description_survives_discovery() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".agents/skills");
        create(&root, "release", "Fix: \"quotes\" #1", "Body").unwrap();
        let catalog = discover(&[SkillRoot::new("agents", &root, SkillScope::User)]).unwrap();
        assert_eq!(
            catalog.skills[0].description.as_deref(),
            Some("Fix: \"quotes\" #1")
        );
    }

    #[cfg(unix)]
    #[test]
    fn refuses_to_create_through_a_symlinked_skill_root() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        symlink(&outside, dir.path().join(".agents")).unwrap();
        let root = dir.path().join(".agents/skills");
        assert!(create(&root, "escaped", "Description", "Body").is_err());
        assert!(!outside.join("skills/escaped/SKILL.md").exists());
    }
}

#[cfg(test)]
mod discovery_tests {
    use super::*;

    #[test]
    fn directory_budget_keeps_a_stable_prefix_across_roots() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        for (root, name) in [(&first, "zebra"), (&first, "alpha"), (&second, "beta")] {
            create(root, name, "Description", "Body").unwrap();
        }
        let roots = [
            SkillRoot::new("first", &first, SkillScope::User),
            SkillRoot::new("second", &second, SkillScope::Project),
        ];
        let catalog = discover_with_limits(&roots, 10, 1).unwrap();
        assert!(catalog.truncated);
        assert_eq!(catalog.skills.len(), 1);
        assert_eq!(catalog.skills[0].name, "alpha");
        let catalog = discover_with_limits(&roots, 10, 2).unwrap();
        assert!(catalog.truncated);
        assert_eq!(catalog.skills.len(), 2);
        let catalog = discover_with_limits(&roots, 10, 3).unwrap();
        assert!(!catalog.truncated);
        assert_eq!(catalog.skills.len(), 3);
    }

    #[test]
    fn non_skill_directories_consume_the_budget_but_regular_files_do_not() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("a-empty")).unwrap();
        std::fs::write(dir.path().join("a-file"), "Not a directory").unwrap();
        create(dir.path(), "z-skill", "Description", "Body").unwrap();
        let roots = [SkillRoot::new("user", dir.path(), SkillScope::User)];
        let catalog = discover_with_limits(&roots, 10, 1).unwrap();
        assert!(catalog.truncated);
        assert!(catalog.skills.is_empty());
        let catalog = discover_with_limits(&roots, 10, 2).unwrap();
        assert!(!catalog.truncated);
        assert_eq!(catalog.skills.len(), 1);
    }

    #[test]
    fn empty_roots_and_exact_name_limit_are_not_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let roots = [SkillRoot::new("user", dir.path(), SkillScope::User)];
        assert!(!discover_with_limits(&roots, 0, 0).unwrap().truncated);
        create(dir.path(), "one", "Description", "Body").unwrap();
        assert!(!discover_with_limits(&roots, 1, 1).unwrap().truncated);
        let catalog = discover_with_limits(&roots, 0, 1).unwrap();
        assert!(catalog.truncated);
        assert!(catalog.skills.is_empty());
    }

    #[test]
    fn front_matter_reader_does_not_consume_the_rest_of_a_large_file() {
        let mut input = std::io::Cursor::new(vec![b'x'; FRONT_MATTER_MAX_BYTES * 2]);
        let head = read_head_from(&mut input).unwrap();
        assert_eq!(head.len(), FRONT_MATTER_MAX_BYTES);
        assert_eq!(input.position(), FRONT_MATTER_MAX_BYTES as u64);
    }
}

struct FoundSkill {
    name: String,
    description: Option<String>,
    enabled: bool,
}

/// Immediate subdirectories of a root, sorted, so a capped scan keeps the same
/// entries between runs instead of whichever the filesystem listed first.
fn skill_directories(root: &Path, cap: usize) -> Result<(Vec<PathBuf>, bool)> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        // A root that is not installed is not an error: most users have some
        // of these ecosystems and not others.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((Vec::new(), false));
        }
        Err(error) => return Err(error).with_context(|| format!("reading {}", root.display())),
    };

    let mut directories = BTreeSet::new();
    let mut truncated = false;
    for path in entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
        .map(|entry| entry.path())
    {
        if cap == 0 {
            return Ok((Vec::new(), true));
        }
        directories.insert(path);
        if directories.len() > cap {
            directories.pop_last();
            truncated = true;
        }
    }
    Ok((directories.into_iter().collect(), truncated))
}

fn read_skill(directory: &Path) -> Option<FoundSkill> {
    let enabled_path = directory.join(SKILL_FILE);
    let (path, enabled) = if enabled_path.is_file() {
        (enabled_path, true)
    } else {
        let disabled = directory.join(DISABLED_SKILL_FILE);
        if disabled.is_file() {
            (disabled, false)
        } else {
            return None;
        }
    };

    let head = read_head(&path).unwrap_or_default();
    let front_matter = parse_front_matter(&head);
    let name = front_matter
        .get("name")
        .cloned()
        .unwrap_or_else(|| directory_name(directory));

    Some(FoundSkill {
        name,
        description: front_matter.get("description").cloned(),
        enabled,
    })
}

fn directory_name(directory: &Path) -> String {
    directory
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn read_head(path: &Path) -> Option<String> {
    read_head_from(std::fs::File::open(path).ok()?).ok()
}

fn read_head_from(reader: impl Read) -> std::io::Result<String> {
    let mut bytes = Vec::new();
    reader
        .take(FRONT_MATTER_MAX_BYTES as u64)
        .read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// The `key: value` block between the leading `---` fences. Deliberately not a
/// YAML parser: a skill file's front matter is flat, and a real parser here
/// would be a dependency and a surface for surprises.
fn parse_front_matter(text: &str) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return values;
    }
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            let value = value.trim();
            let decoded = value
                .starts_with('"')
                .then(|| serde_json::from_str::<String>(value).ok())
                .flatten()
                .unwrap_or_else(|| value.trim_matches('"').trim_matches('\'').to_string());
            if !decoded.is_empty() {
                values.insert(key.trim().to_ascii_lowercase(), decoded);
            }
        }
    }
    values
}

// ---------------------------------------------------------------------------
// The skills Ginka ships: how an agent drives Ginka's own CLI (roadmap M4).

/// One skill that ships in the binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bundled {
    /// Directory name it installs under, and the skill's name.
    pub name: &'static str,
    /// The whole `SKILL.md`.
    pub text: &'static str,
}

/// The skills that teach an agent to drive `ginka`: start another agent,
/// follow a session, use a workspace terminal, and run a review loop.
pub fn bundled() -> [Bundled; 4] {
    [
        Bundled {
            name: "ginka-start",
            text: include_str!("../../../skills/ginka-start/SKILL.md"),
        },
        Bundled {
            name: "ginka-chat",
            text: include_str!("../../../skills/ginka-chat/SKILL.md"),
        },
        Bundled {
            name: "ginka-terminal",
            text: include_str!("../../../skills/ginka-terminal/SKILL.md"),
        },
        Bundled {
            name: "ginka-loop",
            text: include_str!("../../../skills/ginka-loop/SKILL.md"),
        },
    ]
}

/// What installing one skill did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Installed {
    /// It was not there, or was replaced.
    Written,
    /// It was there already, word for word.
    Unchanged,
    /// A different `SKILL.md` was there — the reader's edit, or another
    /// skill of that name — or the reader turned it off; it was left alone.
    Kept,
}

/// Install the bundled skills into one skills directory (`~/.claude/skills`
/// and the like): `<root>/<name>/SKILL.md`. A different file already there is
/// kept unless `force`, and a skill the reader disabled stays disabled.
pub fn install_bundled(root: &Path, force: bool) -> Result<Vec<(&'static str, Installed)>> {
    let mut outcomes = Vec::new();
    for skill in bundled() {
        let directory = root.join(skill.name);
        let path = directory.join(SKILL_FILE);
        let outcome = if directory.join(DISABLED_SKILL_FILE).exists() {
            Installed::Kept
        } else {
            match std::fs::read_to_string(&path) {
                Ok(text) if text == skill.text => Installed::Unchanged,
                Ok(_) if !force => Installed::Kept,
                _ => {
                    std::fs::create_dir_all(&directory)
                        .with_context(|| format!("creating {}", directory.display()))?;
                    std::fs::write(&path, skill.text)
                        .with_context(|| format!("writing {}", path.display()))?;
                    Installed::Written
                }
            }
        };
        outcomes.push((skill.name, outcome));
    }
    Ok(outcomes)
}

#[cfg(test)]
mod bundled_tests {
    use super::*;

    #[test]
    fn every_bundled_skill_names_itself_and_says_when_to_use_it() {
        for skill in bundled() {
            assert!(
                skill
                    .text
                    .starts_with(&format!("---\nname: {}\n", skill.name)),
                "{}",
                skill.name
            );
            assert!(skill.text.contains("\ndescription: "), "{}", skill.name);
            assert!(
                skill.text.contains("ginka "),
                "{} names the CLI",
                skill.name
            );
        }
    }

    #[test]
    fn installing_writes_once_keeps_an_edit_and_forces_only_when_asked() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".claude/skills");
        let first = install_bundled(&root, false).unwrap();
        assert!(
            first
                .iter()
                .all(|(_, outcome)| *outcome == Installed::Written)
        );
        let start = root.join("ginka-start/SKILL.md");
        assert_eq!(std::fs::read_to_string(&start).unwrap(), bundled()[0].text);

        let again = install_bundled(&root, false).unwrap();
        assert!(
            again
                .iter()
                .all(|(_, outcome)| *outcome == Installed::Unchanged)
        );

        std::fs::write(&start, "my own version").unwrap();
        let kept = install_bundled(&root, false).unwrap();
        assert_eq!(kept[0], ("ginka-start", Installed::Kept));
        assert_eq!(std::fs::read_to_string(&start).unwrap(), "my own version");

        let forced = install_bundled(&root, true).unwrap();
        assert_eq!(forced[0], ("ginka-start", Installed::Written));
        assert_eq!(std::fs::read_to_string(&start).unwrap(), bundled()[0].text);
    }

    #[test]
    fn a_disabled_bundled_skill_is_not_turned_back_on() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("skills");
        install_bundled(&root, false).unwrap();
        let skill = root.join("ginka-chat");
        std::fs::rename(skill.join(SKILL_FILE), skill.join(DISABLED_SKILL_FILE)).unwrap();
        let outcomes = install_bundled(&root, true).unwrap();
        assert_eq!(outcomes[1], ("ginka-chat", Installed::Kept));
        assert!(!skill.join(SKILL_FILE).exists(), "still off");
    }
}
