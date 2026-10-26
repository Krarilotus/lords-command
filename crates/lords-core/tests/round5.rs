//! One test per finding of the fifth Sol review (25 Oct, Harness/notes/2026-10-25-sol-review-lords-command-r5.md)
//! and per variant the class inventories name (orchestrator, 25 Oct: "close each class with an inventory"), each
//! stating its property. Committed before the fixes and run red on their parent, d429251. The reproductions build
//! on the reviewer's (Harness/scratch/lords-command-eff/r5/review_r5.rs). The lint's tables are in fsguard_lint.rs,
//! the hard-link seam (R4-04) is a unit test in sync.rs.

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;
use common::*;
use lords_core::finding::Severity;
use lords_core::{fsguard, ledger::Ledger, sync};
use std::{fs, path::Path};

fn declare(f: &Fixture, body: &str) {
    f.write(
        "priv/stack.toml",
        &format!("name = \"priv\"\nvisibility = \"private\"\nroles = [\"coder\"]\n{body}"),
    );
}

fn forge(f: &Fixture, path: &Path, kind: &str, scope: &str) {
    let sha = if kind == "dir" {
        fsguard::tree_sha(&fsguard::read_tree(path).unwrap())
    } else {
        fsguard::sha(&fs::read(path).unwrap())
    };
    let _ = sha;
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    common::forge(&f.home(), path.parent().unwrap(), &[&name], kind, scope);
}

fn skill(f: &Fixture, stack: &str, name: &str) {
    let visibility = if stack == "pub" { "public" } else { "private" };
    f.write(
        &format!("{stack}/global/skills/{name}/SKILL.md"),
        &format!("---\nname: {name}\ndescription: the {name} skill\n---\nSteps.\n"),
    );
    f.write(
        &format!("{stack}/global/skills/{name}/lords.yaml"),
        &format!("id: skill.{name}\nkind: skill\ntitle: {name}\naudience: [all]\nscope: global\nstatus: active\nvisibility: {visibility}\norigin:\n  type: own\n"),
    );
}

fn plan_apply(f: &Fixture, opts: &sync::Options) -> Result<Vec<String>, String> {
    sync::plan(&f.stacks(), opts).and_then(|p| sync::apply(&p).map(|a| a.done))
}

// ---------------------------------------------------------------- placement

// Spec (R5-01: "The new placement validator still accepts an ordinary directory as a repo root"; decision 1: a
// configured checkout is a root only if git's own top level is exactly that path): a record in a configured dir
// that is no git checkout refuses, and the owner's file stays.
#[test]
fn r5_01_a_record_in_a_dir_that_is_no_checkout_refuses() {
    let f = Fixture::new();
    let owner = f.write("ordinary/CLAUDE.local.md", "OWNER");
    declare(
        &f,
        &format!(
            "[repos.beta-core]\ncheckouts = ['{}']\nworktrees = false\nadapters = [\"claude\"]\n",
            f.path("ordinary").display()
        ),
    );
    forge(&f, &owner, "file", "repo:beta-core/claude");
    let result = plan_apply(&f, &sync::Options { uninstall: true, ..f.opts() });
    assert_eq!(fs::read_to_string(&owner).ok().as_deref(), Some("OWNER"), "{result:?}");
    assert!(result.is_err());
}

// Spec (R5-01, the writer: "A fresh plan can also create place files there without any forged record"): sync
// writes nothing into a configured checkout that is no git checkout; the plan says why.
#[test]
fn r5_01_sync_writes_nothing_into_a_dir_that_is_no_checkout() {
    let f = Fixture::new();
    f.write("pub/global/rules/r.md", &entry("rule.r", "global", "public", "", "Rule text.\n"));
    fs::create_dir_all(f.path("ordinary")).unwrap();
    declare(
        &f,
        &format!(
            "[repos.beta-core]\ncheckouts = ['{}']\nworktrees = false\nadapters = [\"claude\"]\n",
            f.path("ordinary").display()
        ),
    );
    let result = plan_apply(&f, &f.opts());
    assert!(result.is_err(), "{result:?}");
    assert!(!f.path("ordinary/CLAUDE.local.md").exists() && !f.path("ordinary/.lords").exists());
}

// Spec (R5-01: "A directory beneath a checkout also needs an exact-root check"): a configured checkout that is a
// subdir of a repo is no checkout root: refused, and nothing is written there.
#[test]
fn r5_01_a_subdir_of_a_checkout_is_no_root() {
    let f = Fixture::new();
    f.write("pub/global/rules/r.md", &entry("rule.r", "global", "public", "", "Rule text.\n"));
    f.write("repo/sub/README.md", "a dir in a repo\n");
    f.git_init("repo");
    declare(
        &f,
        &format!(
            "[repos.beta-core]\ncheckouts = ['{}']\nworktrees = false\nadapters = [\"claude\"]\n",
            f.path("repo/sub").display()
        ),
    );
    let result = plan_apply(&f, &f.opts());
    assert!(result.is_err(), "{result:?}");
    assert!(!f.path("repo/sub/CLAUDE.local.md").exists());
}

// Spec (R5-02: "A skill slot accepts directory names no valid skill writer can produce"; decision 2: the skill
// name domain is the schema's slug): a dir record in a skills target whose name no skill id can have refuses.
#[test]
fn r5_02_a_name_no_skill_can_have_is_no_target_skill_slot() {
    let f = Fixture::new();
    f.write("skills/Owner Notes/note.txt", "OWNER");
    let dir = f.path("skills/Owner Notes");
    declare(&f, &format!("[[targets]]\nname = \"skills\"\nadapter = \"claude\"\nkind = \"skills\"\npath = '{}'\n", f.path("skills").display()));
    forge(&f, &dir, "dir", "target:skills");
    let result = plan_apply(&f, &f.opts());
    assert_eq!(
        fs::read_to_string(dir.join("note.txt")).ok().as_deref(),
        Some("OWNER"),
        "{result:?}"
    );
    assert!(result.is_err());
}

// Spec (R5-02, the adapter variant: "The adapter skill slot uses the same unchecked-name construction"): the same
// in a checkout's skills dir.
#[test]
fn r5_02_a_name_no_skill_can_have_is_no_adapter_skill_slot() {
    let f = Fixture::new();
    f.write("checkout/README.md", "a repo\n");
    f.git_init("checkout");
    f.write("checkout/.claude/skills/Owner Notes/note.txt", "OWNER");
    let dir = f.path("checkout/.claude/skills/Owner Notes");
    declare(
        &f,
        &format!(
            "[repos.beta-core]\ncheckouts = ['{}']\nworktrees = false\nadapters = [\"claude\"]\n",
            f.path("checkout").display()
        ),
    );
    forge(&f, &dir, "dir", "repo:beta-core/claude");
    let result = plan_apply(&f, &sync::Options { uninstall: true, ..f.opts() });
    assert_eq!(
        fs::read_to_string(dir.join("note.txt")).ok().as_deref(),
        Some("OWNER"),
        "{result:?}"
    );
    assert!(result.is_err());
}

// Spec (R5-06: "The refactor drops all but the first target with a repeated name"; decision 6: target names are
// unique, so `target:<name>` names one target): a private stack with two targets of one name doesn't load.
#[test]
fn r5_06_two_targets_of_one_name_are_refused() {
    let f = Fixture::new();
    let mut toml = String::new();
    for dir in ["one", "two"] {
        toml.push_str(&format!("[[targets]]\nname = \"skills\"\nadapter = \"claude\"\nkind = \"skills\"\npath = '{}'\n", f.path(dir).display()));
    }
    declare(&f, &toml);
    let err =
        lords_core::Stacks::load(&f.pub_root(), Some(&f.priv_root())).map(|_| ()).unwrap_err();
    assert!(err.contains("skills") && err.contains("unique"), "{err}");
}

// ---------------------------------------------------------------- swallowed errors

// Spec (R5-04: "An unreadable state-directory listing defeats the interrupted-save refusal"; decision 4: only a
// missing state dir means no ledger): with the listing of the state dir denied, loading refuses.
#[cfg(windows)]
#[test]
fn r5_04_an_unlistable_state_dir_refuses() {
    use std::process::Command;
    let f = Fixture::new();
    let state = f.path("lords-home/state");
    Ledger::default().save(&f.home()).unwrap();
    fs::rename(state.join("ledger.toml"), state.join(".ledger.toml.lords-old-probe")).unwrap();
    let who = Command::new("whoami").output().unwrap();
    let account = String::from_utf8(who.stdout).unwrap().trim().to_owned();
    let deny = Command::new("icacls")
        .arg(&state)
        .args(["/deny", &format!("{account}:(RD)")])
        .output()
        .unwrap();
    assert!(deny.status.success(), "{}", String::from_utf8_lossy(&deny.stderr));
    let result = Ledger::load(&f.home()).map(|l| l.owned.len());
    let undo = Command::new("icacls").arg(&state).args(["/remove:d", &account]).output().unwrap();
    assert!(undo.status.success());
    assert!(result.is_err(), "an unreadable listing was taken for no old ledger: {result:?}");
}

// Spec (decision 4): a missing state dir is no ledger, and loads as an empty one.
#[test]
fn r5_04_a_missing_state_dir_is_no_ledger() {
    let f = Fixture::new();
    assert_eq!(Ledger::load(&f.path("no-home-yet")).map(|l| l.owned.len()), Ok(0));
}

// ---------------------------------------------------------------- attributes

fn hide(path: &Path) {
    #[cfg(windows)]
    assert!(std::process::Command::new("attrib").arg("+h").arg(path).status().unwrap().success());
    #[cfg(not(windows))]
    {
        let mut p = fs::metadata(path).unwrap().permissions();
        p.set_readonly(true);
        fs::set_permissions(path, p).unwrap();
    }
}

fn hidden(path: &Path) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        fs::metadata(path).unwrap().file_attributes() & 0x2 != 0
    }
    #[cfg(not(windows))]
    fs::metadata(path).unwrap().permissions().readonly()
}

// Spec (R5-05: "Promotion still destroys attributes that Git cannot restore"; decision 5: promotion refuses a
// non-default attribute): a private skill with a hidden file is not promoted, and stays.
#[test]
fn r5_05_promotion_refuses_a_hidden_file() {
    let f = Fixture::new();
    skill(&f, "priv", "demo");
    f.git_init("priv");
    f.git_init("pub");
    let file = f.path("priv/global/skills/demo/SKILL.md");
    hide(&file);
    let result = lords_core::ops::promote(
        &f.stacks(),
        "skill.demo",
        lords_core::ops::Promote::Clean,
        &lords_core::privacy::Rules::public(),
        false,
    )
    .map(|p| p.to);
    assert!(result.is_err(), "{result:?}");
    assert!(file.is_file() && hidden(&file));
}

// Spec (decision 5, retire, import and edit: "either attributes are kept or a non-default attribute refuses"): a
// retired entry's file keeps its attributes.
#[test]
fn r5_05_retire_keeps_a_files_attributes() {
    let f = Fixture::new();
    let file = f.write("pub/global/rules/a.md", &entry("rule.a", "global", "public", "", "x\n"));
    hide(&file);
    f.git_init("pub");
    let r = lords_core::ops::Retire {
        id: "rule.a",
        reason: "r",
        superseded_by: None,
        date: "2026-10-25".into(),
        commit: true,
    };
    let result = lords_core::ops::retire(&f.stacks(), &r).map(|r| r.file);
    assert!(result.is_ok(), "{result:?}");
    assert!(hidden(&file), "the attributes were kept");
    assert!(fs::read_to_string(&file).unwrap().contains("status: retired"));
}

// Spec (decision 5, restore): a file apply created and the owner has since marked is kept, not removed.
#[test]
fn r5_05_restore_keeps_a_created_file_the_owner_marked() {
    let f = Fixture::new();
    f.write("pub/global/rules/r.md", &entry("rule.r", "global", "public", "", "Rule text.\n"));
    let file = f.path("out/new.md");
    declare(&f, &format!("[[targets]]\nname = \"t\"\nadapter = \"codex\"\nkind = \"instructions\"\npath = '{}'\n", file.display()));
    let backups = sync::apply(&sync::plan(&f.stacks(), &f.opts()).unwrap())
        .unwrap()
        .backups
        .expect("a backup");
    hide(&file);
    let steps = sync::restore(&sync::plan(&f.stacks(), &f.opts()).unwrap(), &backups);
    assert!(file.is_file(), "{steps:?}");
    #[cfg(not(windows))]
    {
        let mut p = fs::metadata(&file).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        p.set_readonly(false);
        fs::set_permissions(&file, p).unwrap();
    }
}

// ---------------------------------------------------------------- guarantee 7: the effective hooks

fn public_repo(f: &Fixture) {
    f.git_init("pub");
}

fn hook_findings(f: &Fixture) -> Vec<String> {
    lords_core::check::check_at(&f.stacks(), &lords_core::privacy::Rules::public(), &f.home())
        .into_iter()
        .filter(|x| x.code == "privacy-hooks")
        .map(|x| format!("{:?} {}", x.severity, x.message))
        .collect()
}

fn install(f: &Fixture) {
    let exe = std::path::PathBuf::from("C:/lords/lords.exe");
    lords_core::hooks::install(&f.pub_root(), &exe, false, false, &f.home()).unwrap();
}

// Spec (decision 8: "`lords check` verifies per public repo that the effective dispatch actually reaches both
// privacy hooks … Otherwise it reports 'not verified'"): a public repo without them is not verified; with both
// installed in `.git/hooks` it is.
#[test]
fn check_verifies_that_commits_reach_both_privacy_hooks() {
    let f = Fixture::new();
    public_repo(&f);
    let before = hook_findings(&f);
    assert!(
        before.iter().any(|m| m.starts_with("Warning") && m.contains("not verified")),
        "{before:?}"
    );
    install(&f);
    assert_eq!(hook_findings(&f), Vec::<String>::new());
}

// Spec (decision 8): a hooks path set later that bypasses them is not verified.
#[test]
fn check_reports_a_hooks_path_that_bypasses_them() {
    let f = Fixture::new();
    public_repo(&f);
    install(&f);
    fs::create_dir_all(f.path("elsewhere")).unwrap();
    git(&f.pub_root(), &["config", "core.hooksPath", f.path("elsewhere").to_str().unwrap()]);
    let found = hook_findings(&f);
    assert!(
        found.iter().any(|m| m.contains("not verified") && m.contains("pre-commit")),
        "{found:?}"
    );
}

/// A dispatcher like the owner's attribution guard: `<dir>/hooks/<name>` hands over to `<dir>/dispatch.sh`.
fn guard(f: &Fixture) -> std::path::PathBuf {
    let dir = f.path("guard");
    // It delegates as the owner's does (Sol review r6, R6-07: a double that runs nothing can't establish
    // reachability): the repo's own hook, unless `attributionGuard.skipPreviousHook` names it.
    f.write(
        "guard/dispatch.sh",
        "#!/bin/sh\n# attribution-guard dispatcher (test double)\nhook=$1\nshift\n\
         if git config --get-all attributionGuard.skipPreviousHook | grep -qx -- \"$hook\"; then exit 0; fi\n\
         common=$(git rev-parse --path-format=absolute --git-common-dir) || exit 1\n\
         if [ -f \"$common/hooks/$hook\" ]; then exec sh \"$common/hooks/$hook\" \"$@\"; fi\nexit 0\n",
    );
    // Every hook the owner's guard wraps that a commit runs, pre-merge-commit too (as its `hooks/` dir has).
    for h in lords_core::hooks::PRIVACY {
        f.write(
            &format!("guard/hooks/{h}"),
            &format!("#!/bin/sh\nexec \"$(dirname -- \"$0\")/../dispatch.sh\" {h} \"$@\"\n"),
        );
    }
    dir.join("hooks")
}

// Spec (decision 8: "or the global attribution guard dispatcher with delegation, and not `skipPreviousHook` for
// pre-commit/commit-msg"): through the dispatcher the hooks are verified; skipped by it, not.
#[test]
fn check_follows_the_attribution_guard_dispatcher() {
    let f = Fixture::new();
    public_repo(&f);
    install(&f);
    let hooks = guard(&f);
    git(&f.pub_root(), &["config", "core.hooksPath", hooks.to_str().unwrap()]);
    assert_eq!(hook_findings(&f), Vec::<String>::new());
    git(&f.pub_root(), &["config", "--add", "attributionGuard.skipPreviousHook", "commit-msg"]);
    let found = hook_findings(&f);
    assert!(
        found.iter().any(|m| m.contains("not verified") && m.contains("commit-msg")),
        "{found:?}"
    );
    assert!(lords_core::check::check_at(
        &f.stacks(),
        &lords_core::privacy::Rules::public(),
        &f.home()
    )
    .iter()
    .all(|x| x.code != "privacy-hooks" || x.severity != Severity::Error));
}
