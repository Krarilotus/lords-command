//! The design review's three points (orchestrator, 25 Oct) and the staged first use, as spec tests of the new API
//! (docs/design-simplification-r6.md): set-aside and restore take the plan, whose roots are verified, and the plan
//! takes a stage. This file doesn't build on afbe95b (its red run); round6.rs has the rest.

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;
use common::*;
use lords_core::sync::{self, Action, Stage};

fn declare(f: &Fixture, body: &str) {
    f.write(
        "priv/stack.toml",
        &format!("name = \"priv\"\nvisibility = \"private\"\nroles = [\"coder\"]\n{body}"),
    );
}

fn skills_target(f: &Fixture) -> String {
    format!(
        "[[targets]]\nname = \"skills\"\nadapter = \"claude\"\nkind = \"skills\"\npath = '{}'\n",
        f.path("skills").display()
    )
}

// Spec (design review, point 2: "`Root::containing(path)` may only return one of the plan's already-verified roots
// … It never builds a new root from the path itself"): set-aside refuses a path in no verified root and moves it
// nowhere; a path in one (a skills target) goes.
#[test]
fn set_aside_takes_only_a_path_in_a_verified_root() {
    let f = Fixture::new();
    declare(&f, &skills_target(&f));
    f.write("elsewhere/thing/file.md", "OWNER");
    f.write("skills/foreign/SKILL.md", "an owner's skill\n");
    let plan = sync::plan(&f.stacks(), &f.opts()).unwrap();
    let outside = sync::set_aside(&plan, &[f.path("elsewhere/thing")]);
    assert!(outside.is_err(), "{:?}", outside.map(|a| a.done));
    assert!(f.path("elsewhere/thing/file.md").is_file());
    let inside = sync::set_aside(&plan, &[f.path("skills/foreign")]).map(|a| a.done);
    assert!(inside.is_ok() && !f.path("skills/foreign").exists(), "{inside:?}");
}

// Spec (design review, point 3: "a manifest line that doesn't re-derive under the current config refuses. It's
// listed with its path and the backup's location for manual recovery, and the tool never guesses"): after the
// target moved, restore changes nothing and names the old path and its backup.
#[test]
fn restore_refuses_a_line_that_does_not_rederive() {
    let f = Fixture::new();
    f.write("pub/global/rules/r.md", &entry("rule.r", "global", "public", "", "Rule text.\n"));
    let instructions = |file: &str| {
        format!("[[targets]]\nname = \"t\"\nadapter = \"codex\"\nkind = \"instructions\"\npath = '{}'\n", f.path(file).display())
    };
    f.write("out/file.md", "OLD\n");
    declare(&f, &instructions("out/file.md"));
    let backups = sync::apply(&sync::plan(&f.stacks(), &f.opts()).unwrap())
        .unwrap()
        .backups
        .expect("a backup");
    let applied = f.read("out/file.md");
    declare(&f, &instructions("out/other.md"));
    let plan = sync::plan(&f.stacks(), &f.opts()).unwrap();
    let err = sync::restore(&plan, &backups).unwrap_err();
    assert!(err.contains("file.md") && err.contains(&backups.display().to_string()), "{err}");
    assert_eq!(f.read("out/file.md"), applied, "nothing changed");
}

// Spec (design, staged first use: "only tool-owned paths (Create, Install, Update, Refresh) plus each repo's
// git-exclude lines … no Remove or Forget, no user-level owner edit … all-or-nothing per place"): a staged plan
// leaves the user-level file alone, writes a clean place whole with its exclude lines, and skips a place with a
// conflict, saying so.
#[test]
fn the_install_stage_writes_tool_paths_and_excludes_only() {
    let f = Fixture::new();
    f.write("pub/global/rules/r.md", &entry("rule.r", "global", "public", "", "Rule text.\n"));
    f.write("home/AGENTS.md", "OWNER\n");
    for r in ["clean", "taken"] {
        f.write(&format!("{r}/README.md"), "a repo\n");
        f.git_init(r);
    }
    f.write("taken/CLAUDE.local.md", "an owner's file\n");
    declare(
        &f,
        &format!(
            "[[targets]]\nname = \"user\"\nadapter = \"codex\"\nkind = \"instructions\"\npath = '{}'\n[repos.clean]\ncheckouts = ['{}']\nworktrees = false\nadapters = [\"claude\"]\n[repos.taken]\ncheckouts = ['{}']\nworktrees = false\nadapters = [\"claude\"]\n",
            f.path("home/AGENTS.md").display(),
            f.path("clean").display(),
            f.path("taken").display()
        ),
    );
    let opts = sync::Options { stage: Stage::Install, ..f.opts() };
    let p = sync::plan(&f.stacks(), &opts).unwrap();
    assert!(!p.items.iter().any(|i| matches!(i.action, Action::Remove | Action::Forget)));
    assert!(!p.items.iter().any(|i| i.path == f.path("home/AGENTS.md")), "no user-level edit");
    assert!(
        p.notes.iter().any(|n| n.contains("taken")),
        "the skipped place is named: {:?}",
        p.notes
    );
    sync::apply(&p).unwrap();
    assert_eq!(f.read("home/AGENTS.md"), "OWNER\n");
    assert!(f.path("clean/CLAUDE.local.md").is_file() && f.path("clean/.lords").is_dir());
    assert_eq!(
        git(&f.path("clean"), &["status", "--porcelain"]),
        "",
        "its exclude lines keep it clean"
    );
    assert_eq!(f.read("taken/CLAUDE.local.md"), "an owner's file\n");
    assert!(!f.path("taken/.lords").exists(), "the place with a conflict is skipped whole");
}
