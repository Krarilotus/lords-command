//! A place's default role per adapter (orchestrator, 25 Oct, option 1: "in the adapter: Claude inline, Codex by
//! reference, with the role-files line naming the default role's file first"), and worktrees outside the
//! repo folder (orchestrator, 25 Oct: a project keeps them in `<project>/.local/wt/<package>`, owned per `.owner.toml`).

mod common;
use common::*;
use lords_core::ledger::Ledger;
use lords_core::sync::{self, plan, Options};
use std::path::Path;

fn proposed(p: &sync::Plan, path: &Path) -> String {
    p.items.iter().find(|i| i.path == path).and_then(|i| i.proposed.clone()).unwrap_or_default()
}

/// Rules and notes for coders and for orchestrators, in project `beta`.
fn role_entries(f: &Fixture) {
    let rule = |id: &str, roles: &str, text: &str| {
        entry(id, "project:beta", "private", &format!("roles: [{roles}]\n"), text)
    };
    f.write("priv/project/beta/rules/c.md", &rule("rule.c", "coder", "Coders test first.\n"));
    f.write("priv/project/beta/rules/o.md", &rule("rule.o", "orchestrator", "Merge in order.\n"));
    f.write(
        "priv/project/beta/memory/on.md",
        &entry("memory.on", "project:beta", "private", "roles: [orchestrator]\n", "Note.\n"),
    );
    f.write(
        "priv/project/beta/memory/cn.md",
        &entry("memory.cn", "project:beta", "private", "roles: [coder]\n", "Note.\n"),
    );
}

fn repo(f: &Fixture, default_role: &str) -> std::path::PathBuf {
    let checkout = f.path("checkout");
    f.write("checkout/README.md", "a repo\n");
    f.git_init("checkout");
    f.write(
        "priv/stack.toml",
        &format!(
            "name = \"priv\"\nvisibility = \"private\"\nroles = [\"orchestrator\", \"coder\", \"product\"]\n[projects.beta]\n[repos.beta-core]\nproject = \"beta\"\ncheckouts = ['{}']\nworktrees = false\ndefault_role = \"{default_role}\"\n",
            checkout.display()
        ),
    );
    checkout
}

// Spec (option 1, Codex): Codex's file, which stops at its byte limit, carries the default role's entries by
// reference like every other role's; its role-files line and its notes line name the default role first.
#[test]
fn codex_reads_the_default_role_by_reference_and_first() {
    let f = Fixture::new();
    role_entries(&f);
    let checkout = repo(&f, "orchestrator");
    let p = plan(&f.stacks(), &f.opts()).unwrap();
    let codex = proposed(&p, &checkout.join("AGENTS.override.md"));
    assert!(!codex.contains("Merge in order."), "not inline for Codex: {codex}");
    let line = codex.lines().find(|l| l.contains("has them in full:")).unwrap_or_default();
    let (o, c) = (line.find("role-orchestrator.md"), line.find("role-coder.md"));
    assert!(o.is_some() && c.is_some() && o < c, "the default role's file first: {line}");
    let notes =
        codex.lines().find(|l| l.contains("More notes are for one role only")).unwrap_or_default();
    let (o, c) = (notes.find("orchestrator"), notes.find("coder"));
    assert!(o.is_some() && c.is_some() && o < c, "the default role's notes first: {notes}");
}

// Spec (option 1, Claude): Claude's file keeps the default role's entries inline, labelled, and its notes indexed.
#[test]
fn claude_reads_the_default_role_inline() {
    let f = Fixture::new();
    role_entries(&f);
    let checkout = repo(&f, "orchestrator");
    let p = plan(&f.stacks(), &f.opts()).unwrap();
    let claude = proposed(&p, &checkout.join("CLAUDE.local.md"));
    assert!(claude.contains("(orchestrator only)\nMerge in order."), "{claude}");
    assert!(claude.contains("- on: memory.on"), "{claude}");
    assert!(!claude.contains("Coders test first."), "{claude}");
}

// Spec (orchestrator, 25 Oct: "a worktree outside the repo folder gets its place files with git status
// clean, and `.owner.toml` worktrees are never counted or removed as ours"): a worktree in `.local/wt/<package>`
// beside the repo gets its files and stays clean, as does the repo; the owner records there are never planned,
// recorded or removed, and uninstalling removes only sync's own files.
#[test]
fn a_worktree_outside_the_repo_folder_gets_its_files_and_owner_records_stay() {
    let f = Fixture::new();
    f.write("pub/global/rules/r.md", &entry("rule.r", "global", "public", "", "Rule text.\n"));
    f.write("proj/v2/README.md", "a repo\n");
    f.git_init("proj/v2");
    let (repo, wt) = (f.path("proj/v2"), f.path("proj/.local/wt/pkg"));
    git(&repo, &["worktree", "add", "-q", "-b", "pkg", wt.to_str().unwrap()]);
    let owner_records = [
        f.write("proj/.local/wt/.owner.toml", "owner = \"pkg-lead\"\nkeep = false\n"),
        f.write("proj/.local/wt/pkg.owner.toml", "owner = \"pkg-lead\"\nkeep = true\n"),
    ];
    f.write(
        "priv/stack.toml",
        &format!(
            "name = \"priv\"\nvisibility = \"private\"\nroles = [\"coder\"]\n[repos.v2]\ncheckouts = ['{}']\nworktree_roots = ['{}']\n",
            repo.display(),
            f.path("proj/.local/wt").display()
        ),
    );
    let p = plan(&f.stacks(), &f.opts()).unwrap();
    let owner_named = |path: &Path| path.to_string_lossy().contains(".owner.toml");
    assert!(!p.items.iter().any(|i| owner_named(&i.path)), "never planned");
    sync::apply(&p).unwrap();
    for file in ["CLAUDE.local.md", "AGENTS.override.md", ".lords"] {
        assert!(wt.join(file).exists(), "{file} in the worktree");
    }
    assert_eq!(git(&wt, &["status", "--porcelain"]), "", "the worktree stays clean");
    assert_eq!(git(&repo, &["status", "--porcelain"]), "", "the repo stays clean");
    let ledger = Ledger::load(&f.home()).unwrap();
    let wt_key = lords_core::ledger::key(&wt);
    assert!(
        !ledger.owned.values().any(|o| owner_named(Path::new(&o.path))
            || lords_core::ledger::key(Path::new(&o.path)) == wt_key),
        "never recorded: {:?}",
        ledger.owned.keys().collect::<Vec<_>>()
    );
    let opts = Options { uninstall: true, ..f.opts() };
    sync::apply(&plan(&f.stacks(), &opts).unwrap()).unwrap();
    for r in &owner_records {
        assert!(r.is_file(), "{} stays", r.display());
    }
    assert!(wt.join("README.md").is_file() && !wt.join("CLAUDE.local.md").exists());
}
