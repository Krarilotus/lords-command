//! Core lines (the Lords owner, 31 Oct, plan L8: "each core or role entry gets a `core:` line … Agents start with the
//! core lines that apply to them, each with its id. The body stays exactly as it is, authoritative, opened by id").
//! An entry's `start` says how it reaches an agent at start: `core`, a line for every role it binds; `role`, a line
//! for some of them; `detail`, only its index entry. Design note: Harness/notes/2026-10-31-lords-core-lines-composer.md.

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;
use common::*;
use lords_core::core_lines::LINE_MAX_BYTES;
use lords_core::sync::{plan, Plan};
use std::path::{Path, PathBuf};

const ROLES: &str = "roles = [\"orchestrator\", \"coder\", \"helper\", \"product\"]\n";

/// A fixture whose public stack declares the roles; `extra` is more of its stack.toml.
fn stacks_with(extra: &str) -> Fixture {
    let f = Fixture::new();
    f.write(
        "pub/stack.toml",
        &format!("name = \"pub\"\nvisibility = \"public\"\n{ROLES}{extra}[projects.alpha]\n"),
    );
    f
}

fn rule(f: &Fixture, slug: &str, yaml: &str) {
    f.write(
        &format!("pub/global/rules/{slug}.md"),
        &entry(&format!("rule.{slug}"), "global", "public", yaml, "The whole rule.\n"),
    );
}

/// The codes of the core-line findings for `id`.
fn core_codes(f: &Fixture, id: &str) -> Vec<&'static str> {
    let mut v: Vec<_> = f
        .check()
        .into_iter()
        .filter(|x| x.code.starts_with("core-") && x.entry.as_deref() == Some(id))
        .map(|x| x.code)
        .collect();
    v.sort();
    v
}

// Spec (the brief: "`check` refuses one over the limit you set from the drafted values"): the limit counts UTF-8
// bytes, so a line of exactly the limit passes, whatever characters it ends with, and one byte more is refused.
#[test]
fn a_line_at_the_byte_limit_passes_and_one_byte_past_it_is_refused() {
    let f = stacks_with("");
    // Each line's last character is multi-byte, so a count of characters instead of bytes would pass the long ones.
    let ending = |tail: &str, bytes: usize| format!("{}{tail}", "a".repeat(bytes - tail.len()));
    let cases = [
        ("at-two", ending("é", LINE_MAX_BYTES), true),
        ("past-two", ending("é", LINE_MAX_BYTES + 1), false),
        ("at-three", ending("€", LINE_MAX_BYTES), true),
        ("past-three", ending("€", LINE_MAX_BYTES + 1), false),
    ];
    for (slug, line, _) in &cases {
        assert!(line.chars().count() <= LINE_MAX_BYTES, "{slug}: within the limit in characters");
        rule(&f, slug, &format!("start: core\ncore: \"{line}\"\n"));
    }
    assert!(errors(&f.check()).iter().all(|c| *c != "schema"), "every case loads");
    for (slug, line, passes) in cases {
        let codes = core_codes(&f, &format!("rule.{slug}"));
        let want: Vec<&str> = if passes { vec![] } else { vec!["core-line-long"] };
        assert_eq!(codes, want, "{slug}: {} bytes", line.len());
    }
}

// Spec (one line each, the bullet it renders as): a line break inside a core line is refused.
#[test]
fn a_line_with_a_line_break_is_refused() {
    let f = stacks_with("");
    rule(&f, "broken", "start: core\ncore: \"First half.\\nSecond half.\"\n");
    assert_eq!(core_codes(&f, "rule.broken"), vec!["core-line-break"]);
}

// Spec (the brief, inputs to attack: "a split line whose roles lie outside the entry's roles"; and the invariant,
// "nothing that binds it is dropped"): each class holds what it says. `core` reaches every role the entry binds, so a
// line set that leaves a bound role with only the index is refused; `role` names its roles on every line; `detail`
// has no line; no line names a role the entry doesn't bind; intent and memory take neither field.
#[test]
fn each_start_class_holds_what_it_says() {
    let f = stacks_with("");
    let cases: &[(&str, &str, &[&str])] = &[
        ("core-all", "start: core\ncore: Every role's line.\n", &[]),
        (
            "core-split",
            "roles: [orchestrator, coder]\nstart: core\ncore:\n- line: The lead's half.\n  roles: [orchestrator]\n- line: The coder's half.\n  roles: [coder]\n",
            &[],
        ),
        (
            "core-gap",
            "roles: [orchestrator, coder]\nstart: core\ncore:\n- line: Only the lead's.\n  roles: [orchestrator]\n",
            &["core-class"],
        ),
        (
            "role-subset",
            "roles: [orchestrator, coder]\nstart: role\ncore:\n- line: The lead's.\n  roles: [orchestrator]\n",
            &[],
        ),
        ("role-unnamed", "roles: [orchestrator, coder]\nstart: role\ncore: For whom?\n", &["core-class"]),
        (
            "outside",
            "roles: [coder]\nstart: role\ncore:\n- line: The lead's.\n  roles: [orchestrator]\n",
            &["core-roles"],
        ),
        ("detail", "start: detail\n", &[]),
        ("detail-line", "start: detail\ncore: A line.\n", &["core-class"]),
        ("unstarted-line", "core: A line with no class.\n", &["core-class"]),
    ];
    for (slug, yaml, _) in cases {
        rule(&f, slug, yaml);
    }
    f.write(
        "pub/global/intent/i.md",
        &entry("intent.i", "global", "public", "start: core\ncore: Intent.\n", "Intent.\n"),
    );
    for (slug, _, want) in cases {
        assert_eq!(core_codes(&f, &format!("rule.{slug}")), want.to_vec(), "{slug}");
    }
    assert_eq!(core_codes(&f, "intent.i"), vec!["core-class"], "intent keeps its title index");
    assert!(errors(&f.check()).iter().all(|c| *c != "schema"), "every case loads: {:?}", f.check());
}

// Spec (the brief, slice 5: "`check` refuses a start-class entry without a `core:` line only when a stack setting
// turns that on"): with the stack's `require_core`, an entry with no class, or a `core` or `role` one without its
// line, is refused, and so is intent without `start: detail` (its title in the index); `detail` and memory need no
// line. Without the setting the same entries pass, and the other stack's setting doesn't reach them.
#[test]
fn a_missing_line_is_refused_only_where_its_stack_requires_lines() {
    for required in [false, true] {
        let f = stacks_with(if required { "require_core = true\n" } else { "" });
        rule(&f, "unclassified", "");
        rule(&f, "pending", "start: core\n");
        rule(&f, "detail", "start: detail\n");
        rule(&f, "done", "start: core\ncore: A line.\n");
        f.write("pub/global/intent/i.md", &entry("intent.i", "global", "public", "", "Intent.\n"));
        f.write(
            "pub/global/intent/j.md",
            &entry("intent.j", "global", "public", "start: detail\n", "Intent.\n"),
        );
        f.write("pub/global/memory/m.md", &entry("memory.m", "global", "public", "", "A note.\n"));
        f.write("priv/global/rules/p.md", &entry("rule.p", "global", "private", "", "Private.\n"));
        let want: &[&str] = if required { &["core-missing"] } else { &[] };
        for id in ["rule.unclassified", "rule.pending", "intent.i"] {
            assert_eq!(core_codes(&f, id), want.to_vec(), "{id}, required {required}");
        }
        for id in ["rule.detail", "rule.done", "intent.j", "memory.m", "rule.p"] {
            assert!(core_codes(&f, id).is_empty(), "{id}, required {required}");
        }
    }
}

// ---------------------------------------------------------------- composition

const PAST: &str = "2000-01-01";

fn proposed(p: &Plan, path: &Path) -> String {
    p.items.iter().find(|i| i.path == path).and_then(|i| i.proposed.clone()).unwrap_or_default()
}

/// A repo `beta-core` in project `beta`, with `default_role` and both adapters, and one entry of each kind of start
/// (each body says whose it is, so a test sees whether the body or the line came through).
fn place(default_role: &str) -> (Fixture, PathBuf) {
    let f = Fixture::new();
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
    let both = "roles: [orchestrator, coder]\n";
    let put = |kind_dir: &str, slug: &str, id: &str, yaml: &str, body: &str| {
        f.write(
            &format!("priv/project/beta/{kind_dir}/{slug}.md"),
            &entry(id, "project:beta", "private", yaml, body),
        );
    };
    put(
        "rules",
        "all",
        "rule.all",
        &format!("{both}start: core\ncore: Every role's line.\n"),
        "ALL-BODY\n",
    );
    put(
        "rules",
        "lead",
        "rule.lead",
        &format!("{both}start: role\ncore:\n- line: The lead's line.\n  roles: [orchestrator]\n"),
        "LEAD-BODY\n",
    );
    put(
        "rules",
        "split",
        "rule.split",
        &format!(
            "{both}start: core\ncore:\n- line: The lead's half.\n  roles: [orchestrator]\n- line: The coder's half.\n  roles: [coder]\n"
        ),
        "SPLIT-BODY\n",
    );
    put("rules", "detail", "rule.detail", &format!("{both}start: detail\n"), "DETAIL-BODY\n");
    put("rules", "legacy", "rule.legacy", both, "LEGACY-BODY\n");
    put(
        "rules",
        "ended",
        "rule.ended",
        &format!(
            "{both}start: core\ncore: The ended line.\ntemporary:\n  until: {PAST}\n  review_by: 2999-12-31\n  reason: a trial\n"
        ),
        "ENDED-BODY\n",
    );
    put("intent", "i", "intent.i", &format!("{both}start: detail\n"), "INTENT-I-BODY\n");
    put("intent", "j", "intent.j", both, "INTENT-J-BODY\n");
    put(
        "templates",
        "t",
        "template.t",
        &format!("{both}start: core\ncore: The template's line.\n"),
        "T-BODY\n",
    );
    put("templates", "u", "template.u", both, "U-BODY\n");
    (f, checkout)
}

/// The opener the composed files name: the installed guard copy of the plan's tool home.
fn opener(f: &Fixture) -> String {
    format!("`{} show <id>`", f.home().join("bin").join("lords-guard.exe").display())
}

// Spec (the brief, slice 2: "It renders, per place and role, the applicable core lines each with its id, then the
// index of the rest"; plan L8: "Detail entries get no line, only their id in the index"; "An entry with no `core:`
// line yet renders its whole body as today"): a role's file carries each line for that role after its entry's id,
// the index entry of each entry with no line for it, and the whole text of an entry with no class; never a body
// that has a line or an index entry, never another role's line, never an ended entry, and the opener once.
#[test]
fn a_role_file_starts_with_its_lines_then_the_index_of_the_rest() {
    let (f, checkout) = place("orchestrator");
    let p = plan(&f.stacks(), &f.opts()).unwrap();
    let coder = planned_in(&p, &checkout.join(".lords"), "role-coder.md");
    for want in [
        "- rule.all: Every role's line.",
        "- rule.split: The coder's half.",
        "- template.t: The template's line.",
        "- rule.lead: rule.lead",
        "- rule.detail: rule.detail",
        "LEGACY-BODY",
        "INTENT-J-BODY",
        &opener(&f),
    ] {
        assert!(coder.contains(want), "{want} in:\n{coder}");
    }
    for never in [
        "ALL-BODY",
        "SPLIT-BODY",
        "LEAD-BODY",
        "DETAIL-BODY",
        "T-BODY",
        "U-BODY",
        "INTENT-I-BODY",
        "The lead's",
        "ended",
    ] {
        assert!(!coder.contains(never), "{never} not in:\n{coder}");
    }
    assert!(coder.contains("intent.i") || coder.contains("i.md"), "intent.i indexed:\n{coder}");
    assert_eq!(coder.matches(" show <id>").count(), 1, "the opener once:\n{coder}");
    let lead = planned_in(&p, &checkout.join(".lords"), "role-orchestrator.md");
    assert!(
        lead.contains("- rule.lead: The lead's line.")
            && lead.contains("- rule.split: The lead's half.")
    );
    assert!(!lead.contains("The coder's half."), "{lead}");
}

// Spec (the brief: "Claude and Codex places get the same content"; design: a place's default role, its entries
// inline and labelled): a place's file carries every line for every development role unlabelled and the default
// role's lines labelled, in both adapters' files alike; another role's line stays in its role file, counted in the
// role-files line. An entry with no class keeps each adapter's behaviour of today.
#[test]
fn both_adapters_place_files_carry_the_same_lines() {
    let (f, checkout) = place("orchestrator");
    let p = plan(&f.stacks(), &f.opts()).unwrap();
    let claude = proposed(&p, &checkout.join("CLAUDE.local.md"));
    let codex = proposed(&p, &checkout.join("AGENTS.override.md"));
    let lines = |text: &str| -> Vec<String> {
        text.lines()
            .filter(|l| l.starts_with("- rule.") || l.starts_with("- template."))
            .map(String::from)
            .collect()
    };
    assert_eq!(lines(&claude), lines(&codex), "the same lines:\n{claude}\n----\n{codex}");
    for want in [
        "- rule.all: Every role's line.",
        "- rule.lead (orchestrator only): The lead's line.",
        "- rule.split (orchestrator only): The lead's half.",
        "- template.t: The template's line.",
    ] {
        assert!(lines(&claude).iter().any(|l| l == want), "{want} in:\n{claude}");
    }
    assert!(
        !claude.contains("The coder's half."),
        "the coder's line is in its role file:\n{claude}"
    );
    let pointer = claude.lines().find(|l| l.contains("has them in full:")).unwrap_or_default();
    assert!(pointer.contains("role-coder.md"), "the coder's file is named: {pointer}");
    // Today's behaviour for the entry with no class: Claude inline, Codex by reference.
    assert!(!codex.contains("LEGACY-BODY") || claude.contains("LEGACY-BODY"));
}

// Spec (plan L6, an orchestrator's condition 3: "Children start with their own role's core, not the parent's"; the brief, slice
// 4; Claude Code's docs: a non-fork subagent loads "every level of the CLAUDE.md hierarchy … `CLAUDE.local.md`"): with
// the stack's `child_role`, each place's `.lords/child.md` names the role a native child starts as and carries what of
// that role the place's file doesn't: its lines for only some roles and its whole entries with no class, not the lines
// every role has (the place's file carries those), and nothing of a role it doesn't bind. Without `child_role`, no file.
#[test]
fn a_native_child_starts_with_its_own_roles_lines_beyond_the_place_file() {
    for child_role in [None, Some("helper")] {
        let f = Fixture::new();
        let checkout = f.path("checkout");
        f.write("checkout/README.md", "a repo\n");
        f.git_init("checkout");
        let child = child_role.map(|r| format!("child_role = \"{r}\"\n")).unwrap_or_default();
        f.write(
            "priv/stack.toml",
            &format!(
                "name = \"priv\"\nvisibility = \"private\"\nroles = [\"orchestrator\", \"coder\", \"helper\", \"product\"]\n{child}[projects.beta]\n[repos.beta-core]\nproject = \"beta\"\ncheckouts = ['{}']\nworktrees = false\ndefault_role = \"orchestrator\"\n",
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
            "help",
            "roles: [helper, coder]\nstart: core\ncore: The helpers' line.\n",
            "HELP-BODY\n",
        );
        put("lead", "roles: [orchestrator]\nstart: core\ncore: The lead's line.\n", "LEAD-BODY\n");
        put("whole", "roles: [helper]\n", "HELPER-BODY\n");
        // Sol-loop r1, S2: a skill's line for the child's role only, which the place's file leaves to its hook; r2, T1:
        // and a rule extending that skill, which the skills index doesn't carry.
        skill(&f, "tool", "roles: [helper]\nstart: core\ncore: The helper's skill line.\n");
        put("extra", "roles: [helper]\nextends: [skill.tool]\n", "EXTRA-BODY\n");
        f.write(
            "priv/project/beta/memory/n.md",
            &entry("memory.n", "project:beta", "private", "roles: [helper]\n", "A NOTE.\n"),
        );
        let p = plan(&f.stacks(), &f.opts()).unwrap();
        let file = planned_in(&p, &checkout.join(".lords"), "child.md");
        let whole = planned_in(&p, &checkout.join(".lords"), "child-whole.md");
        let Some(role) = child_role else {
            assert_eq!((file.as_str(), whole.as_str()), ("", ""), "no child role, no files");
            continue;
        };
        let intro = format!("as a {role}");
        let own = [
            "- rule.help: The helpers' line.",
            "HELPER-BODY",
            "- skill.tool: The helper's skill line.",
            "EXTRA-BODY",
        ];
        for want in own.iter().chain(&[intro.as_str(), "role-helper.md", &opener(&f)]) {
            assert!(file.contains(want), "{want} in:\n{file}");
        }
        for never in
            ["Every role's line.", "The lead's line.", "ALL-BODY", "LEAD-BODY", "HELP-BODY"]
        {
            assert!(!file.contains(never), "{never} not in:\n{file}");
        }
        let place_file = proposed(&p, &checkout.join("CLAUDE.local.md"));
        assert!(place_file.contains("- rule.all: Every role's line."), "{place_file}");
        // Review r1, F1: a child that loads no place file (Claude's Explore and Plan) starts with the whole: the lines
        // every role has as well as its own, and nothing of another role's or a note's.
        for want in
            own.iter().chain(&[intro.as_str(), "- rule.all: Every role's line.", &opener(&f)])
        {
            assert!(whole.contains(want), "{want} in the whole start:\n{whole}");
        }
        for never in ["The lead's line.", "ALL-BODY", "A NOTE.", "# Memory"] {
            assert!(!whole.contains(never), "{never} not in the whole start:\n{whole}");
        }
    }
}

/// A skill `skill.<name>` in project `beta`, with `yaml` added to its metadata.
fn skill(f: &Fixture, name: &str, yaml: &str) {
    f.write(
        &format!("priv/project/beta/skills/{name}/lords.yaml"),
        &format!(
            "id: skill.{name}\nkind: skill\ntitle: The {name}\naudience: [all]\nscope: project:beta\nstatus: active\nvisibility: private\norigin:\n  type: own\n{yaml}"
        ),
    );
    f.write(
        &format!("priv/project/beta/skills/{name}/SKILL.md"),
        &format!("---\nname: {name}\ndescription: The {name}'s own description.\n---\nUse it.\n"),
    );
}

// Spec (the invariant: "nothing that binds it is dropped"; sol-loop r1, S1, its witness): an entry that extends one
// shown only in the index still shows, by its line or its whole text, where it would have followed its parent.
#[test]
fn an_extension_shows_when_its_parent_is_only_in_the_index() {
    let (f, checkout) = place("orchestrator");
    let put = |slug: &str, yaml: &str, body: &str| {
        f.write(
            &format!("priv/project/beta/rules/{slug}.md"),
            &entry(&format!("rule.{slug}"), "project:beta", "private", yaml, body),
        );
    };
    put("base", "start: detail\n", "BASE-BODY\n");
    put("extra", "extends: [rule.base]\nstart: core\ncore: Do X.\n", "EXTRA-BODY\n");
    put("more", "extends: [rule.base]\n", "MORE-BODY\n");
    let p = plan(&f.stacks(), &f.opts()).unwrap();
    let coder = planned_in(&p, &checkout.join(".lords"), "role-coder.md");
    for want in ["- rule.extra: Do X.", "MORE-BODY", "- rule.base: rule.base"] {
        assert!(coder.contains(want), "{want} in:\n{coder}");
    }
    assert!(!coder.contains("BASE-BODY") && !coder.contains("EXTRA-BODY"), "{coder}");
}

// Spec (sol-loop r1, S3, its witness; review r1, F4: "retain the canonical id visibly beside every classified skill
// line"; design: a place's file labels a line for only some roles): a skill's lines each keep their own label in the
// skills index, one bullet each, after the skill's id, which `lords show` opens; a skill whose name isn't its dir's
// keeps its file named too.
#[test]
fn each_skill_line_keeps_its_own_label_and_its_id() {
    let (f, checkout) = place("orchestrator");
    skill(
        &f,
        "tool",
        "start: core\ncore:\n- line: Shared instruction.\n- line: Lead instruction.\n  roles: [orchestrator]\n",
    );
    skill(&f, "audit", "start: core\ncore: Review every draft.\n");
    let audit = f
        .read("priv/project/beta/skills/audit/SKILL.md")
        .replace("name: audit", "name: draft-check");
    f.write("priv/project/beta/skills/audit/SKILL.md", &audit);
    let stacks = f.stacks();
    let p = plan(&stacks, &f.opts()).unwrap();
    let claude = proposed(&p, &checkout.join("CLAUDE.local.md"));
    for want in [
        "- skill.tool: Shared instruction.",
        "- skill.tool (orchestrator only): Lead instruction.",
        "- skill.audit: Review every draft. (`audit\\SKILL.md`)",
    ] {
        let want = if cfg!(windows) { want.to_string() } else { want.replace('\\', "/") };
        assert!(claude.lines().any(|l| l == want), "{want} in:\n{claude}");
    }
    for id in ["skill.tool", "skill.audit"] {
        assert!(lords_core::core_lines::show(&stacks, id).is_ok(), "{id} opens");
    }
}

// Spec (review r1, F2, its witness): a rule extending an intent whose class is `detail` keeps its own line, and one
// with no class its whole text, in a role's file and a place's file; neither is swallowed by the intent's title.
#[test]
fn a_rule_extending_a_detail_intent_keeps_its_own_form() {
    let (f, checkout) = place("coder");
    let put = |kind_dir: &str, slug: &str, id: &str, yaml: &str, body: &str| {
        f.write(
            &format!("priv/project/beta/{kind_dir}/{slug}.md"),
            &entry(id, "project:beta", "private", yaml, body),
        );
    };
    put("intent", "base", "intent.base", "start: detail\n", "BASE-BODY\n");
    put(
        "rules",
        "extra",
        "rule.extra",
        "extends: [intent.base]\nstart: core\ncore: Never publish the draft.\n",
        "EXTRA-BODY\n",
    );
    put("rules", "more", "rule.more", "extends: [intent.base]\n", "MORE-BODY\n");
    let p = plan(&f.stacks(), &f.opts()).unwrap();
    let coder = planned_in(&p, &checkout.join(".lords"), "role-coder.md");
    let claude = proposed(&p, &checkout.join("CLAUDE.local.md"));
    for (name, file) in [("role file", &coder), ("place file", &claude)] {
        for want in ["- rule.extra: Never publish the draft.", "MORE-BODY"] {
            assert!(file.contains(want), "{want} in the {name}:\n{file}");
        }
        assert!(!file.contains("BASE-BODY") && !file.contains("EXTRA-BODY"), "{name}:\n{file}");
    }
}

// Spec (review r1, F3, its witness): a global skill naming no roles (every role) with a core line has that line, by
// id, in the user-level file every run reads, and in a role's file, which stands alone.
#[test]
fn a_role_free_global_skills_line_reaches_the_user_level_file() {
    let (f, checkout) = place("coder");
    f.write(
        "pub/global/skills/audit/lords.yaml",
        "id: skill.audit\nkind: skill\ntitle: Review before publication\naudience: [all]\nscope: global\nstatus: active\nvisibility: public\norigin:\n  type: own\nstart: core\ncore: Review every draft before publication.\n",
    );
    f.write(
        "pub/global/skills/audit/SKILL.md",
        "---\nname: audit\ndescription: Audit drafts.\n---\nAudit.\n",
    );
    let stacks = f.stacks();
    let line = "- skill.audit: Review every draft before publication.";
    for adapter in ["claude", "codex"] {
        let user =
            lords_core::sync::user_composed(&stacks, lords_core::adapters::get(adapter).unwrap())
                .text;
        assert!(user.contains(line), "{adapter}'s user-level file:\n{user}");
    }
    let p = plan(&stacks, &f.opts()).unwrap();
    let coder = planned_in(&p, &checkout.join(".lords"), "role-coder.md");
    assert!(coder.contains(line), "{coder}");
}

// Spec (the Lords owner, 31 Oct: "a per-adapter setting in the stack gives Codex place files no default-role
// (orchestrator-only) lines; `check` reports a Codex place file over a byte budget the stack sets"): with
// `[place_files.codex] default_role_lines = false`, Codex's place file leaves the default role's lines for only some
// roles to its role file (counted there), while Claude's keeps them; the stack's `warn_bytes` is the warning line for
// that adapter's place files in `check`, and without it the 75 % line stands.
#[test]
fn a_stack_can_leave_the_default_roles_lines_out_of_codex_place_files() {
    let (f, checkout) = place("orchestrator");
    let manifest = f.read("priv/stack.toml");
    f.write(
        "priv/stack.toml",
        &format!("{manifest}[place_files.codex]\ndefault_role_lines = false\n"),
    );
    let p = plan(&f.stacks(), &f.opts()).unwrap();
    let claude = proposed(&p, &checkout.join("CLAUDE.local.md"));
    let codex = proposed(&p, &checkout.join("AGENTS.override.md"));
    let lead = "- rule.lead (orchestrator only): The lead's line.";
    assert!(claude.contains(lead), "{claude}");
    assert!(!codex.contains(lead) && !codex.contains("The lead's half."), "{codex}");
    assert!(codex.contains("- rule.all: Every role's line."), "{codex}");
    let pointer = codex.lines().find(|l| l.contains("has them in full:")).unwrap_or_default();
    assert!(pointer.contains("role-orchestrator.md"), "the lead's file is named: {pointer}");
    // What `lords check` reports of the places' files (main.rs: `sync::drift_findings`).
    let size_warnings = |f: &Fixture| -> Vec<String> {
        lords_core::sync::drift_findings(&f.stacks(), &f.home())
            .into_iter()
            .filter(|x| x.code == "size" && x.severity == lords_core::Severity::Warning)
            .map(|x| x.message)
            .collect()
    };
    assert_eq!(size_warnings(&f), Vec::<String>::new(), "a small file is under the 75 % line");
    f.write(
        "priv/stack.toml",
        &format!("{manifest}[place_files.codex]\ndefault_role_lines = false\nwarn_bytes = 200\n"),
    );
    let warned = size_warnings(&f);
    assert!(
        warned.len() == 1 && warned[0].contains("AGENTS.override.md") && warned[0].contains("200"),
        "past the stack's budget for Codex only: {warned:?}"
    );
}

/// A repo `beta-core` whose default role is the orchestrator, with roles orchestrator, coder, helper and product,
/// `extra` added to its stack.toml, and no entries.
fn bare_place(extra: &str) -> (Fixture, PathBuf) {
    let f = Fixture::new();
    let checkout = f.path("checkout");
    f.write("checkout/README.md", "a repo\n");
    f.git_init("checkout");
    f.write(
        "priv/stack.toml",
        &format!(
            "name = \"priv\"\nvisibility = \"private\"\nroles = [\"orchestrator\", \"coder\", \"helper\", \"product\"]\n{extra}[projects.beta]\n[repos.beta-core]\nproject = \"beta\"\ncheckouts = ['{}']\nworktrees = false\ndefault_role = \"orchestrator\"\n",
            checkout.display()
        ),
    );
    (f, checkout)
}

// Spec (repair sol-loop r1, R1, its witness; design: the role-files line counts what each role's file carries): a
// skill line a place's file leaves to the role file (it shows the skill's hook) is counted for that role, so the
// role file is named even when it is the only such line.
#[test]
fn a_skill_line_left_to_a_role_file_is_counted_there() {
    let (f, checkout) = bare_place("");
    let manifest = f.read("priv/stack.toml");
    f.write(
        "priv/stack.toml",
        &format!("{manifest}[place_files.codex]\ndefault_role_lines = false\n"),
    );
    skill(&f, "tool", "roles: [orchestrator]\nstart: core\ncore: The lead's skill line.\n");
    let p = plan(&f.stacks(), &f.opts()).unwrap();
    let codex = proposed(&p, &checkout.join("AGENTS.override.md"));
    assert!(!codex.contains("The lead's skill line."), "{codex}");
    let pointer = codex.lines().find(|l| l.contains("has them in full:")).unwrap_or_default();
    assert!(
        pointer.contains("role-orchestrator.md` (1"),
        "the lead's file, its one entry: {codex}"
    );
}

// Spec (repair sol-loop r1, R2, its witness; the invariant: nothing that binds a child is dropped): a child's whole
// start that needs more parts than Claude Code's child hooks deliver is refused like a file past its harness's limit:
// the role files aren't written, and the plan says why.
#[test]
fn a_child_start_past_the_parts_delivered_is_refused() {
    let (f, checkout) = bare_place("child_role = \"helper\"\n");
    let lines: Vec<String> =
        (0..80).map(|i| format!("- line: \"{i:02} {}\"", "x".repeat(400))).collect();
    f.write(
        "priv/project/beta/rules/long.md",
        &entry(
            "rule.long",
            "project:beta",
            "private",
            &format!("start: core\ncore:\n{}\n", lines.join("\n")),
            "Long.\n",
        ),
    );
    let p = plan(&f.stacks(), &f.opts()).unwrap();
    let item =
        p.items.iter().find(|i| i.path == checkout.join(".lords")).expect("the role files' item");
    assert_eq!(item.action, lords_core::sync::Action::Conflict, "{:?}", item.notes);
    assert!(item.notes.iter().any(|n| n.contains("parts")), "{:?}", item.notes);
    assert!(p.over_limit, "the plan fails like an over-limit file");
}

// Spec (repair sol-loop r1, R5, its witness; the Lords owner: "the limit itself stays a refusal"): a stack budget
// above the adapter's limit doesn't hide `check`'s error for a file past the limit.
#[test]
fn a_budget_above_the_limit_keeps_the_limits_error() {
    let (f, _) = bare_place("[place_files.codex]\nwarn_bytes = 100000\n");
    let body = "Each line of this rule is long enough to count. ".repeat(900);
    f.write(
        "priv/project/beta/rules/big.md",
        &entry("rule.big", "project:beta", "private", "", &format!("{body}\n")),
    );
    let errors: Vec<String> = lords_core::sync::drift_findings(&f.stacks(), &f.home())
        .into_iter()
        .filter(|x| x.code == "size" && x.severity == lords_core::Severity::Error)
        .map(|x| x.message)
        .collect();
    assert!(errors.iter().any(|m| m.contains("AGENTS.override.md")), "{errors:?}");
}

// Spec (repair sol-loop r1, R6, its witness): an intent whose class is `detail` extending one shown by its whole text
// keeps its title in the intent index; neither its body nor the parent's is lost or doubled.
#[test]
fn a_detail_intent_extending_a_whole_intent_keeps_its_title() {
    let (f, checkout) = bare_place("");
    let put = |slug: &str, yaml: &str, body: &str| {
        f.write(
            &format!("priv/project/beta/intent/{slug}.md"),
            &entry(&format!("intent.{slug}"), "project:beta", "private", yaml, body),
        );
    };
    put("base", "", "BASE-BODY\n");
    put("extra", "start: detail\nextends: [intent.base]\n", "EXTRA-BODY\n");
    let p = plan(&f.stacks(), &f.opts()).unwrap();
    let helper = planned_in(&p, &checkout.join(".lords"), "role-helper.md");
    assert!(helper.contains("BASE-BODY") && helper.contains("intent.extra"), "{helper}");
    assert!(!helper.contains("EXTRA-BODY"), "{helper}");
}

// Spec (the invariant: "nothing private reaches a public file"; design: Privacy): a composition of public entries
// only (what sync reports for a tracked AGENTS.md) carries no private entry's line and names its opener without a
// user path, so the privacy scan a public repo runs finds nothing in it.
#[test]
fn a_public_composition_carries_no_private_line_and_no_user_path() {
    let (f, checkout) = place("coder");
    f.write(
        "pub/global/rules/open.md",
        &entry(
            "rule.open",
            "global",
            "public",
            "start: core\ncore: A public line.\n",
            "OPEN-BODY\n",
        ),
    );
    f.write("checkout/AGENTS.md", "# The repo's own\n");
    git(&checkout, &["add", "AGENTS.md"]);
    git(&checkout, &["commit", "-q", "-m", "tracked"]);
    let p = plan(&f.stacks(), &f.opts()).unwrap();
    let tracked = proposed(&p, &checkout.join("AGENTS.md"));
    assert!(tracked.contains("- rule.open: A public line."), "{tracked}");
    assert!(!tracked.contains("Every role's line.") && !tracked.contains("lead"), "{tracked}");
    let hits = lords_core::privacy::scan(&tracked, &lords_core::privacy::Rules::public());
    assert!(hits.is_empty(), "{hits:?} in:\n{tracked}");
}
