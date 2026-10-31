//! The serialisable views (owner, 25 Oct: "one local web UI"; orchestrator: "a thin, serialisable view model …
//! one function per view, with tests").

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;
use common::*;
use lords_core::compose::{compose, Context, Layers};
use lords_core::finding::Severity;
use lords_core::model::{Audience, Kind, Visibility};
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
    let found = lords_core::check::check_at(&s, &Rules::public(), &f.home());
    let v = view::entries(&s, &found);
    let ids: Vec<&str> = v.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, ["rule.a", "rule.b"]);
    assert_eq!(
        (v[0].kind, v[0].stack, v[0].scope.as_str(), v[0].origin.label()),
        (Kind::Rule, Visibility::Public, "global", "own")
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
    let codes: Vec<&str> = v.items.iter().map(|f| f.code.as_str()).collect();
    assert_eq!(codes, ["e", "w", "i"]);
}

// Spec (brief: "the window renders the view structs whether it reads them locally or over HTTP"): every view the
// panel sends reads back from its JSON exactly as it was, every field of an entry's detail included (an adapted
// origin, a retirement, relations, evidence, basis, a skill's two files, its version), and each finding keeps its
// one-line text.
#[test]
fn every_view_reads_back_from_its_json_as_it_was() {
    let f = Fixture::new();
    f.write(
        "pub/global/rules/a.md",
        &entry("rule.a", "global", "public", "topics: [timing]\nlinks: [rule.none]\nevidence:\n  - summary: a case\n    date: 2026-10-30\n    links: [rule.b]\nbasis:\n  - quote: in his words\n    date: 2026-10-30\n", "A.\n"),
    );
    f.write(
        "priv/global/rules/b.md",
        &entry("rule.b", "global", "private", "", "B.\n").replace(
            "status: active\n",
            "status: retired\nretired:\n  date: 2026-10-31\n  reason: done\nsuperseded_by: rule.a\n",
        ),
    );
    f.write(
        "priv/global/skills/s/lords.yaml",
        "id: skill.s\nkind: skill\ntitle: s\naudience: [opus, sol]\nscope: global\nstatus: active\nvisibility: private\norigin:\n  type: adapted\n  repo: someone/skills\n  commit: 0123abc\n  path: skills/s\n  licence: MIT\n",
    );
    f.write("priv/global/skills/s/SKILL.md", "---\nname: s\n---\nDo it.\n");
    let s = f.stacks();
    let found = lords_core::check::check_at(&s, &Rules::public(), &f.home());
    let v = view::stacks(&s);
    let read: view::StacksView = serde_json::from_str(&serde_json::to_string(&v).unwrap()).unwrap();
    assert_eq!(read, v);
    let skill = v.entries.iter().find(|e| e.id == "skill.s").unwrap();
    assert_eq!(skill.version, s.find("skill.s").unwrap().version);
    assert!(skill.body.ends_with("Do it.\n") && skill.source.is_some());
    let retired = v.entries.iter().find(|e| e.id == "rule.b").unwrap();
    assert!(!retired.active && retired.retired.is_some() && retired.superseded_by.is_some());
    let a = v.entries.iter().find(|e| e.id == "rule.a").unwrap();
    assert_eq!((a.evidence.len(), a.basis.len(), a.links.len()), (1, 1, 1));
    assert_eq!(v.topics.for_stack(Visibility::Private), ["delegation", "timing", "t", "x"]);

    let fv = view::findings(&found);
    let read: view::FindingsView =
        serde_json::from_str(&serde_json::to_string(&fv).unwrap()).unwrap();
    assert_eq!(read, fv);
    let missing = fv.items.iter().find(|i| i.entry.as_deref() == Some("rule.a")).unwrap();
    assert!(missing.text.contains("rule.none"), "{}", missing.text);
    assert_eq!(fv.worst().get("rule.a"), Some(&Severity::Error));

    let c = lords_core::sync::user_composed(&s, lords_core::adapters::get("claude").unwrap());
    let cv = view::composed(&s, &c);
    let read: view::ComposedView =
        serde_json::from_str(&serde_json::to_string(&cv).unwrap()).unwrap();
    assert_eq!(read, cv);
    assert!(cv.lines.iter().any(|l| l.source.is_some()));
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
    let v = view::plan(&plan(&f.stacks(), &Options { user_only: true, ..f.opts() }).unwrap());
    assert_eq!((v.to_change, v.in_sync, v.conflicts), (1, 0, 0));
    assert!(v.items[0].changes && v.items[0].diff.contains("+Rule A."));
    assert!(serde_json::to_string(&v).unwrap().contains("\"action\":\"create\""));
    let read: view::PlanView = serde_json::from_str(&serde_json::to_string(&v).unwrap()).unwrap();
    assert_eq!(read, v, "reads back as it was");
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
            by_reference: false,
            role_dir: None,
            relative_to: None,
            default_role: None,
            default_role_inline: false,
        },
    );
    let v = view::composed(&s, &c);
    assert_eq!(v.lines.len(), c.text.lines().count());
    let two = v.lines.iter().find(|l| l.text == "Line two.").unwrap();
    let src = two.source.as_ref().unwrap();
    assert_eq!(
        (src.id.as_str(), src.kind, src.scope.as_str(), src.stack, src.file_line),
        ("rule.a", Some(Kind::Rule), "global", Visibility::Private, 13)
    );
    assert!(v
        .lines
        .iter()
        .filter(|l| l.text.starts_with("# ") || l.text.trim().is_empty())
        .all(|l| l.source.is_none()));
}
