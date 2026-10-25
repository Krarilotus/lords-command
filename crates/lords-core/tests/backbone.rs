//! A consuming project's requirements on the backbone (BB-1..9), as the orchestrator accepted them.

mod common;
use common::*;
use lords_core::model::Origin;
use lords_core::sync::{apply, drift_findings, plan, Action, Options};
use std::fs;
use std::path::Path;

fn manifest(f: &Fixture, extra: &str) {
    f.write(
        "priv/stack.toml",
        &format!("name = \"priv\"\nvisibility = \"private\"\nroles = [\"orchestrator\", \"coder\", \"product\"]\n[projects.beta]\n{extra}"),
    );
}

fn repo_with(f: &Fixture, checkout: &Path, extra: &str) {
    manifest(
        f,
        &format!(
            "[repos.beta-core]\nproject = \"beta\"\ncheckouts = ['{}']\nworktrees = false\n{extra}",
            checkout.display()
        ),
    );
}

fn proposed(p: &lords_core::sync::Plan, path: &Path) -> String {
    p.items.iter().find(|i| i.path == path).and_then(|i| i.proposed.clone()).unwrap_or_default()
}

// Spec (BB-5: "User-level files may hold only what suits any agent run"; response: "no development-process rules,
// no memory index and no private notes"): a global rule for every role stays at the user level; a global rule for
// development roles and a memory note leave it and go to the repo's files instead.
#[test]
fn user_level_text_suits_any_run() {
    let f = Fixture::new();
    f.write("pub/global/rules/any.md", &entry("rule.any", "global", "public", "", "Be kind.\n"));
    f.write(
        "priv/global/rules/dev.md",
        &entry(
            "rule.dev",
            "global",
            "private",
            "roles: [orchestrator, coder]\n",
            "Gate before merge.\n",
        ),
    );
    f.write("priv/global/memory/n.md", &entry("memory.n", "global", "private", "", "A note.\n"));
    let checkout = f.path("checkout");
    f.write(
        "checkout/README.md",
        "a repo
",
    );
    f.git_init("checkout");
    let user = f.path("home/AGENTS.md");
    repo_with(&f, &checkout, &format!("[[targets]]\nname = \"codex\"\nadapter = \"codex\"\nkind = \"instructions\"\npath = '{}'\n", user.display()));
    let p = plan(&f.stacks(), &Options::default()).unwrap();
    let at_user = proposed(&p, &user);
    assert!(
        at_user.contains("Be kind.")
            && !at_user.contains("Gate before merge")
            && !at_user.contains("# Memory"),
        "{at_user}"
    );
    let in_repo = proposed(&p, &checkout.join("AGENTS.override.md"));
    assert!(
        in_repo.contains("Gate before merge.")
            && in_repo.contains("# Memory")
            && !in_repo.contains("Be kind."),
        "{in_repo}"
    );
    // Place files never reach product runs, so a rule for every development role carries no role label there.
    assert!(!in_repo.contains("only)\nGate before merge."), "{in_repo}");
}

// Spec (response: "Harness isn't a git repo, so it needs a workspace layer (a directory with no repo)"): a
// workspace gets the same files as a checkout, takes its own layer, and needs no git exclude.
#[test]
fn a_workspace_gets_its_layer_without_git() {
    let f = Fixture::new();
    f.write(
        "priv/workspace/harness/rules/w.md",
        &entry("rule.w", "workspace:harness", "private", "", "Notes go under notes/.\n"),
    );
    f.write(
        "priv/repo/beta-core/rules/r.md",
        &entry("rule.r", "repo:beta-core", "private", "", "Repo only.\n"),
    );
    let dir = f.path("harness");
    fs::create_dir_all(&dir).unwrap();
    manifest(
        &f,
        &format!(
            "[repos.beta-core]\nproject = \"beta\"\n[workspaces.harness]\npath = '{}'\n",
            dir.display()
        ),
    );
    assert_eq!(errors(&f.check()), Vec::<&str>::new(), "{:#?}", f.check());
    let p = plan(&f.stacks(), &Options { repo: Some("harness".into()), user_only: false }).unwrap();
    let claude = proposed(&p, &dir.join("CLAUDE.local.md"));
    assert!(
        claude.contains("Notes go under notes/.") && !claude.contains("Repo only."),
        "{claude}"
    );
    assert!(p.items.iter().all(|i| !i.target.contains("exclude")), "no git, no exclude");
    apply(&p, &f.path("backups")).unwrap();
    assert!(fs::read_to_string(dir.join("AGENTS.override.md"))
        .unwrap()
        .contains("Notes go under notes/."));
}

// Spec (BB-2: "A CLAUDE.local.md that hides the tracked file must not drop any line of it"; response: Codex's
// override "carries the tracked text verbatim plus the private layers. check verifies that every tracked line is
// present, in order"): sync writes it so; a file that drops or reorders a line, or a CLAUDE.local.md without the
// include, is an error.
#[test]
fn files_that_hide_agents_md_keep_every_line_of_it() {
    let f = Fixture::new();
    f.write(
        "priv/repo/beta-core/rules/r.md",
        &entry("rule.r", "repo:beta-core", "private", "", "Private repo rule.\n"),
    );
    let checkout = f.path("checkout");
    f.write("checkout/AGENTS.md", "# Tracked\n\n- first\n- second\n");
    f.git_init("checkout");
    repo_with(&f, &checkout, "");
    apply(&plan(&f.stacks(), &Options::default()).unwrap(), &f.path("backups")).unwrap();
    let verbatim = |s: &lords_core::Stacks| {
        drift_findings(s).into_iter().filter(|x| x.code == "verbatim").count()
    };
    assert_eq!(verbatim(&f.stacks()), 0, "sync's own files keep every line");
    let over = fs::read_to_string(checkout.join("AGENTS.override.md")).unwrap();
    assert!(
        over.contains("# Tracked\n\n- first\n- second\n") && over.contains("Private repo rule."),
        "{over}"
    );

    fs::write(
        checkout.join("AGENTS.override.md"),
        over.replace("- first\n- second\n", "- second\n- first\n"),
    )
    .unwrap();
    assert_eq!(verbatim(&f.stacks()), 1, "reordered");
    fs::write(checkout.join("AGENTS.override.md"), over.replace("- second\n", "")).unwrap();
    assert_eq!(verbatim(&f.stacks()), 1, "dropped");
    fs::write(checkout.join("AGENTS.override.md"), &over).unwrap();
    let local = fs::read_to_string(checkout.join("CLAUDE.local.md")).unwrap();
    fs::write(checkout.join("CLAUDE.local.md"), local.replace("@AGENTS.md\n", "")).unwrap();
    assert_eq!(verbatim(&f.stacks()), 1, "the include removed");
}

// Spec (BB-3: "one composed output per repo and role that a brief can reference"; BB-1: several agents in one
// checkout, no worktree per agent; response: "a stable untracked path (e.g. .lords/role-<role>.md, excluded)"
// and "Add the role product"): one checkout gets a file per declared role, each with what that role follows
// (global, project, repo, notes), excluded from git; the product role gets no development rule.
#[test]
fn one_checkout_gets_a_file_per_role() {
    let f = Fixture::new();
    f.write("pub/global/rules/any.md", &entry("rule.any", "global", "public", "", "Be kind.\n"));
    f.write(
        "priv/global/rules/dev.md",
        &entry(
            "rule.dev",
            "global",
            "private",
            "roles: [orchestrator, coder]\n",
            "Gate before merge.\n",
        ),
    );
    f.write(
        "priv/repo/beta-core/rules/o.md",
        &entry(
            "rule.o",
            "repo:beta-core",
            "private",
            "roles: [orchestrator]\n",
            "Merge lanes one at a time.\n",
        ),
    );
    let checkout = f.path("checkout");
    f.write(
        "checkout/README.md",
        "a repo
",
    );
    f.git_init("checkout");
    repo_with(&f, &checkout, "");
    let p = plan(&f.stacks(), &Options::default()).unwrap();
    let role = |r: &str| proposed(&p, &checkout.join(".lords").join(format!("role-{r}.md")));
    let (orch, coder, product) = (role("orchestrator"), role("coder"), role("product"));
    assert!(
        orch.contains("Be kind.")
            && orch.contains("Gate before merge.")
            && orch.contains("Merge lanes one at a time."),
        "{orch}"
    );
    assert!(coder.contains("Gate before merge.") && !coder.contains("Merge lanes"), "{coder}");
    assert!(
        product.contains("Be kind.")
            && !product.contains("Gate before merge")
            && !product.contains("Merge lanes"),
        "{product}"
    );
    apply(&p, &f.path("backups")).unwrap();
    assert!(fs::read_to_string(checkout.join(".git/info/exclude"))
        .unwrap()
        .lines()
        .any(|l| l == ".lords/"));
    assert_eq!(
        git(&checkout, &["status", "--porcelain"]),
        "",
        "role files are untracked and ignored"
    );
}

// Spec (BB-7: "a field for incidents and evidence links, separate from basis"; response: "a typed evidence list
// (summary, date, links)"): evidence is metadata beside the rule, never in the composed text; its id and path
// links are checked like `links`.
#[test]
fn evidence_stays_beside_the_rule() {
    let f = Fixture::new();
    let ev = "evidence:\n- summary: Two lanes committed the same file\n  date: 2026-10-21\n  links: [rule.none]\n";
    f.write(
        "pub/global/rules/a.md",
        &entry("rule.a", "global", "public", ev, "One writer per file.\n"),
    );
    let found = f.check();
    assert!(has(&found, "dangling", "rule.a"), "{found:#?}");
    f.write(
        "pub/global/rules/a.md",
        &entry(
            "rule.a",
            "global",
            "public",
            &ev.replace("rule.none", "https://x.org/incident"),
            "One writer per file.\n",
        ),
    );
    assert_eq!(errors(&f.check()), Vec::<&str>::new());
    let s = f.stacks();
    assert_eq!(
        s.find("rule.a").unwrap().meta.evidence[0].summary,
        "Two lanes committed the same file"
    );
    let ctx = lords_core::compose::Context {
        audience: vec![lords_core::model::Audience::Opus],
        layers: lords_core::compose::Layers::Global,
        public_only: false,
        skip_stores: vec![],
        roles: None,
        by_reference: false,
    };
    let text = lords_core::compose::compose(&s, &ctx).text;
    assert!(text.contains("One writer per file.") && !text.contains("Two lanes"), "{text}");
}

// Spec (BB-8: "Lords Command indexes those skills in place (origin: this repo, this path) and doesn't move or copy
// them"; response: "origin: repo, path, hash"): a skill tracked in a repo is an entry with that origin; the
// repo's files list it; a skills target never links it.
#[test]
fn repo_skills_are_indexed_in_place() {
    let f = Fixture::new();
    let checkout = f.path("checkout");
    f.write(
        "checkout/v2/.claude/skills/review/SKILL.md",
        "---\nname: review\ndescription: review a lane's diff\n---\nSteps.\n",
    );
    f.git_init("checkout");
    let skills_dir = f.path("home/skills");
    repo_with(
        &f,
        &checkout,
        &format!("skills = ['v2/.claude/skills']\n[[targets]]\nname = \"skills\"\nadapter = \"codex\"\nkind = \"skills\"\npath = '{}'\n", skills_dir.display()),
    );
    let s = f.stacks();
    let e = s.find("skill.beta-core.review").expect("indexed");
    match &e.meta.origin {
        Origin::InRepo { repo, path, sha256 } => {
            assert_eq!((repo.as_str(), path.as_str()), ("beta-core", "v2/.claude/skills/review"));
            assert_eq!(
                *sha256,
                lords_core::check::dir_hash(&checkout.join("v2/.claude/skills/review"))
            );
        }
        o => panic!("origin {o:?}"),
    }
    let p = plan(&s, &Options::default()).unwrap();
    assert!(proposed(&p, &checkout.join("AGENTS.override.md"))
        .contains("# Skills here\n\n- review: review a lane's diff (`"));
    assert!(
        p.items.iter().all(|i| !i.path.starts_with(&skills_dir) || i.action != Action::Link),
        "never linked"
    );
    assert!(!f.path("priv/repo/beta-core/skills").exists(), "nothing copied into the stack");
}
