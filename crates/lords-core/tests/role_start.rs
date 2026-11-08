//! A session's own role at launch (F10, design note Harness/notes/2026-11-02-lords-role-at-launch.md): a session T3
//! starts with `delegate_task` reads only its place's file, which carries the place's default role. Sync writes a start
//! per role, `.lords/start-<role>.md`, holding what of that role the place's file doesn't carry; the `role` guard gives
//! it at the first prompt that names the role (tests/role.rs in the CLI crate).

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;
use common::*;
use lords_core::guard::launch_role;
use lords_core::sync::plan;
use std::path::PathBuf;

/// A repo `beta-core` in project `beta` (default role orchestrator, both adapters), with five roles and one entry of
/// each shape a role's start must carry or leave out (each body says whose it is).
fn place() -> (Fixture, PathBuf) {
    let f = Fixture::new();
    let checkout = f.path("checkout");
    f.write("checkout/README.md", "a repo\n");
    f.git_init("checkout");
    f.write(
        "priv/stack.toml",
        &format!(
            "name = \"priv\"\nvisibility = \"private\"\nroles = [\"orchestrator\", \"coder\", \"helper\", \"reviewer\", \"product\"]\nchild_role = \"helper\"\n[projects.beta]\n[repos.beta-core]\nproject = \"beta\"\ncheckouts = ['{}']\nworktrees = false\ndefault_role = \"orchestrator\"\n",
            checkout.display()
        ),
    );
    let put = |slug: &str, yaml: &str, body: &str| {
        f.write(
            &format!("priv/project/beta/rules/{slug}.md"),
            &entry(&format!("rule.{slug}"), "project:beta", "private", yaml, body),
        );
    };
    put("all", "start: core\ncore: Every role's line.\n", "ALL-BODY\n");
    put(
        "verdicts",
        "roles: [orchestrator, reviewer]\nstart: core\ncore: The verdicts line.\n",
        "V\n",
    );
    put("tests", "roles: [coder, reviewer]\nstart: core\ncore: The tests line.\n", "T\n");
    put("push", "roles: [coder]\nstart: core\ncore: The coder's line.\n", "P\n");
    put(
        "split",
        "roles: [orchestrator, reviewer]\nstart: core\ncore:\n- line: The lead's half.\n  roles: [orchestrator]\n- line: The reviewer's half.\n  roles: [reviewer]\n",
        "S\n",
    );
    put("readonly", "roles: [reviewer]\n", "REVIEWER-WHOLE-BODY\n");
    (f, checkout)
}

// Spec (gap 11 design note §0d.5: "the date is read once per H3 run and once per whole delivery"; sol-loop r1, S5: one
// date per plan): a plan made on a given date composes the starts on that date. A temporary coder rule that has ended by
// then is in no start, though it hasn't ended today.
#[test]
fn a_plan_composes_the_starts_on_its_own_date() {
    let (f, checkout) = place();
    let trial = "roles: [coder]\ntemporary:\n  until: 2999-01-01\n  review_by: 2999-12-31\n  reason: a trial\n";
    f.write(
        "priv/project/beta/rules/trial.md",
        &entry("rule.trial", "project:beta", "private", trial, "TRIAL-BODY\n"),
    );
    let dir = checkout.join(".lords");
    let now = plan(&f.stacks(), &f.opts()).unwrap();
    assert!(planned_in(&now, &dir, "start-coder.md").contains("TRIAL-BODY"), "the fixture's rule");
    let later = lords_core::sync::plan_on(&f.stacks(), &f.opts(), "2999-06-01").unwrap();
    let start = planned_in(&later, &dir, "start-coder.md");
    assert!(start.contains("The coder's line.") && !start.contains("TRIAL-BODY"), "{start}");
}

// Spec (F10, the Lords owner's spec: "a top-level session starts with its role's lines, not only the place's default
// role"; design note D2): each declared role but `product` (product runs read no place file) gets `.lords/start-<role>.md`,
// which names the role as given at launch and the role file, and carries the role's lines for only some roles and its
// whole entries with no class; never a line every role has, nor anything of a role it doesn't bind.
#[test]
fn each_role_gets_a_start_of_what_the_place_file_leaves_out() {
    let (f, checkout) = place();
    let p = plan(&f.stacks(), &f.opts()).unwrap();
    let dir = checkout.join(".lords");
    let reviewer = planned_in(&p, &dir, "start-reviewer.md");
    for want in [
        "You start as a reviewer here",
        "at launch",
        "role-reviewer.md",
        "- rule.verdicts: The verdicts line.",
        "- rule.tests: The tests line.",
        "The reviewer's half.",
        "REVIEWER-WHOLE-BODY",
    ] {
        assert!(reviewer.contains(want), "{want} in:\n{reviewer}");
    }
    for never in ["Every role's line.", "The coder's line.", "The lead's half.", "ALL-BODY"] {
        assert!(!reviewer.contains(never), "{never} not in:\n{reviewer}");
    }
    let coder = planned_in(&p, &dir, "start-coder.md");
    assert!(coder.contains("The coder's line.") && coder.contains("The tests line."), "{coder}");
    assert!(!coder.contains("The verdicts line.") && !coder.contains("REVIEWER-WHOLE"), "{coder}");
    let orchestrator = planned_in(&p, &dir, "start-orchestrator.md");
    assert!(orchestrator.contains("The lead's half."), "{orchestrator}");
    assert_eq!(planned_in(&p, &dir, "start-product.md"), "", "no start for product");
}

/// The core lines a composed file carries: each `- id…: text` line's text after the id and its label.
fn core_lines(text: &str) -> Vec<String> {
    text.lines()
        .filter(|l| l.starts_with("- ") && l.contains(": "))
        .filter_map(|l| l.split_once(": ").map(|(_, t)| t.to_string()))
        .collect()
}

// Spec (L1, design §5.2: "a probe quotes every applicable core line of the role"; the invariant behind it): for every
// role and both adapters, each core line of the role's whole file reaches a session of that role through the place's
// file or the role's start, so a session given its start has every core line of its role at its first prompt.
#[test]
fn the_place_file_and_the_start_together_hold_every_core_line_of_the_role() {
    let (f, checkout) = place();
    let p = plan(&f.stacks(), &f.opts()).unwrap();
    let dir = checkout.join(".lords");
    let file = |name: &str| {
        p.items
            .iter()
            .find(|i| i.path == checkout.join(name))
            .and_then(|i| i.proposed.clone())
            .unwrap_or_default()
    };
    for place_file in ["CLAUDE.local.md", "AGENTS.override.md"] {
        let place_lines = core_lines(&file(place_file));
        assert!(!place_lines.is_empty(), "{place_file} planned");
        for role in ["orchestrator", "coder", "helper", "reviewer"] {
            let whole = planned_in(&p, &dir, &format!("role-{role}.md"));
            let start = core_lines(&planned_in(&p, &dir, &format!("start-{role}.md")));
            for line in core_lines(&whole) {
                assert!(
                    place_lines.contains(&line) || start.contains(&line),
                    "{role}, {place_file}: `{line}` reaches it nowhere"
                );
            }
        }
    }
}

// Spec (the Lords owner's option 1, 1 Nov: one hook gives at most 9,500 characters; design note D2: a start that
// doesn't fit is still written, and the plan says so, because the guard then points the session at its role file): a
// reviewer's start past 9,500 characters is written whole, the role files' item notes it with its size and the limit,
// and the plan holds.
#[test]
fn a_start_past_one_hook_is_written_and_noted() {
    let (f, checkout) = place();
    let lines: Vec<String> =
        (0..30).map(|i| format!("- line: \"{i:02} {}\"", "x".repeat(390))).collect();
    f.write(
        "priv/project/beta/rules/long.md",
        &entry(
            "rule.long",
            "project:beta",
            "private",
            &format!("roles: [reviewer]\nstart: core\ncore:\n{}\n", lines.join("\n")),
            "Long.\n",
        ),
    );
    let p = plan(&f.stacks(), &f.opts()).unwrap();
    let item = p.items.iter().find(|i| i.path == checkout.join(".lords")).expect("the role files");
    assert_ne!(item.action, lords_core::sync::Action::Conflict, "{:?}", item.notes);
    let start = planned_in(&p, &checkout.join(".lords"), "start-reviewer.md");
    assert!(start.contains("29 xxx"), "written whole");
    assert!(
        item.notes.iter().any(|n| n.contains("start-reviewer.md") && n.contains("9500")),
        "{:?}",
        item.notes
    );
    assert!(!p.over_limit, "the plan holds");
}

// Spec (design note D1 as revised after review r1, F10-R1-01, and its §5 inputs; T3's `taskPrompt`,
// OrchestratorMcpService.ts:608: `Act as the ${role} sub-agent for this task.\n\n${task}`, no prefix for `general`):
// where T3's prefix is there, it alone decides: implementation → coder, review → reviewer, research → helper, any other
// no role. Without it, the role is the first word after `Role:` on the prompt's first line, lower-cased, ignoring a
// heading mark and Markdown emphasis but never a quote mark. Either signal starts at column 0 of the first line (SR-01).
// A line further down, an indented one, a word that can't be a file name, or `Role:` alone names nothing.
#[test]
fn the_role_comes_from_t3s_prefix_or_else_the_briefs_own_first_line() {
    let t3 = |r: &str, task: &str| format!("Act as the {r} sub-agent for this task.\n\n{task}");
    let cases: Vec<(String, Option<&str>)> = vec![
        ("Role: reviewer\nReview this.".into(), Some("reviewer")),
        ("Role: Reviewer.\n".into(), Some("reviewer")),
        // Review of the simplified delta, F10-SR-01 (the Lords owner: "a signal counts only when it starts at column 0
        // of the prompt's first line, with no trimming of leading whitespace"): an indented line, or one after a blank
        // first line, is quoted or later text, never a signal.
        ("\n\n  Role: Reviewer.\n".into(), None),
        ("\nRole: coder\n".into(), None),
        ("    Role: coder\n".into(), None),
        ("\tRole: coder\n".into(), None),
        (
            "    Act as the implementation sub-agent for this task.\n\nReview the quoted launch instruction above."
                .into(),
            None,
        ),
        ("**Role:** coder\n".into(), Some("coder")),
        ("**Role: coder** (`role-coder.md`)\n".into(), Some("coder")),
        ("# Role: helper\n".into(), Some("helper")),
        (
            "Role: coder (`role-coder.md` in `C:\\x\\.lords`), for the Lords owner.".into(),
            Some("coder"),
        ),
        ("Role: reviewer\r\nReview this.\r\n".into(), Some("reviewer")),
        ("\u{feff}Role: reviewer\n".into(), Some("reviewer")),
        ("role: lead\n".into(), Some("lead")),
        ("Role: product".into(), Some("product")),
        ("Fix the bug.\nRole: coder\n".into(), None),
        ("Role:\nReview.".into(), None),
        ("Role: ../x\n".into(), None),
        ("Role: 9lives\n".into(), None),
        (format!("Role: {}\n", "a".repeat(33)), None),
        ("".into(), None),
        ("\n \n".into(), None),
        (t3("review", "Look at the diff."), Some("reviewer")),
        (t3("implementation", "Build it."), Some("coder")),
        (t3("research", "Find out."), Some("helper")),
        (t3("design", "Sketch it."), None),
        (t3("test", "Test it."), None),
        (t3("review", ""), Some("reviewer")),
        (t3("implementation", "Role: ../x\nBuild it."), Some("coder")),
        ("Act as the review sub-agent.\n\nLook.".into(), None),
        // Review r1, F10-R1-01 (the Lords owner's contract: "The launcher's signal (T3's prefix) wins over a `Role:`
        // line. A `Role:` line counts only as the brief's own first line, and only when the launcher gave none"):
        // T3's prefix decides, also when it names no Lords role; quoted text is never the brief's own line.
        (t3("review", "Role: coder\nBuild it."), Some("reviewer")),
        (t3("design", "Role: coder\nSketch it."), None),
        ("> Role: coder\n> Implement the change.\n".into(), None),
        (
            t3(
                "review",
                "> Role: coder\n> Implement the change.\n\nReview whether this brief is safe to send.",
            ),
            Some("reviewer"),
        ),
    ];
    for (prompt, want) in cases {
        let got = launch_role(&prompt).map(|l| l.role);
        assert_eq!(got.as_deref(), want, "{prompt:?}");
    }
}
