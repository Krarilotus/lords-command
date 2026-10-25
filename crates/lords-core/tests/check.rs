//! `check`, against docs/design.md (sections named in each spec line).

mod common;
use common::*;
use lords_core::Severity;

// Spec (Layers): a narrower layer replaces a broader entry by naming it; that is valid, and `check` lists it.
#[test]
fn a_named_override_is_valid_and_listed() {
    let f = Fixture::new();
    f.write(
        "pub/global/rules/a.md",
        &entry("rule.a", "global", "public", "", "Use two helpers.\n"),
    );
    f.write(
        "priv/project/beta/rules/b.md",
        &entry("rule.b", "project:beta", "private", "overrides: [rule.a]\n", "Use three.\n"),
    );
    let found = f.check();
    assert_eq!(errors(&found), Vec::<&str>::new(), "{found:#?}");
    assert!(has(&found, "override", "rule.b"));
}

// Spec (Layers): two entries with the same id are an error, in any layers or stacks.
#[test]
fn an_id_defined_twice_is_an_error() {
    let f = Fixture::new();
    f.write("pub/global/rules/a.md", &entry("rule.a", "global", "public", "", "x\n"));
    f.write("priv/global/rules/a.md", &entry("rule.a", "global", "private", "", "y\n"));
    assert_eq!(errors(&f.check()), ["id-duplicate", "id-duplicate"]);
}

// Spec (On-disk layout): the kind dir, the layer dir and the stack must agree with kind, scope and visibility.
#[test]
fn metadata_that_disagrees_with_its_place_is_an_error() {
    let cases = [
        ("pub/global/facts/a.md", entry("rule.a", "global", "public", "", "x\n")),
        ("pub/global/rules/a.md", entry("rule.a", "project:alpha", "public", "", "x\n")),
        ("pub/global/rules/a.md", entry("rule.a", "global", "private", "", "x\n")),
        ("pub/repo/alpha-core/rules/a.md", entry("rule.a", "path:other:src", "public", "", "x\n")),
    ];
    for (path, text) in cases {
        let f = Fixture::new();
        f.write(path, &text);
        assert_eq!(errors(&f.check()), ["placement"], "{path}: {text}");
    }
    let f = Fixture::new();
    f.write(
        "pub/repo/alpha-core/rules/a.md",
        &entry("rule.a", "path:alpha-core:src/x", "public", "", "x\n"),
    );
    assert_eq!(errors(&f.check()), Vec::<&str>::new(), "a path scope lives in its repo's dir");
}

// Spec (Layers): overriding a narrower entry is an error; so is naming an entry that doesn't exist, or one that
// never applies together with this one.
#[test]
fn bad_overrides_are_errors() {
    let f = Fixture::new();
    f.write(
        "priv/project/beta/rules/narrow.md",
        &entry("rule.narrow", "project:beta", "private", "", "x\n"),
    );
    f.write(
        "priv/global/rules/broad.md",
        &entry("rule.broad", "global", "private", "overrides: [rule.narrow]\n", "y\n"),
    );
    f.write(
        "priv/global/rules/ghost.md",
        &entry("rule.ghost", "global", "private", "extends: [rule.none]\n", "z\n"),
    );
    f.write(
        "pub/project/alpha/rules/other.md",
        &entry("rule.other", "project:alpha", "public", "", "w\n"),
    );
    f.write(
        "priv/project/beta/rules/cross.md",
        &entry("rule.cross", "project:beta", "private", "overrides: [rule.other]\n", "v\n"),
    );
    let found = f.check();
    assert!(has(&found, "override-narrower", "rule.broad"));
    assert!(has(&found, "dangling", "rule.ghost"));
    assert!(has(&found, "override-unrelated", "rule.cross"));
    assert_eq!(errors(&found).len(), 3, "{found:#?}");
}

// Spec (Two stacks): the public stack never refers to a private id.
#[test]
fn a_public_entry_naming_a_private_one_is_an_error() {
    let f = Fixture::new();
    f.write("priv/global/rules/p.md", &entry("rule.p", "global", "private", "", "x\n"));
    f.write(
        "pub/global/rules/q.md",
        &entry("rule.q", "global", "public", "links: [rule.p]\n", "y\n"),
    );
    assert_eq!(errors(&f.check()), ["public-to-private"]);
    f.write(
        "pub/global/rules/q.md",
        &entry("rule.q", "global", "public", "extends: [rule.p]\n", "y\n"),
    );
    let found = f.check();
    assert!(errors(&found).contains(&"public-to-private"), "{found:#?}");
}

// Spec (Entry metadata, origin): adapted or inherited needs repo, commit, path and licence.
#[test]
fn an_adapted_entry_without_its_licence_is_an_error() {
    let f = Fixture::new();
    let text = "---\nid: rule.a\nkind: rule\ntitle: a\naudience: [all]\nscope: global\nstatus: active\nvisibility: public\norigin:\n  type: adapted\n  repo: someone/skills\n  commit: aeac41d\n  path: x.md\n---\nbody\n";
    f.write("pub/global/rules/a.md", text);
    assert!(errors(&f.check()).contains(&"schema"));
    f.write(
        "pub/global/rules/a.md",
        &text.replace("  path: x.md\n", "  path: x.md\n  licence: MIT\n"),
    );
    assert_eq!(errors(&f.check()), Vec::<&str>::new());
}

// Spec (Check): an inherited entry stays verbatim; its files' hash must match the recorded one.
#[test]
fn an_inherited_skill_that_changed_is_an_error() {
    let f = Fixture::new();
    f.write("pub/global/skills/s/SKILL.md", "---\nname: s\n---\nDo the thing.\n");
    f.write("pub/global/skills/s/LICENSE.txt", "terms\n");
    let hash = lords_core::check::path_hash(&f.path("pub/global/skills/s")).unwrap();
    let sidecar = format!(
        "id: skill.s\nkind: skill\ntitle: s\naudience: [all]\nscope: global\nstatus: active\nvisibility: public\norigin:\n  type: inherited\n  repo: someone/skills\n  commit: 0123abc\n  path: skills/s\n  licence: LICENSE.txt\n  sha256: {hash}\n"
    );
    f.write("pub/global/skills/s/lords.yaml", &sidecar);
    assert_eq!(errors(&f.check()), Vec::<&str>::new());
    // CRLF line endings (a Windows checkout) are the same text.
    f.write("pub/global/skills/s/SKILL.md", "---\r\nname: s\r\n---\r\nDo the thing.\r\n");
    assert_eq!(errors(&f.check()), Vec::<&str>::new());
    f.write("pub/global/skills/s/SKILL.md", "---\nname: s\n---\nDo the thing better.\n");
    assert_eq!(errors(&f.check()), ["inherited-changed"]);
}

// Spec (Check): the same sentence in two unrelated entries is a candidate (give it one home); an entry that
// names the other (override, extension) is not a restatement.
#[test]
fn a_restated_sentence_is_a_candidate_unless_one_names_the_other() {
    let s = "Commits and pull requests carry no attribution line naming a model.\n";
    let f = Fixture::new();
    f.write("pub/global/rules/a.md", &entry("rule.a", "global", "public", "", s));
    f.write("priv/global/rules/b.md", &entry("rule.b", "global", "private", "", s));
    assert_eq!(codes(&f.check(), Severity::Candidate), ["duplicate-text"]);
    f.write(
        "priv/global/rules/b.md",
        &entry("rule.b", "global", "private", "overrides: [rule.a]\n", s),
    );
    assert_eq!(codes(&f.check(), Severity::Candidate), Vec::<&str>::new());
}

// Spec (Layers, unnamed conflict): entries sharing a topic with different numbers for the same quantity, where
// neither names the other, are a candidate; other topics, or a named override, are not.
#[test]
fn conflicting_numbers_on_one_topic_are_a_candidate() {
    let f = Fixture::new();
    f.write(
        "pub/global/rules/a.md",
        &entry(
            "rule.a",
            "global",
            "public",
            "topics: [delegation]\n",
            "Start at most 2 helpers.\n",
        ),
    );
    f.write(
        "priv/global/rules/b.md",
        &entry(
            "rule.b",
            "global",
            "private",
            "topics: [delegation]\n",
            "Up to 6 helpers may run.\n",
        ),
    );
    let found = f.check();
    assert_eq!(codes(&found, Severity::Candidate), ["conflict"]);
    assert_eq!(errors(&found), Vec::<&str>::new(), "a candidate is for the owner, never an error");

    f.write(
        "priv/global/rules/b.md",
        &entry("rule.b", "global", "private", "topics: [timing]\n", "Up to 6 helpers may run.\n"),
    );
    assert_eq!(codes(&f.check(), Severity::Candidate), Vec::<&str>::new());
    f.write(
        "priv/global/rules/b.md",
        &entry(
            "rule.b",
            "global",
            "private",
            "topics: [delegation]\noverrides: [rule.a]\n",
            "Up to 6 helpers.\n",
        ),
    );
    assert_eq!(codes(&f.check(), Severity::Candidate), Vec::<&str>::new());
}

// Spec (Layers): entries that never apply together (different projects) don't conflict.
#[test]
fn entries_in_different_projects_dont_conflict() {
    let f = Fixture::new();
    f.write(
        "pub/project/alpha/rules/a.md",
        &entry("rule.a", "project:alpha", "public", "topics: [t]\n", "Always rebase.\n"),
    );
    f.write(
        "priv/project/beta/rules/b.md",
        &entry("rule.b", "project:beta", "private", "topics: [t]\n", "Never rebase.\n"),
    );
    assert_eq!(codes(&f.check(), Severity::Candidate), Vec::<&str>::new());
    f.write(
        "priv/project/beta/rules/b.md",
        &entry("rule.b", "project:alpha", "private", "topics: [t]\n", "Never rebase.\n"),
    );
    f.write("priv/stack.toml", "name = \"priv\"\nvisibility = \"private\"\n");
    std::fs::rename(f.path("priv/project/beta"), f.path("priv/project/alpha")).unwrap();
    assert_eq!(codes(&f.check(), Severity::Candidate), ["conflict"]);
}

// Spec (Entry metadata): retired needs the date and reason; an active entry pointing at a retired one is flagged.
#[test]
fn retirement_needs_its_record_and_links_to_it_are_flagged() {
    let f = Fixture::new();
    f.write(
        "pub/global/rules/old.md",
        &entry("rule.old", "global", "public", "", "x\n")
            .replace("status: active", "status: retired"),
    );
    assert!(errors(&f.check()).contains(&"schema"));
    let retired = entry(
        "rule.old",
        "global",
        "public",
        "retired:\n  date: 2026-10-25\n  reason: replaced\n",
        "x\n",
    );
    f.write("pub/global/rules/old.md", &retired.replace("status: active", "status: retired"));
    f.write(
        "pub/global/rules/new.md",
        &entry("rule.new", "global", "public", "links: [rule.old]\n", "y\n"),
    );
    let found = f.check();
    assert_eq!(errors(&found), Vec::<&str>::new());
    assert!(has(&found, "links-retired", "rule.new"));
}

// Spec (Check): links and paths must resolve.
#[test]
fn a_dangling_link_is_an_error() {
    let f = Fixture::new();
    f.write("pub/global/rules/there.txt", "");
    f.write(
        "pub/global/rules/a.md",
        &entry("rule.a", "global", "public", "", "See [it](there.txt) and [web](https://x.org).\n"),
    );
    assert_eq!(errors(&f.check()), Vec::<&str>::new());
    f.write(
        "pub/global/rules/a.md",
        &entry("rule.a", "global", "public", "", "line\nSee [it](gone.md#part).\n"),
    );
    let found = f.check();
    assert_eq!(errors(&found), ["dangling"]);
    let d = found.iter().find(|x| x.code == "dangling").unwrap();
    // 11 frontmatter lines, then "line", then the link.
    assert_eq!(d.line, Some(13), "the line in the file, frontmatter included");
}

// Spec (Privacy): the public stack gets every class; the private stack only the secret class.
#[test]
fn privacy_findings_depend_on_the_stack() {
    let f = Fixture::new();
    let detail = format!("Notes live in {}.\n", user_path("alice"));
    f.write("priv/global/facts/a.md", &entry("fact.a", "global", "private", "", &detail));
    assert_eq!(errors(&f.check()), Vec::<&str>::new());
    f.write(
        "pub/global/facts/b.md",
        &entry("fact.b", "global", "public", "", &detail.replace("Notes", "Logs")),
    );
    assert_eq!(errors(&f.check()), ["privacy"]);
    let token = format!("token = \"{}\"\n", "ab12".repeat(6));
    f.write("priv/global/facts/c.md", &entry("fact.c", "global", "private", "", &token));
    assert!(has(&f.check(), "privacy", "fact.c"));
}

// Spec (On-disk layout; design: ids never change meaning): a memory store keeps files verbatim; its defaults
// supply shared fields, the file name gives the id (an agent may reword `name`), and an index (MEMORY.md) is not
// an entry.
#[test]
fn a_memory_store_derives_ids_and_skips_its_index() {
    let f = Fixture::new();
    f.write(
        "priv/global/memory/agent/lords.defaults.yaml",
        "kind: memory\nscope: global\naudience: [opus]\nstatus: active\nvisibility: private\norigin:\n  type: imported\n  store: agent\n",
    );
    f.write("priv/global/memory/agent/MEMORY.md", "- [x](x.md)\n");
    f.write("priv/global/memory/agent/x.md", "---\nname: Prompts in chat\ndescription: paste prompts\nmetadata:\n  type: feedback\n---\nbody\n");
    f.write("priv/global/memory/agent/plain.md", "no frontmatter at all\n");
    let s = f.stacks();
    assert_eq!(errors(&f.check()), Vec::<&str>::new(), "{:#?}", f.check());
    let ids: Vec<&str> = s.entries().map(|e| e.id()).collect();
    assert_eq!(ids, ["memory.agent.plain", "memory.agent.x"]);
    let x = s.find("memory.agent.x").unwrap();
    assert_eq!(x.meta.title, "paste prompts");
    assert_eq!(
        x.meta.origin,
        lords_core::model::Origin::Imported { store: "agent".into(), file: "x.md".into() }
    );
}

// Spec (On-disk layout): a skill is a dir with SKILL.md and a lords.yaml sidecar; its id is skill.<dir>.
#[test]
fn a_skill_needs_its_sidecar_and_matching_id() {
    let f = Fixture::new();
    f.write("pub/global/skills/s/SKILL.md", "---\nname: s\n---\nx\n");
    assert_eq!(errors(&f.check()), ["schema"]);
    let side = "id: skill.t\nkind: skill\ntitle: s\naudience: [all]\nscope: global\nstatus: active\nvisibility: public\norigin:\n  type: own\n";
    f.write("pub/global/skills/s/lords.yaml", side);
    assert_eq!(errors(&f.check()), ["layout"]);
    f.write("pub/global/skills/s/lords.yaml", &side.replace("skill.t", "skill.s"));
    assert_eq!(errors(&f.check()), Vec::<&str>::new());
}

// Spec (design: Entry metadata, topics; owner, 25 Oct: "topics as a controlled list in stack.toml"): an entry
// may tag only declared topics; a public entry only the public stack's, so the public stack stands alone.
#[test]
fn topics_come_from_a_controlled_list() {
    let f = Fixture::new();
    f.write(
        "pub/global/rules/a.md",
        &entry("rule.a", "global", "public", "topics: [timing]\n", "x\n"),
    );
    f.write(
        "priv/global/rules/b.md",
        &entry("rule.b", "global", "private", "topics: [x, timing]\n", "y\n"),
    );
    assert_eq!(errors(&f.check()), Vec::<&str>::new());
    f.write("pub/global/rules/a.md", &entry("rule.a", "global", "public", "topics: [x]\n", "x\n"));
    assert!(has(&f.check(), "topic-undeclared", "rule.a"), "x is declared only privately");
    f.write(
        "pub/global/rules/a.md",
        &entry("rule.a", "global", "public", "topics: [typo]\n", "x\n"),
    );
    assert!(has(&f.check(), "topic-undeclared", "rule.a"));
}

// Spec (orchestrator, 25 Oct: "roles from the private stack's roles list"): an entry names only declared roles;
// a public entry only the public stack's.
#[test]
fn roles_come_from_a_controlled_list() {
    let f = Fixture::new();
    f.write(
        "priv/stack.toml",
        "name = \"priv\"\nvisibility = \"private\"\nroles = [\"orchestrator\"]\n",
    );
    f.write(
        "priv/global/rules/a.md",
        &entry("rule.a", "global", "private", "roles: [orchestrator]\n", "x\n"),
    );
    assert_eq!(errors(&f.check()), Vec::<&str>::new());
    f.write(
        "pub/global/rules/b.md",
        &entry("rule.b", "global", "public", "roles: [orchestrator]\n", "y\n"),
    );
    assert!(has(&f.check(), "role-undeclared", "rule.b"), "declared only privately");
    f.write(
        "priv/global/rules/a.md",
        &entry("rule.a", "global", "private", "roles: [admiral]\n", "x\n"),
    );
    assert!(has(&f.check(), "role-undeclared", "rule.a"));
}

// Spec (design: Memory, "Agents keep their own memory dirs as the write path"): a note whose frontmatter isn't
// strict YAML (an unquoted colon, which the agent's harness reads fine) still loads from a store, line by line;
// outside a store such frontmatter stays an error.
#[test]
fn an_agents_loose_frontmatter_still_loads_in_a_store() {
    let f = Fixture::new();
    f.write(
        "priv/global/memory/agent/lords.defaults.yaml",
        "kind: memory\nscope: global\naudience: [all]\nstatus: active\nvisibility: private\norigin:\n  type: imported\n  store: agent\n",
    );
    let loose = "---\nname: facts\ndescription: Facts: what we know vs beliefs\n---\nbody\n";
    f.write("priv/global/memory/agent/facts.md", loose);
    assert_eq!(errors(&f.check()), Vec::<&str>::new(), "{:#?}", f.check());
    assert_eq!(
        f.stacks().find("memory.agent.facts").unwrap().meta.title,
        "Facts: what we know vs beliefs"
    );
    f.write("priv/global/rules/r.md", &loose.replace("name: facts", "id: rule.r\nkind: rule"));
    assert!(errors(&f.check()).contains(&"schema"));
}
