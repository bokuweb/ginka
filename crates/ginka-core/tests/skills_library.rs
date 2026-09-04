//! N11: the agents' own skills are manageable from the app — discovered
//! wherever each ecosystem keeps them, grouped across duplicate installs, and
//! disabled without deleting anything.

use ginka_core::skills::{self, SkillRoot, SkillScope};
use std::path::Path;

fn write_skill(root: &Path, name: &str, body: &str) {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("SKILL.md"), body).unwrap();
}

fn roots(paths: &[(&str, &Path, SkillScope)]) -> Vec<SkillRoot> {
    paths
        .iter()
        .map(|(label, path, scope)| SkillRoot::new(*label, *path, *scope))
        .collect()
}

#[test]
fn a_directory_with_a_skill_file_is_a_skill() {
    let tmp = tempfile::tempdir().unwrap();
    write_skill(
        tmp.path(),
        "review-diff",
        "---\nname: review-diff\ndescription: Review a diff carefully\n---\n\nBody.\n",
    );

    let catalog = skills::discover(&roots(&[("user", tmp.path(), SkillScope::User)])).unwrap();
    assert_eq!(catalog.skills.len(), 1);
    let skill = &catalog.skills[0];
    assert_eq!(skill.name, "review-diff");
    assert_eq!(
        skill.description.as_deref(),
        Some("Review a diff carefully")
    );
    assert!(skill.enabled);
}

#[test]
fn a_skill_without_front_matter_is_named_after_its_directory() {
    let tmp = tempfile::tempdir().unwrap();
    write_skill(
        tmp.path(),
        "deploy",
        "Just instructions, no front matter.\n",
    );

    let catalog = skills::discover(&roots(&[("user", tmp.path(), SkillScope::User)])).unwrap();
    assert_eq!(catalog.skills[0].name, "deploy");
    assert_eq!(catalog.skills[0].description, None);
}

#[test]
fn a_directory_without_a_skill_file_is_not_a_skill() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("not-a-skill")).unwrap();
    std::fs::write(tmp.path().join("not-a-skill/README.md"), "hi").unwrap();

    let catalog = skills::discover(&roots(&[("user", tmp.path(), SkillScope::User)])).unwrap();
    assert!(catalog.skills.is_empty());
}

#[test]
fn the_same_skill_installed_twice_is_one_entry_with_two_installs() {
    let tmp = tempfile::tempdir().unwrap();
    let claude = tmp.path().join("claude");
    let codex = tmp.path().join("codex");
    write_skill(&claude, "review-diff", "---\nname: review-diff\n---\n");
    write_skill(&codex, "review-diff", "---\nname: review-diff\n---\n");

    let catalog = skills::discover(&roots(&[
        ("claude", claude.as_path(), SkillScope::User),
        ("codex", codex.as_path(), SkillScope::User),
    ]))
    .unwrap();

    assert_eq!(catalog.skills.len(), 1, "one skill, installed twice");
    assert_eq!(catalog.skills[0].installs.len(), 2);
    let labels: Vec<&str> = catalog.skills[0]
        .installs
        .iter()
        .map(|install| install.root_label.as_str())
        .collect();
    assert_eq!(labels, ["claude", "codex"]);
}

#[test]
fn disabling_a_skill_hides_it_from_every_tool_at_once() {
    let tmp = tempfile::tempdir().unwrap();
    let claude = tmp.path().join("claude");
    let codex = tmp.path().join("codex");
    write_skill(&claude, "review-diff", "x");
    write_skill(&codex, "review-diff", "x");
    let roots = roots(&[
        ("claude", claude.as_path(), SkillScope::User),
        ("codex", codex.as_path(), SkillScope::User),
    ]);

    let catalog = skills::discover(&roots).unwrap();
    skills::set_enabled(&catalog.skills[0], false).unwrap();

    // Every tool finds skills by that exact file name, so the rename hides it
    // everywhere without touching the directory or its supporting files.
    assert!(!claude.join("review-diff/SKILL.md").exists());
    assert!(claude.join("review-diff/SKILL.md.disabled").exists());
    assert!(codex.join("review-diff/SKILL.md.disabled").exists());

    let after = skills::discover(&roots).unwrap();
    assert_eq!(after.skills.len(), 1, "a disabled skill is still listed");
    assert!(!after.skills[0].enabled);
}

#[test]
fn re_enabling_puts_it_back() {
    let tmp = tempfile::tempdir().unwrap();
    write_skill(tmp.path(), "review-diff", "x");
    let roots = roots(&[("user", tmp.path(), SkillScope::User)]);

    let catalog = skills::discover(&roots).unwrap();
    skills::set_enabled(&catalog.skills[0], false).unwrap();
    let disabled = skills::discover(&roots).unwrap();
    skills::set_enabled(&disabled.skills[0], true).unwrap();

    assert!(tmp.path().join("review-diff/SKILL.md").exists());
    assert!(skills::discover(&roots).unwrap().skills[0].enabled);
}

#[test]
fn a_partly_disabled_skill_reads_as_disabled_until_every_copy_is_back() {
    let tmp = tempfile::tempdir().unwrap();
    let claude = tmp.path().join("claude");
    let codex = tmp.path().join("codex");
    write_skill(&claude, "review-diff", "x");
    write_skill(&codex, "review-diff", "x");
    std::fs::rename(
        codex.join("review-diff/SKILL.md"),
        codex.join("review-diff/SKILL.md.disabled"),
    )
    .unwrap();

    let catalog = skills::discover(&roots(&[
        ("claude", claude.as_path(), SkillScope::User),
        ("codex", codex.as_path(), SkillScope::User),
    ]))
    .unwrap();
    assert!(
        !catalog.skills[0].enabled,
        "one copy still hidden means the skill is not fully on"
    );
    assert!(catalog.skills[0].installs[0].enabled);
    assert!(!catalog.skills[0].installs[1].enabled);
}

#[test]
fn a_root_that_does_not_exist_is_skipped_rather_than_failing() {
    let tmp = tempfile::tempdir().unwrap();
    write_skill(tmp.path(), "here", "x");

    let catalog = skills::discover(&roots(&[
        ("present", tmp.path(), SkillScope::User),
        (
            "absent",
            Path::new("/definitely/not/here/ginka-test"),
            SkillScope::Project,
        ),
    ]))
    .unwrap();
    assert_eq!(catalog.skills.len(), 1);
}

#[test]
fn scanning_stops_at_the_cap_and_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    for n in 0..10 {
        write_skill(tmp.path(), &format!("skill-{n}"), "x");
    }
    let catalog =
        skills::discover_with_cap(&roots(&[("user", tmp.path(), SkillScope::User)]), 4).unwrap();
    assert_eq!(catalog.skills.len(), 4);
    assert!(catalog.truncated);
}

#[test]
fn skills_are_listed_in_a_stable_order() {
    let tmp = tempfile::tempdir().unwrap();
    write_skill(tmp.path(), "zebra", "x");
    write_skill(tmp.path(), "alpha", "x");

    let names: Vec<String> = skills::discover(&roots(&[("user", tmp.path(), SkillScope::User)]))
        .unwrap()
        .skills
        .into_iter()
        .map(|skill| skill.name)
        .collect();
    assert_eq!(names, ["alpha", "zebra"]);
}

#[test]
fn the_scope_of_each_install_is_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("project");
    write_skill(&project, "deploy", "x");

    let catalog = skills::discover(&roots(&[(
        "project",
        project.as_path(),
        SkillScope::Project,
    )]))
    .unwrap();
    assert_eq!(catalog.skills[0].installs[0].scope, SkillScope::Project);
}
