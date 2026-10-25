//! Temporary entries (owner, 25 Oct: "on temporary rules we need to see that if we have them, that we actually
//! don't poison the context with them forever as being temporary in the past"; orchestrator: an end condition and a
//! review date on every one; ended ones reported, never composed, retired by sync with a commit).

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;
use common::*;
use lords_core::compose::{compose, Context, Layers};
use lords_core::model::Audience;
use lords_core::Severity;

const PAST: &str = "2000-01-01";
const FUTURE: &str = "2999-12-31";

fn temp(block: &str) -> String {
    format!("temporary:\n{block}  reason: a trial\n")
}

fn user_level(f: &Fixture) -> String {
    let ctx = Context {
        audience: vec![Audience::Opus],
        layers: Layers::Global,
        public_only: false,
        skip_stores: vec![],
        roles: None,
        by_reference: false,
        role_dir: None,
        relative_to: None,
        default_role: None,
    };
    compose(&f.stacks(), &ctx).text
}

// Spec (above): an entry past its `until` date has ended: `check` reports `temporary-expired` (a warning that fails
// under --strict) and no composition carries it; a live one is composed with its end in one short clause.
#[test]
fn a_date_ends_a_temporary_entry() {
    let f = Fixture::new();
    f.write(
        "pub/global/rules/old.md",
        &entry(
            "rule.old",
            "global",
            "public",
            &temp(&format!("  until: {PAST}\n  review_by: {FUTURE}\n")),
            "Old trial rule.\n",
        ),
    );
    f.write(
        "pub/global/rules/live.md",
        &entry(
            "rule.live",
            "global",
            "public",
            &temp(&format!("  until: {FUTURE}\n  review_by: {FUTURE}\n")),
            "Live trial rule.\n",
        ),
    );
    let found = f.check();
    assert!(
        has(&found, "temporary-expired", "rule.old")
            && !has(&found, "temporary-expired", "rule.live")
    );
    assert_eq!(
        found.iter().find(|x| x.code == "temporary-expired").unwrap().severity,
        Severity::Warning
    );
    assert!(
        !lords_core::check::has_errors(&found, false)
            && lords_core::check::has_errors(&found, true),
        "fails only --strict"
    );
    let text = user_level(&f);
    assert!(
        !text.contains("Old trial rule.")
            && text.contains(&format!("(temporary, until {FUTURE})\nLive trial rule.")),
        "{text}"
    );
}

// Spec (above: "when: <status-entry id + the state that ends it>"; "status entries … carry a state field, so a
// when condition is checkable"): the entry holds while the status has another state and ends once it has the end
// state; its label names the status.
#[test]
fn a_status_reaching_its_end_state_ends_a_temporary_entry() {
    let f = Fixture::new();
    let status = |state: &str| {
        entry(
            "status.triage",
            "global",
            "private",
            &format!("state: {state}\n"),
            "Triage of the old line.\n",
        )
        .replace("title: status.triage", "title: the triage")
    };
    f.write("priv/global/status/triage.md", &status("in-progress"));
    let when = temp(&format!(
        "  when:\n    status: status.triage\n    state: done\n  review_by: {FUTURE}\n"
    ));
    f.write(
        "priv/global/rules/caps.md",
        &entry("rule.caps", "global", "private", &when, "Two helpers per lead.\n"),
    );
    assert_eq!(errors(&f.check()), Vec::<&str>::new(), "{:#?}", f.check());
    assert!(user_level(&f).contains("(temporary, until the triage: done)\nTwo helpers per lead."));
    f.write("priv/global/status/triage.md", &status("done"));
    assert!(has(&f.check(), "temporary-expired", "rule.caps"));
    assert!(!user_level(&f).contains("Two helpers per lead."));
}

// Spec (above: "Every temporary entry needs an end condition and a review date"): a review date that passes
// unrenewed ends the entry, even before its end condition.
#[test]
fn an_unrenewed_review_date_ends_a_temporary_entry() {
    let f = Fixture::new();
    f.write(
        "pub/global/rules/r.md",
        &entry(
            "rule.r",
            "global",
            "public",
            &temp(&format!("  until: {FUTURE}\n  review_by: {PAST}\n")),
            "Review me.\n",
        ),
    );
    assert!(has(&f.check(), "temporary-expired", "rule.r"));
    assert!(!user_level(&f).contains("Review me."));
}

// Spec (schema): a temporary block needs an end condition (until or when), a review date and a reason; `state` is
// for status entries only.
#[test]
fn the_schema_demands_an_end_and_keeps_state_on_status_entries() {
    let f = Fixture::new();
    f.write(
        "pub/global/rules/a.md",
        &entry(
            "rule.a",
            "global",
            "public",
            &format!("temporary:\n  review_by: {FUTURE}\n  reason: r\n"),
            "x\n",
        ),
    );
    assert!(errors(&f.check()).contains(&"schema"), "no end condition");
    f.write("pub/global/rules/a.md", &entry("rule.a", "global", "public", "state: done\n", "x\n"));
    assert!(errors(&f.check()).contains(&"schema"), "state on a rule");
}

// Spec (above: "an expired temporary entry is not composed at all. The tool retires it itself (status retired,
// reason "expired: <condition>", with a commit)"): sync's plan lists it; applying retires it with a commit, and
// none of the files sync writes (user level, the checkout's agent files, its role files) carries it.
#[test]
fn sync_retires_an_expired_entry_and_no_file_carries_it() {
    let f = Fixture::new();
    let dev = "roles: [orchestrator, coder]\n";
    for (id, scope, dir) in [
        ("rule.gone", "global", "priv/global/rules"),
        ("rule.gone-here", "repo:beta-core", "priv/repo/beta-core/rules"),
    ] {
        let block = format!("{dev}{}", temp(&format!("  until: {PAST}\n  review_by: {FUTURE}\n")));
        f.write(
            &format!("{dir}/{id}.md"),
            &entry(id, scope, "private", &block, &format!("Text of {id}.\n")),
        );
    }
    f.write(
        "priv/global/rules/stays.md",
        &entry("rule.stays", "global", "private", dev, "Text that stays.\n"),
    );
    let checkout = f.path("checkout");
    f.write("checkout/README.md", "a repo\n");
    f.git_init("checkout");
    let user = f.path("home/AGENTS.md");
    f.write(
        "priv/stack.toml",
        &format!(
            "name = \"priv\"\nvisibility = \"private\"\nroles = [\"orchestrator\", \"coder\"]\n[repos.beta-core]\ncheckouts = ['{}']\nworktrees = false\n[[targets]]\nname = \"codex\"\nadapter = \"codex\"\nkind = \"instructions\"\npath = '{}'\n",
            checkout.display(),
            user.display()
        ),
    );
    f.git_init("priv");
    let p = lords_core::sync::plan(&f.stacks(), &f.opts()).unwrap();
    let listed: Vec<&str> = p
        .items
        .iter()
        .filter(|i| i.action == lords_core::sync::Action::RetireEntry)
        .map(|i| i.target.as_str())
        .collect();
    assert_eq!(listed.len(), 2, "{listed:?}");

    lords_core::sync::import_and_apply(&f.pub_root(), Some(&f.priv_root()), &f.opts()).unwrap();
    let s = f.stacks();
    for id in ["rule.gone", "rule.gone-here"] {
        let e = s.find(id).unwrap();
        assert!(
            !e.active()
                && e.meta
                    .retired
                    .as_ref()
                    .unwrap()
                    .reason
                    .starts_with("expired: until 2000-01-01 reached"),
            "{id}"
        );
    }
    let log = git(&f.priv_root(), &["log", "--format=%s"]);
    assert!(
        log.contains("Retire rule.gone: expired") && log.contains("Retire rule.gone-here: expired"),
        "{log}"
    );
    let written: Vec<String> = [
        user.clone(),
        checkout.join("AGENTS.override.md"),
        checkout.join("CLAUDE.local.md"),
        checkout.join(".lords/role-orchestrator.md"),
        checkout.join(".lords/role-coder.md"),
    ]
    .iter()
    .map(|p| std::fs::read_to_string(p).unwrap_or_default())
    .collect();
    assert!(written.iter().any(|t| t.contains("Text that stays.")), "the rest is written");
    assert!(
        written.iter().all(|t| !t.contains("Text of rule.gone")),
        "no file carries an expired entry"
    );
}
