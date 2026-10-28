//! Temporary entries (owner, 25 Oct: "on temporary rules we need to see that if we have them, that we actually
//! don't poison the context with them forever as being temporary in the past"; design v2 §4: a reason and at least
//! one date on every one; expired ones reported, never composed, retired by sync with a commit; review-due ones
//! reported and composed, labelled; invalid ones are errors).

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;
use common::*;
use lords_core::compose::{compose, Context, Layers};
use lords_core::model::{valid_date, Audience, Lifecycle};
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
        default_role_inline: false,
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

// Spec (design v2 §4, `review_by`: "`as_of > review_by` means review due, not expiry. Include a visible 'review
// due since DATE' label in context … until an actual end condition occurs"; it "deliberately replaces today's
// auto-expiry after review date"): a passed review date is reported as due, never as expired, and the entry is
// still composed, labelled.
#[test]
fn a_passed_review_date_makes_an_entry_review_due_and_keeps_it_composed() {
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
    let found = f.check();
    assert!(has(&found, "temporary-review-due", "rule.r"), "{found:#?}");
    assert!(!has(&found, "temporary-expired", "rule.r"), "{found:#?}");
    let text = user_level(&f);
    assert!(
        text.contains(&format!("(temporary, until {FUTURE}; review due since {PAST})\nReview me.")),
        "{text}"
    );
}

// Spec (design v2 §4: "at least one valid calendar date (`until` or `review_by`)"; "A review-only temporary entry
// is allowed"; schema): a temporary block needs a reason and a date, a review date alone is enough; `state` is for
// status entries only.
#[test]
fn the_schema_demands_a_date_and_keeps_state_on_status_entries() {
    let f = Fixture::new();
    f.write(
        "pub/global/rules/a.md",
        &entry("rule.a", "global", "public", "temporary:\n  reason: r\n", "x\n"),
    );
    assert!(errors(&f.check()).contains(&"schema"), "no date");
    let when_only = "temporary:\n  when:\n    status: status.s\n    state: done\n  reason: r\n";
    f.write("pub/global/rules/a.md", &entry("rule.a", "global", "public", when_only, "x\n"));
    assert!(errors(&f.check()).contains(&"schema"), "a when without a review date");
    f.write(
        "pub/global/rules/a.md",
        &entry("rule.a", "global", "public", &temp(&format!("  review_by: {FUTURE}\n")), "x\n"),
    );
    assert_eq!(errors(&f.check()), Vec::<&str>::new(), "review-only is allowed");
    assert!(user_level(&f).contains(&format!("(temporary, review by {FUTURE})\nx")));
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

/// The temporary-entry findings `check` reports for the stacks on the day `as_of`.
fn on(f: &Fixture, as_of: &str) -> Vec<lords_core::Finding> {
    let mut out = Vec::new();
    lords_core::check::temporaries(&f.stacks(), as_of, &mut out);
    out
}

fn codes_on(f: &Fixture, as_of: &str) -> Vec<&'static str> {
    on(f, as_of).iter().map(|x| x.code).collect()
}

fn rule(f: &Fixture, block: &str) {
    f.write("pub/global/rules/t.md", &entry("rule.t", "global", "public", block, "Trial text.\n"));
}

// Spec (design v2 §4, its example: "`until=2026-10-30`, `review_by=2026-10-28` … On 29 Oct UTC, it is returned
// review due; at 00:00 UTC on 30 Oct, it is excluded"; Check: "Report due and expired entries separately; `--strict`
// treats them as action-required"): due on the 29th only, expired from the 30th, each failing only --strict.
#[test]
fn the_design_example_is_due_on_the_29th_and_expired_on_the_30th() {
    let f = Fixture::new();
    rule(&f, &temp("  until: 2026-10-30\n  review_by: 2026-10-28\n"));
    assert_eq!(codes_on(&f, "2026-10-28"), Vec::<&str>::new());
    let due = on(&f, "2026-10-29");
    assert_eq!(due.iter().map(|x| x.code).collect::<Vec<_>>(), ["temporary-review-due"]);
    assert_eq!(due[0].severity, Severity::Warning);
    assert!(
        due[0].message.contains("2026-10-28"),
        "the due finding names the review date: {}",
        due[0].message
    );
    assert!(
        !lords_core::check::has_errors(&due, false) && lords_core::check::has_errors(&due, true)
    );
    let gone = on(&f, "2026-10-30");
    assert_eq!(gone.iter().map(|x| x.code).collect::<Vec<_>>(), ["temporary-expired"]);
    assert!(
        !lords_core::check::has_errors(&gone, false) && lords_core::check::has_errors(&gone, true)
    );
}

// Spec (design v2 §4, `until`: "Exclusive UTC calendar boundary: `as_of >= until` means expired"): the day before
// holds, the day itself has ended.
#[test]
fn until_holds_the_day_before_and_ends_on_the_day() {
    let f = Fixture::new();
    rule(&f, &temp(&format!("  until: 2026-03-01\n  review_by: {FUTURE}\n")));
    assert_eq!(codes_on(&f, "2026-02-28"), Vec::<&str>::new());
    assert_eq!(codes_on(&f, "2026-03-01"), ["temporary-expired"]);
}

// Spec (design v2 §4, `review_by`: "`as_of > review_by` means review due, not expiry"): on the review date nothing
// is due; the day after it is due and still not expired.
#[test]
fn review_by_is_due_only_the_day_after() {
    let f = Fixture::new();
    rule(&f, &temp(&format!("  until: {FUTURE}\n  review_by: 2026-12-31\n")));
    assert_eq!(codes_on(&f, "2026-12-31"), Vec::<&str>::new());
    assert_eq!(codes_on(&f, "2027-01-01"), ["temporary-review-due"]);
}

// Spec (design v2 §4: "A review-only temporary entry is allowed"; it is "review due" after its date "until an
// actual end condition occurs"): with no end condition it never expires, however late.
#[test]
fn a_review_only_entry_becomes_due_and_never_expires() {
    let f = Fixture::new();
    rule(&f, &temp("  review_by: 2026-10-28\n"));
    assert_eq!(errors(&f.check()), Vec::<&str>::new(), "{:#?}", f.check());
    assert_eq!(codes_on(&f, "2026-10-28"), Vec::<&str>::new());
    assert_eq!(codes_on(&f, "2026-10-29"), ["temporary-review-due"]);
    assert_eq!(codes_on(&f, "2999-01-01"), ["temporary-review-due"]);
}

// Spec (design v2 §4, `when`: "An unreadable/dangling target is invalid and a check error, not silent indefinite
// validity"; Check: "Reject … invalid end conditions"): a `when` watching no entry, or a status entry with no state
// to read, is an error, and the entry is not composed.
#[test]
fn a_when_with_a_missing_target_is_an_invalid_entry() {
    let f = Fixture::new();
    f.write(
        "pub/global/status/blank.md",
        &entry("status.blank", "global", "public", "", "No state.\n"),
    );
    for target in ["status.nowhere", "status.blank"] {
        rule(
            &f,
            &temp(&format!(
                "  when:\n    status: {target}\n    state: done\n  review_by: {FUTURE}\n"
            )),
        );
        let found = on(&f, "2026-10-28");
        assert_eq!(
            found.iter().map(|x| x.code).collect::<Vec<_>>(),
            ["temporary-invalid"],
            "{target}"
        );
        assert_eq!(found[0].severity, Severity::Error);
        assert!(!user_level(&f).contains("Trial text."), "{target}");
    }
}

// Spec (design v2 §4: "reject malformed/impossible dates"; Check: "Reject missing reason/date"): an impossible
// calendar date in either field, or a blank reason, is an error and the entry is not composed; a real leap day is
// fine.
#[test]
fn impossible_dates_and_a_blank_reason_are_invalid() {
    let f = Fixture::new();
    for block in [
        format!("temporary:\n  until: 2026-02-30\n  review_by: {FUTURE}\n  reason: r\n"),
        format!("temporary:\n  until: {FUTURE}\n  review_by: 2025-02-29\n  reason: r\n"),
        "temporary:\n  review_by: 2026-13-01\n  reason: r\n".to_string(),
        "temporary:\n  review_by: 2026-04-31\n  reason: r\n".to_string(),
        format!("temporary:\n  until: {FUTURE}\n  review_by: {FUTURE}\n  reason: \"  \"\n"),
    ] {
        rule(&f, &block);
        let found = on(&f, "2026-01-01");
        assert_eq!(
            found.iter().map(|x| x.code).collect::<Vec<_>>(),
            ["temporary-invalid"],
            "{block}"
        );
        assert!(lords_core::check::has_errors(&found, false), "{block}");
        assert!(!user_level(&f).contains("Trial text."), "{block}");
    }
    rule(&f, &temp(&format!("  until: 2028-02-29\n  review_by: {FUTURE}\n")));
    assert_eq!(codes_on(&f, "2026-01-01"), Vec::<&str>::new());
    assert!(user_level(&f).contains("Trial text."));
}

// Spec (design v2 §4: "Sync retires only Expired" via the lifecycle; Check: "Do not auto-retire or auto-commit during
// a read/check"): sync plans no retirement for a review-due or an invalid entry, and neither check nor composing
// changes an expired entry's file.
#[test]
fn only_expired_entries_are_retired_and_reads_change_nothing() {
    let f = Fixture::new();
    f.write(
        "priv/global/rules/due.md",
        &entry(
            "rule.due",
            "global",
            "private",
            &temp(&format!("  until: {FUTURE}\n  review_by: {PAST}\n")),
            "Due.\n",
        ),
    );
    f.write(
        "priv/global/rules/bad.md",
        &entry(
            "rule.bad",
            "global",
            "private",
            &format!("temporary:\n  until: {PAST}\n  review_by: {FUTURE}\n  reason: \"\t\"\n"),
            "Bad.\n",
        ),
    );
    let gone = f.write(
        "priv/global/rules/gone.md",
        &entry(
            "rule.gone",
            "global",
            "private",
            &temp(&format!("  until: {PAST}\n  review_by: {FUTURE}\n")),
            "Gone.\n",
        ),
    );
    f.git_init("priv");
    let before = std::fs::read(&gone).unwrap();
    let _ = f.check();
    let _ = user_level(&f);
    assert_eq!(std::fs::read(&gone).unwrap(), before, "a read never retires");
    let p = lords_core::sync::plan(&f.stacks(), &f.opts()).unwrap();
    let listed: Vec<&str> = p
        .items
        .iter()
        .filter(|i| i.action == lords_core::sync::Action::RetireEntry)
        .map(|i| i.target.as_str())
        .collect();
    assert_eq!(listed, ["rule.gone (temporary entry)"]);
}

fn lifecycle_on(f: &Fixture, id: &str, as_of: &str) -> Lifecycle {
    let s = f.stacks();
    let e = s.find(id).unwrap();
    e.lifecycle(as_of, &|x| s.find(x))
}

// Spec (design v2 §4: "`model::Entry` evaluates `lifecycle(as_of, status_lookup)` as active, review-due, expired,
// retired or invalid"; the example's states; "reject … already-expired writes": a write whose `until` is the day
// of writing is already expired by the same evaluator): each state through the one evaluator.
#[test]
fn the_entry_evaluates_its_own_lifecycle() {
    let f = Fixture::new();
    rule(&f, &temp("  until: 2026-10-30\n  review_by: 2026-10-28\n"));
    assert_eq!(lifecycle_on(&f, "rule.t", "2026-10-28"), Lifecycle::Active);
    assert_eq!(
        lifecycle_on(&f, "rule.t", "2026-10-29"),
        Lifecycle::ReviewDue { since: "2026-10-28".into() }
    );
    assert_eq!(
        lifecycle_on(&f, "rule.t", "2026-10-30"),
        Lifecycle::Expired { why: "until 2026-10-30 reached".into() }
    );
    rule(&f, &temp("  until: 2026-10-28\n"));
    assert!(matches!(lifecycle_on(&f, "rule.t", "2026-10-28"), Lifecycle::Expired { .. }));
    f.write("pub/global/rules/p.md", &entry("rule.p", "global", "public", "", "Plain.\n"));
    assert_eq!(lifecycle_on(&f, "rule.p", "2999-01-01"), Lifecycle::Active, "no temporary block");
    f.write(
        "pub/global/rules/p.md",
        &entry("rule.p", "global", "public", "", "Plain.\n").replace(
            "status: active",
            "status: retired\nretired:\n  date: 2026-01-01\n  reason: r",
        ),
    );
    assert_eq!(lifecycle_on(&f, "rule.p", "2026-10-28"), Lifecycle::Retired);
}

// Spec (design v2 §4, `when`: "matching state expires it through the same model evaluator"; "Require a real status
// target"): the watched status's state ends it; a status with no state can't be read and is invalid.
#[test]
fn a_when_ends_through_the_same_evaluator() {
    let f = Fixture::new();
    let status = |extra: &str| entry("status.s", "global", "public", extra, "S.\n");
    rule(
        &f,
        &temp(&format!("  when:\n    status: status.s\n    state: done\n  review_by: {FUTURE}\n")),
    );
    f.write("pub/global/status/s.md", &status("state: open\n"));
    assert_eq!(lifecycle_on(&f, "rule.t", "2026-10-28"), Lifecycle::Active);
    f.write("pub/global/status/s.md", &status("state: done\n"));
    assert_eq!(
        lifecycle_on(&f, "rule.t", "2026-10-28"),
        Lifecycle::Expired { why: "status.s is done".into() }
    );
    f.write("pub/global/status/s.md", &status(""));
    assert!(matches!(lifecycle_on(&f, "rule.t", "2026-10-28"), Lifecycle::Invalid { .. }));
}

// Spec (design v2 §4: "Context returns its next validity boundary"): the next day the lifecycle changes by date:
// the day after the review date, then the `until` day, then none; none for an entry no date changes.
#[test]
fn next_boundary_is_the_next_day_the_lifecycle_changes() {
    let f = Fixture::new();
    rule(&f, &temp("  until: 2026-10-30\n  review_by: 2026-10-28\n"));
    let s = f.stacks();
    let e = s.find("rule.t").unwrap();
    assert_eq!(e.next_boundary("2026-10-27").as_deref(), Some("2026-10-29"));
    assert_eq!(e.next_boundary("2026-10-28").as_deref(), Some("2026-10-29"));
    assert_eq!(e.next_boundary("2026-10-29").as_deref(), Some("2026-10-30"));
    assert_eq!(e.next_boundary("2026-10-30"), None);
    // Each boundary is where the evaluator's answer changes, and the day before it is not.
    for (before, at) in [("2026-10-28", "2026-10-29"), ("2026-10-29", "2026-10-30")] {
        let on = |d: &str| e.lifecycle(d, &|x| s.find(x));
        assert_ne!(on(before), on(at), "{at}");
    }
    rule(&f, &temp("  review_by: 2026-12-31\n"));
    let s = f.stacks();
    let e = s.find("rule.t").unwrap();
    assert_eq!(e.next_boundary("2026-12-31").as_deref(), Some("2027-01-01"), "across a year");
    assert_eq!(e.next_boundary("2027-01-01"), None, "due for good");
    rule(&f, &temp("  review_by: 2028-02-28\n"));
    assert_eq!(
        f.stacks().find("rule.t").unwrap().next_boundary("2028-01-01").as_deref(),
        Some("2028-02-29"),
        "a leap day"
    );
    rule(&f, &temp("  until: 2026-02-30\n"));
    assert_eq!(f.stacks().find("rule.t").unwrap().next_boundary("2026-01-01"), None, "invalid");
    f.write("pub/global/rules/p.md", &entry("rule.p", "global", "public", "", "Plain.\n"));
    assert_eq!(f.stacks().find("rule.p").unwrap().next_boundary("2026-01-01"), None);
}

// Spec (design v2 §4: "reject malformed/impossible dates"; the task: "`2026-02-30` is invalid"): a real calendar
// date in YYYY-MM-DD form only.
#[test]
fn valid_date_accepts_only_real_calendar_dates() {
    for d in ["2026-10-28", "2024-02-29", "2000-02-29", "2026-12-31", "2026-01-01"] {
        assert!(valid_date(d), "{d}");
    }
    for d in [
        "2026-02-30",
        "2025-02-29",
        "1900-02-29",
        "2026-04-31",
        "2026-13-01",
        "2026-00-10",
        "2026-10-00",
        "2026-1-05",
        "2026-10-5",
        "26-10-28",
        "2026/10/28",
        "2026-10-28 ",
        "+026-10-28",
        "2026-1a-28",
        "",
    ] {
        assert!(!valid_date(d), "{d:?}");
    }
}
