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
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const SKILL_FILE: &str = "SKILL.md";
pub const DISABLED_SKILL_FILE: &str = "SKILL.md.disabled";

/// Upper bound on skills collected in one pass. Past this the library is
/// best-effort: a dotfiles repository that symlinks a thousand skills should
/// make the page partial, not slow.
pub const DEFAULT_SCAN_CAP: usize = 500;

/// Only the head of a skill file is read, for its front matter.
const FRONT_MATTER_MAX_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillScope {
    /// Installed for the user, wherever an ecosystem keeps them.
    User,
    /// Checked into the project it belongs to.
    Project,
}

/// One directory that holds skills.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillRoot {
    /// How this root is named in the UI — usually the ecosystem it belongs to.
    pub label: String,
    pub path: PathBuf,
    pub scope: SkillScope,
}

impl SkillRoot {
    pub fn new(label: impl Into<String>, path: impl Into<PathBuf>, scope: SkillScope) -> Self {
        Self {
            label: label.into(),
            path: path.into(),
            scope,
        }
    }
}

/// Where one copy of a skill lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillInstall {
    pub root_label: String,
    pub scope: SkillScope,
    /// The skill's own directory, not the file inside it.
    pub directory: PathBuf,
    pub enabled: bool,
}

impl SkillInstall {
    pub fn enabled_path(&self) -> PathBuf {
        self.directory.join(SKILL_FILE)
    }

    pub fn disabled_path(&self) -> PathBuf {
        self.directory.join(DISABLED_SKILL_FILE)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: Option<String>,
    /// True only when every copy is enabled: a skill half-hidden is a skill
    /// the user cannot rely on, and the toggle should say so.
    pub enabled: bool,
    pub installs: Vec<SkillInstall>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkillCatalog {
    /// Sorted by name, so the list does not reshuffle between scans.
    pub skills: Vec<Skill>,
    pub truncated: bool,
}

pub fn discover(roots: &[SkillRoot]) -> Result<SkillCatalog> {
    discover_with_cap(roots, DEFAULT_SCAN_CAP)
}

pub fn discover_with_cap(roots: &[SkillRoot], cap: usize) -> Result<SkillCatalog> {
    let mut grouped: BTreeMap<String, Skill> = BTreeMap::new();
    let mut truncated = false;

    'roots: for root in roots {
        for directory in skill_directories(&root.path)? {
            let Some(found) = read_skill(&directory) else {
                continue;
            };
            if grouped.len() >= cap && !grouped.contains_key(&found.name) {
                truncated = true;
                break 'roots;
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
            (install.disabled_path(), install.enabled_path())
        } else {
            (install.enabled_path(), install.disabled_path())
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

struct FoundSkill {
    name: String,
    description: Option<String>,
    enabled: bool,
}

/// Immediate subdirectories of a root, sorted, so a capped scan keeps the same
/// entries between runs instead of whichever the filesystem listed first.
fn skill_directories(root: &Path) -> Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        // A root that is not installed is not an error: most users have some
        // of these ecosystems and not others.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("reading {}", root.display())),
    };

    let mut directories: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
        .map(|entry| entry.path())
        .collect();
    directories.sort();
    Ok(directories)
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
    let bytes = std::fs::read(path).ok()?;
    let head = &bytes[..bytes.len().min(FRONT_MATTER_MAX_BYTES)];
    Some(String::from_utf8_lossy(head).into_owned())
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
            let value = value.trim().trim_matches('"').trim_matches('\'');
            if !value.is_empty() {
                values.insert(key.trim().to_ascii_lowercase(), value.to_string());
            }
        }
    }
    values
}
