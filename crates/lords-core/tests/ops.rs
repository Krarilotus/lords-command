//! `retire`, `import` and `promote`, against docs/design.md (sections named in each spec line).

mod common;
use common::*;
use lords_core::ops::{import, promote, retire, Promote, Retire};
use lords_core::privacy::Rules;
use lords_core::Stack;

fn retire_args<'a>(id: &'a str, reason: &'a str, by: Option<&'a str>) -> Retire<'a> {
    Retire { id, reason, superseded_by: by, date: "2026-10-25".into(), commit: true }
}

// Spec (Entry metadata; brief: "move an entry to retired, with superseded_by. Nothing is silently deleted"):
// retire records status, successor, date and reason, keeps the file and every other line, and commits.
#[test]
fn retire_records_the_retirement_and_keeps_the_entry() {
    let f = Fixture::new();
    let old = entry(
        "rule.old",
        "global",
        "private",
        "# owner's words below\ntopics: [x]\n",
        "Old text.\n",
    );
    f.write("priv/global/rules/old.md", &old);
    f.write("priv/global/rules/new.md", &entry("rule.new", "global", "private", "", "New text.\n"));
    f.git_init("priv");
    let r = retire(&f.stacks(), &retire_args("rule.old", "said better", Some("rule.new"))).unwrap();
    assert!(r.commit.is_some());
    let now = f.read("priv/global/rules/old.md");
    let want = old.replace("status: active\n", "").replace(
        "topics: [x]\n",
        "topics: [x]\nstatus: retired\nsuperseded_by: rule.new\nretired:\n  date: 2026-10-25\n  reason: \"said better\"\n",
    );
    assert_eq!(now, want);
    assert!(!f.stacks().find("rule.old").unwrap().active());
    assert_eq!(
        git(&f.priv_root(), &["log", "-1", "--format=%s"]).trim(),
        "Retire rule.old: said better"
    );
    assert_eq!(
        git(&f.priv_root(), &["status", "--porcelain"]),
        "",
        "everything it changed is committed"
    );
}

// Spec (retire): it refuses what would leave a wrong record: no reason, an unknown or retired successor, a
// private successor for a public entry, or an entry that is already retired.
#[test]
fn retire_refuses_bad_requests() {
    let f = Fixture::new();
    f.write("pub/global/rules/a.md", &entry("rule.a", "global", "public", "", "x\n"));
    f.write("priv/global/rules/p.md", &entry("rule.p", "global", "private", "", "y\n"));
    let s = f.stacks();
    for (args, why) in [
        (retire_args("rule.a", " ", None), "empty reason"),
        (retire_args("rule.a", "r", Some("rule.none")), "unknown successor"),
        (retire_args("rule.a", "r", Some("rule.p")), "private successor"),
        (retire_args("rule.a", "r", Some("rule.a")), "itself"),
        (retire_args("rule.none", "r", None), "unknown entry"),
    ] {
        assert!(retire(&s, &args).is_err(), "{why}");
    }
    let mut once = retire_args("rule.a", "r", None);
    once.commit = false;
    retire(&s, &once).unwrap();
    assert!(retire(&f.stacks(), &once).is_err(), "already retired");
    assert_eq!(f.read("pub/global/rules/a.md").matches("status:").count(), 1);
}

fn store_fixture() -> (Fixture, std::path::PathBuf) {
    let f = Fixture::new();
    let src = f.path("agent-memory");
    std::fs::create_dir_all(&src).unwrap();
    let manifest = format!(
        "name = \"priv\"\nvisibility = \"private\"\n[[memory_stores]]\nname = \"agent\"\npath = '{}'\nscope = \"global\"\naudience = [\"opus\"]\n",
        src.display()
    );
    f.write("priv/stack.toml", &manifest);
    f.git_init("priv");
    (f, src)
}

// Spec (Import and memory): import copies what agents wrote into the private stack and commits it; unchanged
// files make no commit; a file gone from the agent's dir stays in the stack and is reported.
#[test]
fn import_gives_memory_history() {
    let (f, src) = store_fixture();
    std::fs::write(src.join("a.md"), "---\nname: a\ndescription: first\n---\nA\n").unwrap();
    std::fs::write(src.join("MEMORY.md"), "- [a](a.md)\n").unwrap();
    std::fs::write(src.join("notes.sqlite"), [0u8, 1, 2]).unwrap();
    let load = || Stack::load(&f.priv_root()).unwrap();

    let r = &import(&load(), None, true).unwrap()[0];
    assert_eq!(
        (r.added.clone(), r.changed.len()),
        (vec!["MEMORY.md".to_string(), "a.md".to_string()], 0)
    );
    assert!(r.commit.is_some());
    assert_eq!(
        f.read("priv/global/memory/agent/a.md"),
        "---\nname: a\ndescription: first\n---\nA\n",
        "verbatim"
    );
    assert!(!f.path("priv/global/memory/agent/notes.sqlite").exists(), "only Markdown memories");
    assert_eq!(errors(&f.check()), Vec::<&str>::new(), "{:#?}", f.check());

    let r = &import(&load(), None, true).unwrap()[0];
    assert_eq!((r.unchanged, r.commit.clone()), (2, None));

    std::fs::write(src.join("a.md"), "---\nname: a\ndescription: second\n---\nA2\n").unwrap();
    std::fs::remove_file(src.join("MEMORY.md")).unwrap();
    let r = &import(&load(), None, true).unwrap()[0];
    assert_eq!(r.changed, ["a.md"]);
    assert_eq!(r.absent_in_source, ["MEMORY.md"]);
    assert!(f.path("priv/global/memory/agent/MEMORY.md").exists());
    assert_eq!(
        git(&f.priv_root(), &["rev-list", "--count", "HEAD"]).trim(),
        "3",
        "fixture + 2 imports"
    );
}

// Spec (Import and memory, retire): a retired memory stays retired when the agent later edits its copy.
#[test]
fn import_keeps_a_retirement_when_the_agent_edits_the_file() {
    let (f, src) = store_fixture();
    std::fs::write(src.join("a.md"), "---\nname: a\n---\nA\n").unwrap();
    import(&Stack::load(&f.priv_root()).unwrap(), None, true).unwrap();
    let stacks = lords_core::Stacks::load(&f.pub_root(), Some(&f.priv_root())).unwrap();
    retire(&stacks, &retire_args("memory.agent.a", "now a rule", None)).unwrap();
    // Every key the stack adds is kept, the newer fields too (roles, evidence).
    let copy = f.path("priv/global/memory/agent/a.md");
    let text = std::fs::read_to_string(&copy).unwrap();
    std::fs::write(
        &copy,
        text.replacen(
            "---\n",
            "---\nroles: [orchestrator]\nevidence:\n- summary: why\n  date: 2026-10-25\n",
            1,
        ),
    )
    .unwrap();
    let unchanged =
        &lords_core::ops::import_preview(&Stack::load(&f.priv_root()).unwrap(), None).unwrap()[0];
    assert!(
        unchanged.changed.is_empty(),
        "the stack's own keys aren't a change: {:?}",
        unchanged.changed
    );
    std::fs::write(src.join("a.md"), "---\nname: a\n---\nA, edited\n").unwrap();
    import(&Stack::load(&f.priv_root()).unwrap(), None, true).unwrap();
    let s = f.stacks();
    let a = s.find("memory.agent.a").unwrap();
    assert!(!a.active());
    assert_eq!(a.body, "A, edited\n");
    assert_eq!(a.meta.roles, ["orchestrator"]);
    assert_eq!(a.meta.evidence[0].summary, "why");
}

fn promote_fixture(body: &str) -> Fixture {
    let f = Fixture::new();
    f.write("priv/global/rules/r.md", &entry("rule.r", "global", "private", "", body));
    f.git_init("priv");
    f.git_init("pub");
    f
}

// Spec (Privacy, promotion): promotion refuses while a finding is left, and changes nothing.
#[test]
fn promote_refuses_private_details() {
    let f = promote_fixture(&format!("Keep logs.\nThey live in {}.\n", user_path("alice")));
    let err = promote(&f.stacks(), "rule.r", Promote::Clean, &Rules::public(), true).err().unwrap();
    assert!(err.contains("path"), "{err}");
    assert!(f.path("priv/global/rules/r.md").exists());
    assert!(!f.path("pub/global/rules/r.md").exists());
}

// Spec (Privacy, promotion): split moves the lines with findings into a private entry that extends the public
// one; the public copy is clean, both stacks check clean, and each stack commits its side.
#[test]
fn promote_split_keeps_the_detail_private() {
    let detail = format!("They live in {}.\n", user_path("alice"));
    let f = promote_fixture(&format!("Keep logs.\n{detail}Rotate weekly.\n"));
    let r = promote(&f.stacks(), "rule.r", Promote::Split, &Rules::public(), true).unwrap();
    assert_eq!(r.commits.len(), 2);
    let public = f.read("pub/global/rules/r.md");
    assert!(
        public.contains("visibility: public") && public.ends_with("Keep logs.\nRotate weekly.\n"),
        "{public}"
    );
    let s = f.stacks();
    let part = s.find("rule.r.private").unwrap();
    assert_eq!(part.meta.extends, ["rule.r"]);
    assert_eq!(part.body, detail);
    assert!(!f.path("priv/global/rules/r.md").exists());
    assert_eq!(errors(&f.check()), Vec::<&str>::new(), "{:#?}", f.check());
    assert_eq!(git(&f.pub_root(), &["status", "--porcelain"]), "");
    assert_eq!(git(&f.priv_root(), &["status", "--porcelain"]), "");
}

// Spec (Privacy, promotion): strip replaces each finding with a placeholder; check then asks for rewording.
#[test]
fn promote_strip_leaves_a_placeholder_to_reword() {
    let f = promote_fixture(&format!("Ask {} first.\n", email("bob")));
    promote(&f.stacks(), "rule.r", Promote::Strip, &Rules::public(), false).unwrap();
    assert!(f.read("pub/global/rules/r.md").ends_with("Ask <private:contact> first.\n"));
    let found = f.check();
    assert_eq!(errors(&found), Vec::<&str>::new());
    assert!(has(&found, "placeholder", "rule.r"));
}

// Spec (Import and memory; Privacy): a memory isn't promoted as is, and a project or repo name only becomes
// public once the public stack declares it.
#[test]
fn promote_refuses_memories_and_undeclared_layers() {
    let f = Fixture::new();
    f.write("priv/project/beta/rules/b.md", &entry("rule.b", "project:beta", "private", "", "x\n"));
    f.write("priv/global/memory/m.md", &entry("memory.m", "global", "private", "", "y\n"));
    let s = f.stacks();
    assert!(promote(&s, "rule.b", Promote::Clean, &Rules::public(), false)
        .unwrap_err()
        .contains("declare project `beta`"));
    assert!(promote(&s, "memory.m", Promote::Clean, &Rules::public(), false).is_err());
}

// Spec (brief, GUI: "editing with metadata fields"; design: Entry metadata): an edit changes only the fields it
// names and the text, keeps every other line, and commits; it refuses an inherited entry's text, a memory's
// text, an undeclared topic, and anything that would leave the entry unloadable (then nothing changes).
#[test]
fn edit_changes_named_fields_and_refuses_what_would_break() {
    let f = Fixture::new();
    let original =
        entry("rule.e", "global", "private", "# kept comment\ntopics: [x]\n", "Old text.\n");
    f.write("priv/global/rules/e.md", &original);
    f.write("priv/global/memory/m.md", &entry("memory.m", "global", "private", "", "Note.\n"));
    f.git_init("priv");
    let change = lords_core::ops::Edit {
        title: Some("A better title".into()),
        audience: Some(vec!["opus".into(), "sol".into()]),
        body: Some("New text.\n".into()),
        ..Default::default()
    };
    assert!(lords_core::ops::edit(&f.stacks(), "rule.e", &change, true).unwrap().is_some());
    let now = f.read("priv/global/rules/e.md");
    assert!(
        now.contains("# kept comment\ntopics: [x]\n") && now.ends_with("---\nNew text.\n"),
        "{now}"
    );
    let s = f.stacks();
    let e = s.find("rule.e").unwrap();
    assert_eq!(e.meta.title, "A better title");
    assert_eq!(git(&f.priv_root(), &["log", "-1", "--format=%s"]).trim(), "Edit rule.e");

    let refuse = |id: &str, change: lords_core::ops::Edit| {
        lords_core::ops::edit(&f.stacks(), id, &change, false).is_err()
    };
    let body = |b: &str| lords_core::ops::Edit { body: Some(b.into()), ..Default::default() };
    assert!(refuse("memory.m", body("x")), "a memory's text");
    assert!(refuse(
        "rule.e",
        lords_core::ops::Edit { topics: Some(vec!["nope".into()]), ..Default::default() }
    ));
    let before = f.read("priv/global/rules/e.md");
    assert!(refuse(
        "rule.e",
        lords_core::ops::Edit { audience: Some(vec!["nobody".into()]), ..Default::default() }
    ));
    assert_eq!(f.read("priv/global/rules/e.md"), before, "a broken edit is put back");
}

// Spec (orchestrator, 25 Oct: roles come from the stack's roles list): an edit sets roles from that list and
// refuses an undeclared one.
#[test]
fn edit_sets_declared_roles_only() {
    let f = Fixture::new();
    f.write(
        "priv/stack.toml",
        "name = \"priv\"\nvisibility = \"private\"\nroles = [\"orchestrator\"]\n",
    );
    f.write("priv/global/memory/n.md", &entry("memory.n", "global", "private", "", "Note.\n"));
    let set = |roles: &[&str]| {
        let change = lords_core::ops::Edit {
            roles: Some(roles.iter().map(|r| r.to_string()).collect()),
            ..Default::default()
        };
        lords_core::ops::edit(&f.stacks(), "memory.n", &change, false)
    };
    set(&["orchestrator"]).unwrap();
    assert_eq!(f.stacks().find("memory.n").unwrap().meta.roles, ["orchestrator"]);
    assert!(set(&["admiral"]).is_err());
}
