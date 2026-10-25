//! The serialisable views (owner, 25 Oct: "one local web UI"; orchestrator: "a thin, serialisable view model …
//! one function per view, with tests").

mod common;
use common::*;
use lords_core::compose::{compose, Context, Layers};
use lords_core::finding::Severity;
use lords_core::model::Audience;
use lords_core::privacy::Rules;
use lords_core::sync::{plan, Options};
use lords_core::view;

// Spec: the entry list has every entry of both stacks, ordered by layer (public global first), with what a list
// line shows: kind, scope, stack, where it came from, and its worst finding (info doesn't count).
#[test]
fn the_entries_view_lists_both_stacks_with_their_worst_finding() {
    let f = Fixture::new();
    f.write("pub/global/rules/a.md", &entry("rule.a", "global", "public", "", "A.\n"));
    f.write(
        "priv/project/beta/rules/b.md",
        &entry("rule.b", "project:beta", "private", "links: [rule.none]\n", "B.\n"),
    );
    let s = f.stacks();
    let found = lords_core::check::check(&s, &Rules::public());
    let v = view::entries(&s, &found);
    let ids: Vec<&str> = v.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, ["rule.a", "rule.b"]);
    assert_eq!(
        (v[0].kind, v[0].stack.as_str(), v[0].scope.as_str(), v[0].origin),
        ("rule", "public", "global", "own")
    );
    assert_eq!(
        (v[0].worst, v[1].worst),
        (None, Some(Severity::Error)),
        "rule.b links a missing entry"
    );
    assert_eq!(v[1].findings.errors, 1);
    assert!(serde_json::to_string(&v).unwrap().contains("\"id\":\"rule.b\""), "serialisable");
}

// Spec: the findings view counts by severity and lists the worst first.
#[test]
fn the_findings_view_counts_and_orders_by_severity() {
    let found = vec![
        lords_core::Finding::warning("w", "warn"),
        lords_core::Finding::error("e", "err"),
        lords_core::Finding::info("i", "note"),
    ];
    let v = view::findings(&found);
    assert_eq!(
        (v.counts.errors, v.counts.warnings, v.counts.candidates, v.counts.info),
        (1, 1, 0, 1)
    );
    let codes: Vec<&str> = v.items.iter().map(|f| f.code).collect();
    assert_eq!(codes, ["e", "w", "i"]);
}

// Spec: the plan view carries each target's action, diff and notes, and counts what would change.
#[test]
fn the_plan_view_carries_each_targets_diff_and_the_counts() {
    let f = Fixture::new();
    f.write("pub/global/rules/a.md", &entry("rule.a", "global", "public", "", "Rule A.\n"));
    let target = f.path("home/AGENTS.md");
    f.write(
        "priv/stack.toml",
        &format!("name = \"priv\"\nvisibility = \"private\"\n[[targets]]\nname = \"codex\"\nadapter = \"codex\"\nkind = \"instructions\"\npath = '{}'\n", target.display()),
    );
    let v =
        view::plan(&plan(&f.stacks(), &Options { user_only: true, ..Options::default() }).unwrap());
    assert_eq!((v.to_change, v.in_sync, v.conflicts), (1, 0, 0));
    assert!(v.items[0].changes && v.items[0].diff.contains("+Rule A."));
    assert!(serde_json::to_string(&v).unwrap().contains("\"action\":\"create\""));
}

// Spec (design: Sync, "Where each line comes from"): every line of composed text is listed in order; a line from
// an entry names the entry, its kind, scope and file line; headings, markers and blank lines name none.
#[test]
fn the_composed_view_maps_each_line_to_its_source() {
    let f = Fixture::new();
    f.write(
        "priv/global/rules/a.md",
        &entry("rule.a", "global", "private", "", "Line one.\nLine two.\n"),
    );
    let s = f.stacks();
    let c = compose(
        &s,
        &Context {
            audience: vec![Audience::Opus],
            layers: Layers::Global,
            public_only: false,
            skip_stores: vec![],
            roles: None,
        },
    );
    let v = view::composed(&s, &c);
    assert_eq!(v.lines.len(), c.text.lines().count());
    let two = v.lines.iter().find(|l| l.text == "Line two.").unwrap();
    let src = two.source.as_ref().unwrap();
    assert_eq!(
        (src.id.as_str(), src.kind, src.scope.as_str(), src.stack.as_str(), src.file_line),
        ("rule.a", "rule", "global", "private", 13)
    );
    assert!(v
        .lines
        .iter()
        .filter(|l| l.text.starts_with("# ") || l.text.trim().is_empty())
        .all(|l| l.source.is_none()));
}
