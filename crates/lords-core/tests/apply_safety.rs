//! What `apply` may never do to a file or link (Sol review, 25 Oct, against the apply contract: owner text and
//! bytes survive, every replaced thing has a backup, nothing outside the configured path changes, ownership is
//! proven before a removal, ended entries leave every output). One test per finding; each failed before its fix.

mod common;
use common::*;
use lords_core::sync::{apply, plan, Action, Options, Plan};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BEGIN: &str = "<!-- lords-command:begin -->";
const END: &str = "<!-- lords-command:end -->";
const PAST: &str = "2000-01-01";

/// A rule every run reads, so user-level files have text to compose.
fn rule(f: &Fixture) {
    f.write("pub/global/rules/r.md", &entry("rule.r", "global", "public", "", "Rule text.\n"));
}

/// The private stack with these `[[targets]]` (TOML) and nothing else.
fn targets(f: &Fixture, toml: &str) {
    f.write(
        "priv/stack.toml",
        &format!("name = \"priv\"\nvisibility = \"private\"\nroles = [\"coder\"]\n{toml}"),
    );
}

fn instructions(name: &str, path: &Path, extra: &str) -> String {
    format!(
        "[[targets]]\nname = \"{name}\"\nadapter = \"codex\"\nkind = \"instructions\"\npath = '{}'\n{extra}",
        path.display()
    )
}

fn plan_all(f: &Fixture) -> Plan {
    plan(&f.stacks(), &Options::default()).unwrap()
}

fn item<'p>(p: &'p Plan, path: &Path) -> &'p lords_core::sync::Item {
    p.items
        .iter()
        .find(|i| i.path == path)
        .unwrap_or_else(|| panic!("no item for {}: {:#?}", path.display(), p.items))
}

/// Every file's text under `dir`, recursively.
fn texts(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        let meta = fs::symlink_metadata(&p).unwrap();
        if meta.is_dir() {
            out.extend(texts(&p));
        } else if meta.is_file() {
            out.push(String::from_utf8_lossy(&fs::read(&p).unwrap()).into_owned());
        }
    }
    out
}

/// A dir link: a junction on Windows, a symlink elsewhere.
fn dir_link(link: &Path, to: &Path) {
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    #[cfg(windows)]
    {
        let win = |p: &Path| p.to_string_lossy().replace('/', "\\");
        let out = Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(win(link))
            .arg(win(to))
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }
    #[cfg(not(windows))]
    std::os::unix::fs::symlink(to, link).unwrap();
}

/// A skill in the public stack; `extra` is more sidecar YAML.
fn skill(f: &Fixture, name: &str, extra: &str) -> PathBuf {
    f.write(
        &format!("pub/global/skills/{name}/SKILL.md"),
        &format!("---\nname: {name}\ndescription: the {name} skill\n---\nSteps.\n"),
    );
    f.write(
        &format!("pub/global/skills/{name}/lords.yaml"),
        &format!("id: skill.{name}\nkind: skill\ntitle: {name}\naudience: [all]\nscope: global\nstatus: active\nvisibility: public\norigin:\n  type: own\n{extra}"),
    );
    f.path(&format!("pub/global/skills/{name}"))
}

fn expired() -> String {
    format!("temporary:\n  until: {PAST}\n  review_by: 2999-12-31\n  reason: a trial\n")
}

fn skills_target(dir: &Path) -> String {
    format!(
        "[[targets]]\nname = \"skills\"\nadapter = \"claude\"\nkind = \"skills\"\npath = '{}'\n",
        dir.display()
    )
}

// Spec (finding 1: "Distinct targets overwrite each other's backups"): two targets whose paths differ only in a
// separator both keep their replaced text in the run's backups, and each run gets its own backup dir.
#[test]
fn backups_do_not_collide() {
    let f = Fixture::new();
    rule(&f);
    let (a, b) = (f.write("out/a_b", "FIRST\n"), f.write("out/a/b", "SECOND\n"));
    targets(
        &f,
        &format!(
            "{}{}",
            instructions("one", &a, "own_text = \"replace\"\n"),
            instructions("two", &b, "own_text = \"replace\"\n")
        ),
    );
    let run = f.path("backups/run");
    apply(&plan_all(&f), &run).unwrap();
    let kept = texts(&run).join("\n");
    assert!(kept.contains("FIRST") && kept.contains("SECOND"), "{kept}");
    let (one, two) =
        (lords_core::sync::backup_dir().unwrap(), lords_core::sync::backup_dir().unwrap());
    assert_ne!(one, two, "two applies in one process get two backup dirs");
}

// Spec (finding 2: "Invalid or unreadable targets are treated as absent and overwritten without backup"): a
// target whose bytes aren't UTF-8 is a conflict, and apply leaves its bytes alone.
#[test]
fn invalid_utf8_is_not_a_missing_file() {
    let f = Fixture::new();
    rule(&f);
    let settings = f.path("profile/settings.json");
    let doc = f.path("profile/AGENTS.md");
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    fs::write(&settings, [0xFF, 0x7B, 0x7D]).unwrap();
    fs::write(&doc, [0xFF, 0xFE, 0x41, 0x00]).unwrap();
    targets(
        &f,
        &format!(
            "[[targets]]\nname = \"settings\"\nadapter = \"claude\"\nkind = \"settings\"\npath = '{}'\nexclude = ['C:/generic.md']\n{}",
            settings.display(),
            instructions("doc", &doc, "")
        ),
    );
    let p = plan_all(&f);
    assert_eq!(item(&p, &settings).action, Action::Conflict);
    assert_eq!(item(&p, &doc).action, Action::Conflict);
    assert!(apply(&p, &f.path("backups/run")).is_err());
    assert_eq!(fs::read(&settings).unwrap(), [0xFF, 0x7B, 0x7D]);
}

// Spec (finding 3: "Apply backs up stale planned text, losing owner edits made before the write"): a target that
// changed after planning makes apply refuse, and the owner's edit stays.
#[test]
fn apply_does_not_lose_post_plan_edits() {
    let f = Fixture::new();
    rule(&f);
    let t = f.write("out/AGENTS.md", "OLD");
    targets(&f, &instructions("t", &t, ""));
    let p = plan_all(&f);
    fs::write(&t, "OWNER EDIT AFTER PLAN").unwrap();
    assert!(apply(&p, &f.path("backups/run")).is_err(), "a stale plan is refused");
    assert_eq!(fs::read_to_string(&t).unwrap(), "OWNER EDIT AFTER PLAN");
}

// Spec (finding 4: "A shared `via` file discards all owner text outside any managed block"): the shared file
// keeps its owner's text, as every other target does.
#[test]
fn via_file_keeps_owner_text() {
    let f = Fixture::new();
    rule(&f);
    let shared = f.write("shared.md", "OWNER PREFIX\r\n");
    let profile = f.path("profile/CLAUDE.md");
    targets(
        &f,
        &format!(
            "[[targets]]\nname = \"p\"\nadapter = \"claude\"\nkind = \"instructions\"\npath = '{}'\nvia = '{}'\n",
            profile.display(),
            shared.display()
        ),
    );
    let p = plan_all(&f);
    let proposed = item(&p, &shared).proposed.clone().unwrap();
    assert!(
        proposed.starts_with("OWNER PREFIX\r\n") && proposed.contains("Rule text."),
        "{proposed}"
    );
}

// Spec (finding 5: "Writes escape through existing junctions"): a target whose dir is a link to somewhere else is
// a conflict, and nothing is written through it.
#[test]
fn parent_junction_does_not_redirect_writes() {
    let f = Fixture::new();
    rule(&f);
    fs::create_dir_all(f.path("outside")).unwrap();
    dir_link(&f.path("target"), &f.path("outside"));
    let t = f.path("target/AGENTS.md");
    targets(&f, &instructions("t", &t, ""));
    let p = plan_all(&f);
    assert_eq!(item(&p, &t).action, Action::Conflict, "{:?}", item(&p, &t).notes);
    let _ = apply(&p, &f.path("backups/run"));
    assert!(!f.path("outside/AGENTS.md").exists());
}

// Spec (finding 5: "and modify unconfigured hardlinks"): a target that is a hardlink of another file changes alone;
// the other file keeps its bytes.
#[test]
fn hardlink_does_not_change_unconfigured_file() {
    let f = Fixture::new();
    rule(&f);
    let outside = f.write("outside.md", "OWNER");
    let t = f.path("target.md");
    fs::hard_link(&outside, &t).unwrap();
    targets(&f, &instructions("t", &t, "own_text = \"replace\"\n"));
    apply(&plan_all(&f), &f.path("backups/run")).unwrap();
    assert!(fs::read_to_string(&t).unwrap().contains("Rule text."));
    assert_eq!(fs::read_to_string(&outside).unwrap(), "OWNER");
}

// Spec (finding 6: "A rejected apply has already changed stack files without backups"): when the plan has a
// conflict, `import_and_apply` changes nothing, not even an expired entry in a stack.
#[test]
fn conflict_does_not_first_retire_stack() {
    let f = Fixture::new();
    let gone = f.write(
        "pub/global/rules/gone.md",
        &entry("rule.gone", "global", "public", &expired(), "Old.\n"),
    );
    let before = fs::read_to_string(&gone).unwrap();
    let settings = f.write("profile/settings.json", "{ invalid");
    targets(
        &f,
        &format!(
            "[[targets]]\nname = \"settings\"\nadapter = \"claude\"\nkind = \"settings\"\npath = '{}'\nexclude = ['C:/generic.md']\n",
            settings.display()
        ),
    );
    let r = lords_core::sync::import_and_apply(
        &f.pub_root(),
        Some(&f.priv_root()),
        &Options::default(),
        &f.path("backups/run"),
    );
    assert!(r.is_err());
    assert_eq!(fs::read_to_string(&gone).unwrap(), before, "the stack is untouched");
}

// Spec (finding 7: "Interrupted creation can leave a partial target that re-sync preserves as owner text"): a file
// with a begin marker and no end marker is a conflict, never owner text to append after.
#[test]
fn a_partial_block_is_a_conflict() {
    let f = Fixture::new();
    rule(&f);
    let t = f.write("out/AGENTS.md", &format!("{BEGIN}\n<!-- Generated by Lords"));
    targets(&f, &instructions("t", &t, ""));
    assert_eq!(item(&plan_all(&f), &t).action, Action::Conflict);
}

// Spec (finding 8: "Removed or repointed junctions have no rollback backup"): unlinking records the link and where
// it pointed in the run's backups, so it can be put back.
#[test]
fn removed_junction_has_a_backup() {
    let f = Fixture::new();
    let src = skill(&f, "old", &expired());
    let dir = f.path("home/skills");
    dir_link(&dir.join("old"), &src);
    targets(&f, &skills_target(&dir));
    let p = plan_all(&f);
    assert_eq!(item(&p, &dir.join("old")).action, Action::Unlink);
    let run = f.path("backups/run");
    apply(&p, &run).unwrap();
    let want = src.to_string_lossy().replace('\\', "/").to_lowercase();
    let kept = texts(&run).join("\n").replace('\\', "/").to_lowercase();
    assert!(kept.contains(&want), "the link's target is recorded: {kept}");
}

// Spec (finding 9: "A path-prefix collision makes a foreign junction tool-owned"): a link into a sibling dir whose
// name merely starts with the stack's is not ours: no Unlink.
#[test]
fn sibling_of_stack_is_not_owned() {
    let f = Fixture::new();
    fs::create_dir_all(f.path("pub-other/x")).unwrap();
    let dir = f.path("home/skills");
    dir_link(&dir.join("foreign"), &f.path("pub-other/x"));
    targets(&f, &skills_target(&dir));
    let p = plan_all(&f);
    assert!(
        p.items.iter().all(|i| i.path != dir.join("foreign")
            || !matches!(i.action, Action::Unlink | Action::ReplaceLink)),
        "{:#?}",
        p.items
    );
}

// Spec (finding 10: "An unrelated real directory is removed based only on its basename"): a real dir that merely
// shares a stack skill's name, with files the skill doesn't have, stays.
#[test]
fn unrelated_directory_with_retired_skill_name() {
    let f = Fixture::new();
    skill(&f, "demo", &expired());
    let dir = f.path("home/skills");
    f.write("home/skills/demo/OWNER.txt", "mine\n");
    targets(&f, &skills_target(&dir));
    let p = plan_all(&f);
    assert!(p.items.iter().all(|i| i.action != Action::RemoveDir), "{:#?}", p.items);
}

// Spec (finding 11: "Literal inline marker strings delete owner prose"): markers count only as whole lines; a
// sentence that quotes them is owner text and stays as it is.
#[test]
fn inline_markers_are_owner_bytes() {
    let f = Fixture::new();
    rule(&f);
    let line = format!("Example: {BEGIN} keep this {END} tail\n");
    let t = f.write("out/AGENTS.md", &line);
    targets(&f, &instructions("t", &t, ""));
    let proposed = item(&plan_all(&f), &t).proposed.clone().unwrap();
    assert!(proposed.starts_with(&line) && proposed.contains("Rule text."), "{proposed}");
}

// Spec (finding 18: "Planning a real skill directory follows its foreign sub-junctions"): planning never reads
// through a link inside an installed copy.
#[test]
fn owner_skill_subjunction_is_not_traversed() {
    let f = Fixture::new();
    skill(&f, "demo", "");
    f.write("outside/secret-owner.txt", "secret\n");
    let dir = f.path("home/skills");
    f.write("home/skills/demo/SKILL.md", "---\nname: demo\n---\nOld.\n");
    dir_link(&dir.join("demo/foreign"), &f.path("outside"));
    targets(&f, &skills_target(&dir));
    let p = plan_all(&f);
    let notes = format!("{:?}", p.items.iter().map(|i| &i.notes).collect::<Vec<_>>());
    assert!(!notes.contains("secret-owner"), "{notes}");
}

// Spec (finding 19: "Expired repo skills remain installed and loadable"): a checkout's link to a stack skill that no
// longer applies is unlinked.
#[test]
fn expired_repo_skill_link_is_removed() {
    let f = Fixture::new();
    let src = skill(&f, "demo", &format!("roles: [coder]\n{}", expired()));
    let checkout = f.path("checkout");
    f.write("checkout/README.md", "a repo\n");
    f.git_init("checkout");
    dir_link(&checkout.join(".claude/skills/demo"), &src);
    f.write(
        "priv/stack.toml",
        &format!(
            "name = \"priv\"\nvisibility = \"private\"\nroles = [\"coder\"]\n[repos.beta-core]\ncheckouts = ['{}']\nworktrees = false\nadapters = [\"claude\"]\n",
            checkout.display()
        ),
    );
    let p = plan_all(&f);
    assert_eq!(item(&p, &checkout.join(".claude/skills/demo")).action, Action::Unlink);
}

// Spec (P2 finding 20: "Settings edits reformat unrelated keys and values"): adding the exclude key keeps the rest of
// the file's bytes: its line endings, spacing and number spelling.
#[test]
fn settings_preserve_formatting() {
    let f = Fixture::new();
    let settings =
        f.write("profile/settings.json", "{\r\n\t\"model\" : \"x\", \"number\":1e0\r\n}");
    targets(
        &f,
        &format!(
            "[[targets]]\nname = \"settings\"\nadapter = \"claude\"\nkind = \"settings\"\npath = '{}'\nexclude = ['C:/generic.md']\n",
            settings.display()
        ),
    );
    let proposed = item(&plan_all(&f), &settings).proposed.clone().unwrap();
    assert!(
        proposed.starts_with("{\r\n\t\"model\" : \"x\", \"number\":1e0")
            && proposed.contains("\"claudeMdExcludes\"")
            && proposed.contains("C:/generic.md")
            && !proposed.contains("\n\n")
            && proposed.matches('\n').count() == proposed.matches("\r\n").count(),
        "{proposed:?}"
    );
    let v: serde_json::Value = serde_json::from_str(&proposed).unwrap();
    assert_eq!(v["model"], "x");
}

// Spec (P2 finding 21: "Git excludes are broad patterns rather than exact generated paths"): the files sync writes
// are ignored by their exact paths; a file of the same name deeper down, or another file in `.lords`, is not.
#[test]
fn excludes_are_exact() {
    let f = Fixture::new();
    rule(&f);
    f.write(
        "pub/global/rules/dev.md",
        &entry("rule.dev", "global", "public", "roles: [coder]\n", "Dev text.\n"),
    );
    let checkout = f.path("checkout");
    f.write("checkout/README.md", "a repo\n");
    f.git_init("checkout");
    f.write(
        "priv/stack.toml",
        &format!(
            "name = \"priv\"\nvisibility = \"private\"\nroles = [\"coder\"]\n[repos.beta-core]\ncheckouts = ['{}']\nworktrees = false\n",
            checkout.display()
        ),
    );
    apply(&plan_all(&f), &f.path("backups/run")).unwrap();
    f.write("checkout/nested/AGENTS.override.md", "mine\n");
    f.write("checkout/.lords/owner.txt", "mine\n");
    let ignored = |rel: &str| {
        Command::new("git")
            .arg("-C")
            .arg(&checkout)
            .args(["check-ignore", "-q", rel])
            .status()
            .unwrap()
            .success()
    };
    assert!(ignored("AGENTS.override.md") && ignored(".lords/role-coder.md"));
    assert!(!ignored("nested/AGENTS.override.md") && !ignored(".lords/owner.txt"));
}

// Spec (P2 finding 22: "Re-sync keeps updating when carried tracked instructions contain markers"): a tracked
// AGENTS.md that quotes the markers inline syncs once; the next plan is in sync.
#[test]
fn tracked_marker_text_does_not_grow_each_sync() {
    let f = Fixture::new();
    let checkout = f.path("checkout");
    f.write("checkout/AGENTS.md", &format!("Explain {BEGIN} and {END} literally.\n"));
    f.git_init("checkout");
    f.write(
        "priv/repo/beta-core/rules/x.md",
        &entry("rule.x", "repo:beta-core", "private", "", "Repo rule.\n"),
    );
    f.write(
        "priv/stack.toml",
        &format!(
            "name = \"priv\"\nvisibility = \"private\"\nroles = [\"coder\"]\n[repos.beta-core]\ncheckouts = ['{}']\nworktrees = false\nadapters = [\"codex\"]\n",
            checkout.display()
        ),
    );
    apply(&plan_all(&f), &f.path("backups/one")).unwrap();
    assert_eq!(item(&plan_all(&f), &checkout.join("AGENTS.override.md")).action, Action::InSync);
}

// Spec (P2 finding 23: "Replacing a block changes its owner's suffix newline bytes"): what follows the end marker
// keeps its bytes, CRLF and a missing final newline included; a BOM before the owner text stays.
#[test]
fn marker_suffix_bytes_survive() {
    let f = Fixture::new();
    rule(&f);
    let a = f.write("out/a.md", &format!("\u{feff}owner\r\n{BEGIN}\r\nold\r\n{END}\r\nTAIL"));
    let b = f.write("out/b.md", &format!("owner\n{BEGIN}\nold\n{END}"));
    targets(&f, &format!("{}{}", instructions("a", &a, ""), instructions("b", &b, "")));
    let p = plan_all(&f);
    let pa = item(&p, &a).proposed.clone().unwrap();
    let pb = item(&p, &b).proposed.clone().unwrap();
    assert!(
        pa.starts_with("\u{feff}owner\r\n") && pa.ends_with(&format!("{END}\r\nTAIL")),
        "{pa:?}"
    );
    assert!(pb.ends_with(END) && pb.contains("Rule text."), "{pb:?}");
}

// Spec (P2 finding 24: "An entry still composes on its documented end date"): an entry ends on its `until` date.
#[test]
fn until_date_is_an_end_boundary() {
    let f = Fixture::new();
    f.write(
        "pub/global/rules/t.md",
        &entry(
            "rule.t",
            "global",
            "public",
            "temporary:\n  until: 2026-10-25\n  review_by: 2999-12-31\n  reason: r\n",
            "Ends today.\n",
        ),
    );
    let s = f.stacks();
    let e = s.find("rule.t").unwrap();
    assert!(lords_core::temporary::ended(e, &s, "2026-10-25").is_some());
    assert!(lords_core::temporary::ended(e, &s, "2026-10-24").is_none());
}

// Spec (finding 8, "the backup cannot restore its original link target"; the orchestrator, 25 Oct: backups and
// rollback for every apply): `restore` undoes an apply from its backup dir: a replaced file gets its bytes back, a
// created file goes, a removed link comes back pointing where it did, and a file edited since apply is kept.
#[test]
fn restore_undoes_an_apply() {
    let f = Fixture::new();
    rule(&f);
    let src = skill(&f, "old", &expired());
    let dir = f.path("home/skills");
    dir_link(&dir.join("old"), &src);
    let (kept, new, edited) =
        (f.write("out/kept.md", "OWNER\n"), f.path("out/new.md"), f.path("out/edited.md"));
    targets(
        &f,
        &format!(
            "{}{}{}{}",
            instructions("kept", &kept, "own_text = \"replace\"\n"),
            instructions("new", &new, ""),
            instructions("edited", &edited, ""),
            skills_target(&dir)
        ),
    );
    let run = f.path("backups/run");
    apply(&plan_all(&f), &run).unwrap();
    assert!(!dir.join("old").exists() && new.exists());
    fs::write(&edited, "EDITED AFTER APPLY").unwrap();

    let steps = lords_core::sync::restore(&run).unwrap();
    assert_eq!(fs::read_to_string(&kept).unwrap(), "OWNER\n");
    assert!(!new.exists(), "{steps:?}");
    assert_eq!(fs::read_to_string(&edited).unwrap(), "EDITED AFTER APPLY", "{steps:?}");
    assert!(lords_core::sync::link_points_at(&dir.join("old"), &src), "{steps:?}");
    assert!(src.join("SKILL.md").is_file(), "the stack's skill is untouched");
}
