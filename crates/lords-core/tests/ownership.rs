//! The ownership boundary (design: Ownership boundary), one test per finding of the second Sol review (25 Oct,
//! Harness/notes/2026-10-25-sol-review-lords-command-r2.md). Each states its property; each failed on 83f450e where
//! the finding reproduces there (run there with that commit's version of common/shim.rs).

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;
use common::*;
#[path = "common/shim.rs"]
mod shim;
use lords_core::sync::Action;
use shim::*;
use std::fs;
use std::path::Path;
use std::process::Command;

const BEGIN: &str = "<!-- lords-command:begin -->";
const END: &str = "<!-- lords-command:end -->";

fn expired() -> String {
    "temporary:\n  until: 2000-01-01\n  review_by: 2999-12-31\n  reason: a trial\n".into()
}

/// A skill every run gets (no roles), so a user-level skills target installs it.
fn skill(f: &Fixture, name: &str, extra: &str) {
    f.write(
        &format!("pub/global/skills/{name}/SKILL.md"),
        &format!("---\nname: {name}\ndescription: the {name} skill\n---\nSteps.\n"),
    );
    f.write(
        &format!("pub/global/skills/{name}/lords.yaml"),
        &format!("id: skill.{name}\nkind: skill\ntitle: {name}\naudience: [all]\nscope: global\nstatus: active\nvisibility: public\norigin:\n  type: own\n{extra}"),
    );
}

fn rule(f: &Fixture) {
    f.write("pub/global/rules/r.md", &entry("rule.r", "global", "public", "", "Rule text.\n"));
}

/// The private stack with these `[[targets]]` (TOML).
fn targets(f: &Fixture, toml: &str) {
    f.write(
        "priv/stack.toml",
        &format!("name = \"priv\"\nvisibility = \"private\"\nroles = [\"coder\"]\n{toml}"),
    );
}

fn skills_target(dir: &Path) -> String {
    format!(
        "[[targets]]\nname = \"skills\"\nadapter = \"claude\"\nkind = \"skills\"\npath = '{}'\n",
        dir.display()
    )
}

fn instructions(name: &str, path: &Path) -> String {
    format!("[[targets]]\nname = \"{name}\"\nadapter = \"codex\"\nkind = \"instructions\"\npath = '{}'\n", path.display())
}

/// A repo `beta-core` checked out at `checkout`, Claude's files only.
fn repo(f: &Fixture) -> std::path::PathBuf {
    let checkout = f.path("checkout");
    f.write("checkout/README.md", "a repo\n");
    f.git_init("checkout");
    f.write(
        "priv/stack.toml",
        &format!(
            "name = \"priv\"\nvisibility = \"private\"\nroles = [\"coder\"]\n[repos.beta-core]\ncheckouts = ['{}']\nworktrees = false\nadapters = [\"claude\"]\n",
            checkout.display()
        ),
    );
    checkout
}

/// A dir link (a junction on Windows, a symlink elsewhere).
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

/// A plain copy of a dir (the files only), for staging a look-alike.
fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap().flatten() {
        let (p, q) = (e.path(), to.join(e.file_name()));
        if p.is_dir() {
            copy_dir(&p, &q);
        } else {
            fs::copy(&p, &q).unwrap();
        }
    }
}

fn actions(p: &lords_core::sync::Plan) -> Vec<(String, Action)> {
    p.items.iter().map(|i| (i.path.display().to_string(), i.action.clone())).collect()
}

/// Waits (up to 20 s) for a file or dir whose name starts with `prefix` in `dir`.
fn wait_for(dir: &Path, prefix: &str) -> bool {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while std::time::Instant::now() < until {
        if fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().starts_with(prefix))
        {
            return true;
        }
        std::thread::yield_now();
    }
    false
}

// Spec (R2-01: "Cleanup … acts through ancestor junctions"; property: a reparse point anywhere on the path is
// refused): with the skills dir itself a junction to an unconfigured dir, nothing below it is removed or written.
#[test]
fn r2_01_a_junction_above_never_redirects_a_removal() {
    let f = Fixture::new();
    skill(&f, "old", &expired());
    fs::create_dir_all(f.path("outside")).unwrap();
    dir_link(&f.path("outside/old"), &f.path("pub/global/skills/old"));
    dir_link(&f.path("skills"), &f.path("outside"));
    targets(&f, &skills_target(&f.path("skills")));
    let p = plan_all(&f);
    let _ = apply_plan(&f, &p);
    assert!(f.path("outside/old").exists(), "{:?}", actions(&p));
    assert!(f.path("pub/global/skills/old/SKILL.md").is_file());
}

// Spec (R2-01: "copy refresh … act[s] through ancestor junctions"): a parent swapped for a junction between plan
// and apply stops the refresh; the dir it points at is untouched.
#[test]
fn r2_01_a_junction_above_never_redirects_a_refresh() {
    let f = Fixture::new();
    skill(&f, "demo", "");
    targets(&f, &skills_target(&f.path("skills")));
    apply_plan(&f, &plan_all(&f)).unwrap();
    f.write("pub/global/skills/demo/SKILL.md", "NEW STEPS");
    let p = plan_all(&f);
    fs::rename(f.path("skills"), f.path("parked")).unwrap();
    copy_dir(&f.path("parked"), &f.path("outside"));
    dir_link(&f.path("skills"), &f.path("outside"));
    let _ = apply_plan(&f, &p);
    assert_ne!(fs::read_to_string(f.path("outside/demo/SKILL.md")).unwrap(), "NEW STEPS");
}

// Spec (R2-01: "restore … act[s] through ancestor junctions"): restoring after a parent was swapped for a junction
// deletes nothing it points at.
#[test]
fn r2_01_a_junction_above_never_redirects_a_restore() {
    let f = Fixture::new();
    skill(&f, "demo", "");
    rule(&f);
    targets(
        &f,
        &format!(
            "{}{}",
            skills_target(&f.path("skills")),
            instructions("doc", &f.path("doc/AGENTS.md"))
        ),
    );
    f.write("doc/AGENTS.md", "OWNER\n");
    let backups = apply_plan(&f, &plan_all(&f)).unwrap();
    fs::rename(f.path("skills"), f.path("parked")).unwrap();
    copy_dir(&f.path("parked"), &f.path("outside"));
    dir_link(&f.path("skills"), &f.path("outside"));
    if let Some(b) = backups {
        let _ = restore_run(&f, &b);
    }
    assert!(f.path("outside/demo/SKILL.md").is_file());
}

// Spec (R2-02: "A later target can still lose an owner edit during apply"; property: an owner edit made mid-apply
// survives): an owner file edited while apply writes an earlier, large target keeps the owner's edit.
#[test]
fn r2_02_an_owner_edit_made_mid_apply_survives() {
    let f = Fixture::new();
    rule(&f);
    // Two targets, planned as sync plans them (an item's place is the plan's: design-simplification-r6, rule A):
    // the victim's owner file is updated after the slow one's.
    f.write("out/victim", "OLD");
    targets(
        &f,
        &format!(
            "{}{}",
            instructions("slow", &f.path("out/slow")),
            instructions("victim", &f.path("out/victim"))
        ),
    );
    let mut p = plan_all(&f);
    for i in p.items.iter_mut().filter(|i| i.path == f.path("out/slow")) {
        i.proposed = Some("X".repeat(96 * 1024 * 1024));
    }
    assert!(p.items.iter().any(|i| i.path == f.path("out/victim") && i.action == Action::Update));
    let (dir, victim) = (f.path("out"), f.path("out/victim"));
    let edit = std::thread::spawn(move || {
        assert!(wait_for(&dir, ".slow.lords-"), "the slow write's staging file appeared");
        fs::write(victim, "OWNER EDIT DURING APPLY").unwrap();
    });
    let result = apply_plan(&f, &p);
    edit.join().unwrap();
    assert_eq!(
        fs::read_to_string(f.path("out/victim")).unwrap(),
        "OWNER EDIT DURING APPLY",
        "{result:?}"
    );
}

// Spec (R2-03: "Restore overwrites edits to an updated file"): restore puts an owner file back only while it still
// holds exactly what sync wrote; an edit since stays.
#[test]
fn r2_03_restore_keeps_an_edit_made_after_apply() {
    let f = Fixture::new();
    rule(&f);
    f.write("out/file", "OLD");
    targets(&f, &instructions("t", &f.path("out/file")));
    let backups = apply_plan(&f, &plan_all(&f)).unwrap().expect("a backup");
    f.write("out/file", "OWNER AFTER APPLY");
    let _ = restore_run(&f, &backups);
    assert_eq!(fs::read_to_string(f.path("out/file")).unwrap(), "OWNER AFTER APPLY");
}

// Spec (R2-04: "A rejected atomic write deletes a pre-existing sibling file"; property: sync deletes only what it
// made itself): a file beside the target with the staging name an earlier version used survives.
#[test]
fn r2_04_a_file_sync_did_not_make_is_never_deleted() {
    let f = Fixture::new();
    rule(&f);
    targets(&f, &instructions("t", &f.path("out/file")));
    let temp = format!("out/.file.lords-{}.tmp", std::process::id());
    f.write(&temp, "OWNER TEMP FILE");
    let _ = apply_plan(&f, &plan_all(&f));
    assert_eq!(fs::read_to_string(f.path(&temp)).unwrap(), "OWNER TEMP FILE");
}

// Spec (R2-05: "The clean-Git check still permits unbacked stack data loss before a rejected apply"): an import
// never overwrites a stack file git can't give back (here one git ignores).
#[test]
fn r2_05_an_import_never_overwrites_a_file_git_does_not_keep() {
    let f = Fixture::new();
    f.write(
        "priv/stack.toml",
        &format!("name = \"priv\"\nvisibility = \"private\"\n[[memory_stores]]\nname = \"store\"\npath = '{}'\nscope = \"global\"\n", f.path("incoming").display()),
    );
    f.write("incoming/note.md", "INCOMING");
    f.write("priv/global/memory/store/note.md", "OWNER IGNORED BY GIT");
    git(&f.priv_root(), &["init", "-q"]);
    f.write("priv/.git/info/exclude", "*\n");
    let result = import_apply(&f);
    assert_eq!(f.read("priv/global/memory/store/note.md"), "OWNER IGNORED BY GIT", "{result:?}");
}

// Spec (R2-06: "Copy verification ignores owner files that restore then deletes"): a file added to an installed
// copy (a sidecar, or anything under `.lords/`) makes the copy no longer sync's to touch, and no restore deletes it.
#[test]
fn r2_06_a_file_added_to_a_copy_is_never_deleted() {
    let f = Fixture::new();
    skill(&f, "demo", "");
    rule(&f);
    f.write("doc/AGENTS.md", "OWNER\n");
    targets(
        &f,
        &format!(
            "{}{}",
            skills_target(&f.path("skills")),
            instructions("doc", &f.path("doc/AGENTS.md"))
        ),
    );
    let backups = apply_plan(&f, &plan_all(&f)).unwrap();
    f.write("skills/demo/lords.yaml", "OWNER SIDECAR");
    f.write("skills/demo/.lords/OWNER.txt", "OWNER DATA");
    let p = plan_all(&f);
    let demo = p.items.iter().find(|i| i.path == f.path("skills/demo")).map(|i| i.action.clone());
    assert_eq!(demo, Some(Action::Conflict), "{:?}", actions(&p));
    if let Some(b) = backups {
        let _ = restore_run(&f, &b);
    }
    assert!(
        f.path("skills/demo/lords.yaml").is_file()
            && f.path("skills/demo/.lords/OWNER.txt").is_file()
    );
}

// Spec (R2-07: "The fallback still claims unrelated skill directories from filenames alone"; property: no path
// outside the ledger is ever removed): a dir that shares a stack skill's name and SKILL.md `name` stays.
#[test]
fn r2_07_no_path_outside_the_ledger_is_ever_removed() {
    let f = Fixture::new();
    skill(&f, "demo", &expired());
    targets(&f, &skills_target(&f.path("skills")));
    f.write("skills/demo/SKILL.md", "---\nname: demo\n---\nOWNER'S UNRELATED STEPS\n");
    f.write("skills/other/SKILL.md", "mine\n");
    let p = plan_all(&f);
    let _ = apply_plan(&f, &p);
    assert!(f.path("skills/demo/SKILL.md").is_file(), "{:?}", actions(&p));
    assert!(f.path("skills/other/SKILL.md").is_file());
}

// Spec (R2-08: "The ownership-format delta abandons earlier managed copies"; property: no format inside a copy
// decides ownership): a copy in an earlier manifest format, edited, is a conflict where its skill goes; and one
// whose skill ended is left alone (a human sets it aside), never removed by inference.
#[test]
fn r2_08_a_copy_from_before_the_ledger_is_never_claimed() {
    let f = Fixture::new();
    skill(&f, "demo", "");
    skill(&f, "gone", &expired());
    targets(&f, &skills_target(&f.path("skills")));
    let header = "# Lords Command: a copy of a stack skill, kept by sync (it refreshes and removes it). Edit the stack's copy.";
    for name in ["demo", "gone"] {
        let text = f.read(&format!("pub/global/skills/{name}/SKILL.md"));
        let sha = format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(text.as_bytes()));
        f.write(&format!("skills/{name}/SKILL.md"), &text);
        f.write(&format!("skills/{name}/.lords-copy"), &format!("{header}\n{sha}  SKILL.md\n"));
    }
    f.write("skills/demo/SKILL.md", "OWNER EDIT");
    let p = plan_all(&f);
    let demo = p.items.iter().find(|i| i.path == f.path("skills/demo")).map(|i| i.action.clone());
    assert_eq!(demo, Some(Action::Conflict), "{:?}", actions(&p));
    let _ = apply_plan(&f, &p);
    assert!(f.path("skills/gone/SKILL.md").is_file() && f.path("skills/demo/SKILL.md").is_file());
}

// Spec (R2-09: "Ownership verification reads through known foreign links"): a link inside an installed copy makes
// it a conflict, and what the link points at is never read: changing it changes nothing in the plan.
#[test]
fn r2_09_nothing_is_read_through_a_link() {
    let f = Fixture::new();
    skill(&f, "demo", "");
    f.write("pub/global/skills/demo/resources/x.txt", "OLD");
    targets(&f, &skills_target(&f.path("skills")));
    apply_plan(&f, &plan_all(&f)).unwrap();
    fs::rename(f.path("skills/demo/resources"), f.path("outside")).unwrap();
    dir_link(&f.path("skills/demo/resources"), &f.path("outside"));
    let view = |p: &lords_core::sync::Plan| {
        p.items
            .iter()
            .filter(|i| i.path == f.path("skills/demo"))
            .map(|i| (i.action.clone(), i.notes.clone()))
            .collect::<Vec<_>>()
    };
    let first = view(&plan_all(&f));
    f.write("outside/x.txt", "OWNER CHANGED OUTSIDE");
    let second = view(&plan_all(&f));
    assert_eq!(first, second);
    assert!(first.iter().all(|(a, _)| *a == Action::Conflict), "{first:?}");
}

// Spec (R2-10: "A case-only installed-directory rename uninstalls a desired copy"): one path, however its case is
// spelt (Windows): a case-only rename of an installed copy leaves it installed.
#[test]
fn r2_10_a_case_only_rename_keeps_the_copy() {
    let f = Fixture::new();
    skill(&f, "demo", "roles: [coder]\n");
    let checkout = repo(&f);
    apply_plan(&f, &plan_all(&f)).unwrap();
    let skills = checkout.join(".claude/skills");
    fs::rename(skills.join("demo"), skills.join("staged")).unwrap();
    fs::rename(skills.join("staged"), skills.join("Demo")).unwrap();
    let p = plan_all(&f);
    let _ = apply_plan(&f, &p);
    assert!(
        skills.join("demo/SKILL.md").is_file() || skills.join("Demo/SKILL.md").is_file(),
        "{:?}",
        actions(&p)
    );
}

// Spec (R2-11: "A successful copy install can have a false ownership record immediately"): what apply records is
// what it installed: a source edited mid-install leaves a copy sync still owns (the next plan refreshes it, no
// conflict).
#[test]
fn r2_11_what_is_recorded_is_what_was_installed() {
    let f = Fixture::new();
    skill(&f, "demo", "");
    fs::write(f.path("pub/global/skills/demo/zz-large.dat"), vec![b'X'; 96 * 1024 * 1024]).unwrap();
    targets(&f, &skills_target(&f.path("skills")));
    fs::create_dir_all(f.path("skills")).unwrap();
    let p = plan_all(&f);
    let (dir, edit) = (f.path("skills"), f.path("pub/global/skills/demo/SKILL.md"));
    let watcher = std::thread::spawn(move || {
        // Once the copy's staging dir holds the large file, SKILL.md has been copied already: edit the source then.
        let until = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while std::time::Instant::now() < until {
            let staging = fs::read_dir(&dir).into_iter().flatten().flatten().find(|e| {
                e.file_name().to_string_lossy().starts_with(".demo.lords-")
                    && e.path().join("zz-large.dat").exists()
            });
            if staging.is_some() {
                fs::write(&edit, "NEW SOURCE").unwrap();
                return;
            }
            std::thread::yield_now();
        }
    });
    let result = apply_plan(&f, &p);
    watcher.join().unwrap();
    assert!(result.is_ok(), "{result:?}");
    let again = plan_all(&f);
    let demo =
        again.items.iter().find(|i| i.path == f.path("skills/demo")).map(|i| i.action.clone());
    assert_ne!(demo, Some(Action::Conflict), "{:?}", actions(&again));
}

// Spec (R2-16: "Installing the new commit-msg hook can destroy an unreadable owner hook"): a hook that isn't the
// tool's, readable or not, is never replaced without --force.
#[test]
fn r2_16_an_owner_hook_is_never_replaced_unasked() {
    let f = Fixture::new();
    f.write("repo/README.md", "a repo\n");
    f.git_init("repo");
    let hook = f.path("repo/.git/hooks/commit-msg");
    fs::create_dir_all(hook.parent().unwrap()).unwrap();
    let bytes = [0xFF, 0x4F, 0x57, 0x4E, 0x45, 0x52, 0x20, 0x48, 0x4F, 0x4F, 0x4B];
    fs::write(&hook, bytes).unwrap();
    let result = install_hooks(&f, &f.path("repo"), false);
    assert_eq!(fs::read(&hook).unwrap(), bytes, "{result:?}");
}

// Spec (R2-17: "Backup names still collide across distinct UNC prefixes"; property: every replaced owner file has
// its own backup). No UNC share here, so this checks two local paths a punctuation-stripping name would merge.
#[test]
fn r2_17_every_replaced_owner_file_keeps_its_own_backup() {
    let f = Fixture::new();
    rule(&f);
    f.write("out/x-1/f", "FIRST");
    f.write("out/x1/f", "SECOND");
    targets(
        &f,
        &format!(
            "{}{}",
            instructions("one", &f.path("out/x-1/f")),
            instructions("two", &f.path("out/x1/f"))
        ),
    );
    let backups = apply_plan(&f, &plan_all(&f)).unwrap().expect("a backup");
    let mut kept = Vec::new();
    fn walk(d: &Path, out: &mut Vec<String>) {
        for e in fs::read_dir(d).into_iter().flatten().flatten() {
            if e.path().is_dir() {
                walk(&e.path(), out);
            } else {
                out.push(fs::read_to_string(e.path()).unwrap_or_default());
            }
        }
    }
    walk(&backups, &mut kept);
    assert!(kept.iter().any(|t| t == "FIRST") && kept.iter().any(|t| t == "SECOND"), "{kept:?}");
}

// Spec (R2-18: "Removing the last managed block consumes an owner suffix newline"): removing the block keeps every
// byte around it.
#[test]
fn r2_18_removing_the_block_keeps_the_owner_bytes() {
    let old = format!("OWNER\r\n{BEGIN}\nold\n{END}\r\nTAIL");
    let got = lords_core::compose::place_block(&old, "", false).unwrap();
    assert_eq!(got, "OWNER\r\n\r\nTAIL");
}

// Spec (R2-19: "Already exact owner Git excludes are duplicated"): a line the owner's exclude file already has is
// never added again.
#[test]
fn r2_19_an_exclude_line_already_there_is_not_added() {
    let f = Fixture::new();
    rule(&f);
    f.write(
        "pub/global/rules/dev.md",
        &entry("rule.dev", "global", "public", "roles: [coder]\n", "Dev text.\n"),
    );
    let checkout = repo(&f);
    f.write("checkout/.git/info/exclude", "/CLAUDE.local.md\n");
    let p = plan_all(&f);
    let ex = p
        .items
        .iter()
        .find(|i| i.path.ends_with("info/exclude"))
        .and_then(|i| i.proposed.clone())
        .unwrap_or_default();
    assert_eq!(ex.matches("/CLAUDE.local.md").count(), 1, "{ex}");
    let _ = checkout;
}

// Spec (R2-20: "Equivalent Windows settings excludes are duplicated"): one path, however its separators are spelt,
// is in the settings list once.
#[test]
fn r2_20_a_settings_path_is_listed_once() {
    let f = Fixture::new();
    f.write(
        "settings.json",
        r#"{"owner":1e0,"claudeMdExcludes":["C:\\fixture\\instructions.md"]}"#,
    );
    targets(
        &f,
        &format!("[[targets]]\nname = \"s\"\nadapter = \"claude\"\nkind = \"settings\"\npath = '{}'\nexclude = ['C:/fixture/instructions.md']\n", f.path("settings.json").display()),
    );
    let p = plan_all(&f);
    let item = p.items.iter().find(|i| i.path == f.path("settings.json")).unwrap();
    assert_eq!(item.action, Action::InSync, "{:?}", item.proposed);
}

// Spec (R2-21: "An empty or unchanged apply still writes a lock"): a plan with nothing to change writes nothing.
#[test]
fn r2_21_nothing_to_change_writes_nothing() {
    let f = Fixture::new();
    apply_nothing(&f, &f.path("never-created")).unwrap();
    assert!(!f.path("never-created").exists());
}

// Spec (R2-23: "A source junction silently produces an incomplete successful skill copy"): a stack skill holding a
// link is a conflict, never a copy without what the link points at.
#[test]
fn r2_23_a_skill_holding_a_link_is_a_conflict() {
    let f = Fixture::new();
    skill(&f, "demo", "");
    f.write("outside/needed.txt", "NEEDED");
    dir_link(&f.path("pub/global/skills/demo/resources"), &f.path("outside"));
    targets(&f, &skills_target(&f.path("skills")));
    let p = plan_all(&f);
    assert!(p.items.iter().any(|i| i.action == Action::Conflict), "{:?}", actions(&p));
    assert!(!f.path("skills/demo").exists() || apply_plan(&f, &p).is_err());
}
