//! What goes by reference in a place's agent files (orchestrator T1, 25 Oct: "In repo files, intent goes by
//! reference (an index line plus the file, read on demand, like memory), with the full text only in the role
//! files … Don't raise Codex's byte limit: silent truncation is the failure").

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;
use common::*;
use lords_core::sync::{plan, Options};
use std::path::Path;

fn manifest(f: &Fixture, checkout: &Path) {
    f.write(
        "priv/stack.toml",
        &format!(
            "name = \"priv\"\nvisibility = \"private\"\nroles = [\"orchestrator\", \"coder\", \"product\"]\n[projects.beta]\n[repos.beta-core]\nproject = \"beta\"\ncheckouts = ['{}']\nworktrees = false\n",
            checkout.display()
        ),
    );
}

fn proposed(p: &lords_core::sync::Plan, path: &Path) -> String {
    p.items.iter().find(|i| i.path == path).and_then(|i| i.proposed.clone()).unwrap_or_default()
}

// Spec (orchestrator, 25 Oct: "`default_role` per place … The default role's entries go inline in the place
// file, and the role-files line stays for briefed agents"): with `default_role = "coder"`, a rule for coders only
// is inline (labelled), a rule for orchestrators only stays behind the role-files line, and so do their notes.
#[test]
fn a_places_default_role_reads_its_entries_inline() {
    let f = Fixture::new();
    let rule = |id: &str, roles: &str, text: &str| {
        entry(id, "project:beta", "private", &format!("roles: [{roles}]\n"), text)
    };
    f.write("priv/project/beta/rules/c.md", &rule("rule.c", "coder", "Coders test first.\n"));
    f.write("priv/project/beta/rules/o.md", &rule("rule.o", "orchestrator", "Merge in order.\n"));
    f.write(
        "priv/project/beta/memory/cn.md",
        &entry("memory.cn", "project:beta", "private", "roles: [coder]\n", "Note.\n"),
    );
    let checkout = f.path("checkout");
    f.write("checkout/README.md", "a repo\n");
    f.git_init("checkout");
    manifest(&f, &checkout);
    let toml = std::fs::read_to_string(f.path("priv/stack.toml")).unwrap();
    f.write("priv/stack.toml", &format!("{toml}default_role = \"coder\"\n"));
    let p = plan(&f.stacks(), &f.opts()).unwrap();
    let place = proposed(&p, &checkout.join("CLAUDE.local.md"));
    assert!(place.contains("(coder only)\nCoders test first."), "{place}");
    assert!(place.contains("- cn: memory.cn"), "the default role's notes are indexed: {place}");
    assert!(
        !place.contains("Merge in order.")
            && place.contains("has them in full: `role-orchestrator.md` (1)."),
        "{place}"
    );
}

// Spec (orchestrator, 25 Oct: "List them in check's info as 'AGENTS.md shadowed by an ancestor place file'"):
// a worktree outside `worktree_roots` that has an AGENTS.md and lies inside a workspace place is listed; a
// worktree that gets its own files is not.
#[test]
fn check_lists_worktrees_whose_agents_md_an_ancestor_place_hides() {
    let f = Fixture::new();
    f.write("repo/AGENTS.md", "Repo rules.\n");
    f.git_init("repo");
    let (repo, ws) = (f.path("repo"), f.path("ws"));
    std::fs::create_dir_all(&ws).unwrap();
    for (dir, branch) in [("ws/inside", "a"), ("wts/own", "b")] {
        git(&repo, &["worktree", "add", "-q", "-b", branch, f.path(dir).to_str().unwrap()]);
    }
    f.write(
        "priv/stack.toml",
        &format!(
            "name = \"priv\"\nvisibility = \"private\"\nroles = [\"coder\"]\n[repos.r]\ncheckouts = ['{}']\nworktree_roots = ['{}']\n[workspaces.w]\npath = '{}'\n",
            repo.display(),
            f.path("wts").display(),
            ws.display()
        ),
    );
    let found = lords_core::sync::shadow_findings(&f.stacks());
    let messages: Vec<&str> = found.iter().map(|x| x.message.as_str()).collect();
    assert!(
        messages.len() == 1
            && messages[0]
                .contains("inside: AGENTS.md shadowed by an ancestor place file (workspace w"),
        "{messages:?}"
    );
}

// Spec (orchestrator, 25 Oct: Claude's files now, Codex's "only after the grouped-form marker run passes"): a plan
// for one adapter holds that adapter's place file and none of the other's; the role files, which agents of every
// adapter read, are planned either way.
#[test]
fn a_plan_for_one_adapter_leaves_the_others_files_alone() {
    let f = Fixture::new();
    f.write(
        "priv/project/beta/rules/all.md",
        &entry("rule.all", "project:beta", "private", "", "Gate before merge.\n"),
    );
    let checkout = f.path("checkout");
    f.write("checkout/README.md", "a repo\n");
    f.git_init("checkout");
    manifest(&f, &checkout);
    let opts = Options { adapter: Some("claude".into()), ..f.opts() };
    let p = plan(&f.stacks(), &opts).unwrap();
    let has = |path: &Path| p.items.iter().any(|i| i.path == path);
    assert!(has(&checkout.join("CLAUDE.local.md")) && !has(&checkout.join("AGENTS.override.md")));
    assert!(has(&checkout.join(".lords")), "role files are planned");
}

// Spec (above; orchestrator, 25 Oct, the efficiency pass: cut "index lines that the role files already carry in
// full" and "long paths where a stable id plus one lookup line would do"): in the files every agent in a checkout
// reads, owner intent is an index line (its title and file) under a heading that says it binds, its dir named
// once; the role files carry its text; rules for every development role stay inline, while rules for only some
// roles are one line naming the checkout's role files and how many each carries.
#[test]
fn intent_and_partial_role_rules_go_by_reference_in_place_files() {
    let f = Fixture::new();
    let dev = "roles: [orchestrator, coder]\n";
    f.write(
        "priv/project/beta/intent/goals.md",
        &entry("intent.goals", "project:beta", "private", dev, "Exceed, not parity.\n")
            .replace("title: intent.goals", "title: Release goals"),
    );
    f.write(
        "priv/project/beta/rules/all.md",
        &entry("rule.all", "project:beta", "private", dev, "Gate before merge.\n"),
    );
    f.write(
        "priv/project/beta/rules/orch.md",
        &entry(
            "rule.orch",
            "project:beta",
            "private",
            "roles: [orchestrator]\n",
            "Merge lanes one at a time.\n",
        )
        .replace("title: rule.orch", "title: Merge order"),
    );
    let checkout = f.path("checkout");
    f.write("checkout/README.md", "a repo\n");
    f.git_init("checkout");
    manifest(&f, &checkout);
    let p = plan(&f.stacks(), &f.opts()).unwrap();

    let place = proposed(&p, &checkout.join("AGENTS.override.md"));
    let native =
        |p: std::path::PathBuf| p.display().to_string().replace('/', std::path::MAIN_SEPARATOR_STR);
    let intent_dir = native(f.path("priv/project/beta/intent"));
    assert!(place.contains("# Owner intent\n\nThe owner's intent, binding."), "{place}");
    assert!(
        place.contains(&format!("In `{intent_dir}`:\n- Release goals (`goals.md`)"))
            && !place.contains("Exceed, not parity."),
        "{place}"
    );
    assert!(place.contains("Gate before merge."), "a rule for every development role stays inline");
    let role_dir = native(checkout.join(".lords"));
    assert!(
        place.contains(&format!(
            "each role's file in `{role_dir}` has them in full: `role-orchestrator.md` (1)."
        )) && !place.contains("Merge order")
            && !place.contains("Merge lanes one at a time."),
        "{place}"
    );

    let orch = planned_in(&p, &checkout.join(".lords"), "role-orchestrator.md");
    assert!(
        orch.contains("Exceed, not parity.") && orch.contains("Merge lanes one at a time."),
        "{orch}"
    );
    let coder = planned_in(&p, &checkout.join(".lords"), "role-coder.md");
    assert!(coder.contains("Exceed, not parity.") && !coder.contains("Merge lanes"), "{coder}");
}

// Spec (orchestrator, 25 Oct: "Any form Codex must resolve (relative paths) waits for the 20:00 marker test, but
// structure the output now so that switch is one flag"): with `relative_to` set, every dir an index names is
// relative to it (`..` where it lies outside); without it, absolute.
#[test]
fn relative_paths_are_one_switch() {
    let f = Fixture::new();
    f.write(
        "priv/project/beta/intent/goals.md",
        &entry("intent.goals", "project:beta", "private", "", "Exceed.\n")
            .replace("title: intent.goals", "title: Release goals"),
    );
    f.write(
        "priv/project/beta/rules/orch.md",
        &entry("rule.orch", "project:beta", "private", "roles: [orchestrator]\n", "Merge.\n"),
    );
    let checkout = f.path("checkout");
    manifest(&f, &checkout);
    let compose = |relative: bool| {
        let ctx = lords_core::compose::Context {
            audience: vec![lords_core::model::Audience::Sol],
            layers: lords_core::sync::place_layers(&f.stacks(), "beta-core", false),
            public_only: false,
            skip_stores: vec![],
            roles: None,
            by_reference: true,
            role_dir: Some(checkout.join(".lords")),
            relative_to: relative.then(|| checkout.clone()),
            default_role: None,
            default_role_inline: false,
        };
        lords_core::compose::compose(&f.stacks(), &ctx).text
    };
    let sep = std::path::MAIN_SEPARATOR_STR;
    let relative = compose(true);
    assert!(
        relative.contains(&format!("In `..{sep}priv{sep}project{sep}beta{sep}intent`:\n"))
            && relative.contains("each role's file in `.lords` has them"),
        "{relative}"
    );
    let absolute = compose(false);
    let intent = f.path("priv/project/beta/intent").display().to_string().replace('/', sep);
    assert!(absolute.contains(&format!("In `{intent}`:\n")), "{absolute}");
}

// Spec (design: Memory, one-line hooks): an index line's hook is at most 100 characters, cut at a word, with "…";
// the note's file keeps the whole description.
#[test]
fn index_hooks_are_cut_at_a_word() {
    let f = Fixture::new();
    let long = "word ".repeat(40);
    f.write(
        "priv/project/beta/memory/n.md",
        &entry(
            "memory.n",
            "project:beta",
            "private",
            &format!("description: \"{}\"\n", long.trim()),
            "Note.\n",
        ),
    );
    let checkout = f.path("checkout");
    f.write("checkout/README.md", "a repo\n");
    f.git_init("checkout");
    manifest(&f, &checkout);
    let place =
        proposed(&plan(&f.stacks(), &f.opts()).unwrap(), &checkout.join("AGENTS.override.md"));
    let line = place.lines().find(|l| l.starts_with("- n: ")).expect("an index line");
    let hook = line.trim_start_matches("- n: ").split(" (`").next().unwrap();
    assert!(hook.ends_with("word…") && hook.chars().count() <= 101, "{hook}");
}
