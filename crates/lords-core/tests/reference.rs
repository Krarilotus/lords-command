//! What goes by reference in a place's agent files (orchestrator T1, 25 Oct: "In repo files, intent goes by
//! reference (an index line plus the file, read on demand, like memory), with the full text only in the role
//! files … Don't raise Codex's byte limit: silent truncation is the failure").

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

// Spec (above): in the files every agent in a checkout reads, owner intent is an index line (its title and file)
// under a heading that says it binds; the role files carry its text; rules for every development role stay inline,
// while a rule for only some roles is a reference there too (its roles' files carry it).
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
    let p = plan(&f.stacks(), &lords_core::sync::Options::default()).unwrap();

    let place = proposed(&p, &checkout.join("AGENTS.override.md"));
    let goals = f
        .path("priv/project/beta/intent/goals.md")
        .display()
        .to_string()
        .replace('/', std::path::MAIN_SEPARATOR_STR);
    assert!(place.contains("# Owner intent\n\nThe owner's intent, binding."), "{place}");
    assert!(
        place.contains(&format!("- Release goals (`{goals}`)"))
            && !place.contains("Exceed, not parity."),
        "{place}"
    );
    assert!(place.contains("Gate before merge."), "a rule for every development role stays inline");
    assert!(
        place.contains("- (orchestrator only) Merge order (`")
            && !place.contains("Merge lanes one at a time."),
        "{place}"
    );

    let orch = proposed(&p, &checkout.join(".lords/role-orchestrator.md"));
    assert!(
        orch.contains("Exceed, not parity.") && orch.contains("Merge lanes one at a time."),
        "{orch}"
    );
    let coder = proposed(&p, &checkout.join(".lords/role-coder.md"));
    assert!(coder.contains("Exceed, not parity.") && !coder.contains("Merge lanes"), "{coder}");
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
    let place = proposed(
        &plan(&f.stacks(), &Options::default()).unwrap(),
        &checkout.join("AGENTS.override.md"),
    );
    let line = place.lines().find(|l| l.starts_with("- n: ")).expect("an index line");
    let hook = line.trim_start_matches("- n: ").split(" (`").next().unwrap();
    assert!(hook.ends_with("word…") && hook.chars().count() <= 101, "{hook}");
}
