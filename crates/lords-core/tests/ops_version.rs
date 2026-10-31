//! An edit or a retirement made from what was read lands only on that version (the Lords panel, design note
//! 2026-10-31-lords-panel-remote: "never over a newer version").

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;
use common::*;
use lords_core::ops::{edit, retire, Edit, Retire};

fn retire_from<'a>(id: &'a str, base: &'a str) -> Retire<'a> {
    Retire {
        id,
        reason: "r",
        superseded_by: None,
        date: "2026-10-31".into(),
        commit: true,
        base: Some(base),
    }
}

// Spec (brief, inputs to attack: "two writers at once (the panel and an agent's commit), and a stale hash"): an
// edit or retirement whose version was read before an agent committed a change is refused and changes nothing; the
// same change made from the version now on disk goes through. A skill's version covers its text as well as its
// sidecar, so a change to SKILL.md alone makes an older version stale too.
#[test]
fn a_change_made_from_an_older_version_is_refused_and_writes_nothing() {
    let f = Fixture::new();
    f.write("priv/global/rules/v.md", &entry("rule.v", "global", "private", "", "Read.\n"));
    f.write(
        "priv/global/skills/s/lords.yaml",
        "id: skill.s\nkind: skill\ntitle: s\naudience: [all]\nscope: global\nstatus: active\nvisibility: private\norigin:\n  type: own\n",
    );
    f.write("priv/global/skills/s/SKILL.md", "---\nname: s\n---\nRead.\n");
    f.git_init("priv");
    let read = f.stacks();
    let (rule_read, skill_read) = (
        read.find("rule.v").unwrap().version.clone(),
        read.find("skill.s").unwrap().version.clone(),
    );

    // An agent changes both and commits, after the panel read them.
    let agents = entry("rule.v", "global", "private", "", "The agent's.\n");
    f.write("priv/global/rules/v.md", &agents);
    f.write("priv/global/skills/s/SKILL.md", "---\nname: s\n---\nThe agent's.\n");
    git(&f.priv_root(), &["commit", "-q", "-am", "an agent's change"]);
    let skill_files = |f: &Fixture| {
        (f.read("priv/global/skills/s/lords.yaml"), f.read("priv/global/skills/s/SKILL.md"))
    };
    let skill_before = skill_files(&f);

    let change = |base: &str| Edit {
        title: Some("Mine".into()),
        body: Some("Mine.\n".into()),
        base: Some(base.into()),
        ..Default::default()
    };
    let stale = edit(&f.stacks(), "rule.v", &change(&rule_read), true);
    assert!(stale.as_ref().is_err_and(|e| e.contains("changed since it was read")), "{stale:?}");
    let stale = retire(&f.stacks(), &retire_from("rule.v", &rule_read)).map(|r| r.file);
    assert!(stale.as_ref().is_err_and(|e| e.contains("changed since it was read")), "{stale:?}");
    let title = Edit { title: Some("Mine".into()), base: Some(skill_read), ..Default::default() };
    let stale = edit(&f.stacks(), "skill.s", &title, true);
    assert!(stale.as_ref().is_err_and(|e| e.contains("changed since it was read")), "{stale:?}");
    assert_eq!(f.read("priv/global/rules/v.md"), agents, "the agent's version stands");
    assert_eq!(skill_files(&f), skill_before, "the agent's version stands");
    assert_eq!(git(&f.priv_root(), &["log", "-1", "--format=%s"]).trim(), "an agent's change");

    // Made from what is there now, the same change lands, and so does a retirement made from its result.
    let now = f.stacks().find("rule.v").unwrap().version.clone();
    edit(&f.stacks(), "rule.v", &change(&now), true).unwrap();
    assert!(f.read("priv/global/rules/v.md").ends_with("---\nMine.\n"));
    let now = f.stacks().find("rule.v").unwrap().version.clone();
    assert!(retire(&f.stacks(), &retire_from("rule.v", &now)).is_ok());
    assert!(!f.stacks().find("rule.v").unwrap().active());
}
