//! An edit or a retirement made from what was read lands only on that version (the Lords panel, design note
//! 2026-10-31-lords-panel-remote: "never over a newer version").

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;
use common::*;
use lords_core::fsguard;
use lords_core::git::Covered;
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

// Spec (review r1 R3: a retirement holds its version over every file the version covers, as an edit does): a skill
// whose text has a change nobody committed yet (an agent's, mid-work) is neither retired nor edited: each is refused
// before anything is written or committed, since what the version was read from isn't in git to come back to.
#[test]
fn a_change_to_an_entry_whose_text_isnt_committed_is_refused_and_commits_nothing() {
    let f = Fixture::new();
    f.write(
        "priv/global/skills/s/lords.yaml",
        "id: skill.s\nkind: skill\ntitle: s\naudience: [all]\nscope: global\nstatus: active\nvisibility: private\norigin:\n  type: own\n",
    );
    f.write("priv/global/skills/s/SKILL.md", "---\nname: s\n---\nRead.\n");
    f.git_init("priv");
    let agents = "---\nname: s\n---\nThe agent's, not committed yet.\n";
    f.write("priv/global/skills/s/SKILL.md", agents);
    let read = f.stacks().find("skill.s").unwrap().version.clone();
    let head = || git(&f.priv_root(), &["rev-parse", "HEAD"]);
    let (before, meta) = (head(), f.read("priv/global/skills/s/lords.yaml"));

    let retired = retire(&f.stacks(), &retire_from("skill.s", &read)).map(|r| r.file);
    assert!(retired.as_ref().is_err_and(|e| e.contains("uncommitted changes")), "{retired:?}");
    let title = Edit { title: Some("Mine".into()), base: Some(read), ..Default::default() };
    let edited = edit(&f.stacks(), "skill.s", &title, true);
    assert!(edited.as_ref().is_err_and(|e| e.contains("uncommitted changes")), "{edited:?}");
    assert_eq!(head(), before, "nothing committed, nothing reverted");
    assert_eq!(f.read("priv/global/skills/s/lords.yaml"), meta);
    assert_eq!(f.read("priv/global/skills/s/SKILL.md"), agents, "the agent's work stands");
}

fn covered<'a>(path: &'a str, was: &'a [u8], wrote: Option<&'a [u8]>) -> Covered<'a> {
    Covered { path: std::path::Path::new(path), was, wrote }
}

// Spec (review r1 R1, the Lords owner's direction: git's own `git commit --only -- <paths>` in the repo's own index, the
// version checked against the new commit's parent): an entry's commit
// - holds the bytes the edit wrote, and the repo's index agrees with it at once;
// - when the stack repo's hooks refuse it, leaves HEAD, the index and the file as they were (Refused);
// - when it carries another writer's bytes for a file it wrote (a write between the edit's own and git's staging), is
//   reported for what it is, never as this edit's success;
// - when a file the version covers, written or not, changed in the commit's parent since it was read, is reported as a
//   conflict naming the edit's commit and the newer one, and nothing further changes: no revert, history keeps both,
//   the window reloads and the owner decides (the Lords owner, fix check r1 N1, N2);
// - when an agent's `git commit -a` already took the file the edit wrote, so nothing is left to commit, is reported as
//   that commit, if that one commit holds the version as the edit's own would: its parent holds every covered file as
//   read, and it changes none the edit didn't write; otherwise the same conflict, and the agent's commit stays (fix
//   check r1 R3, the review's sidecar and body witness).
#[test]
fn an_entrys_commit_holds_its_version_over_every_covered_file() {
    use lords_core::git::{commit_entry, Uncommitted};
    let f = Fixture::new();
    f.write("priv/global/skills/s/lords.yaml", "meta 1\n");
    f.write("priv/global/skills/s/SKILL.md", "body 1\n");
    f.git_init("priv");
    let root = f.priv_root();
    let (meta, body) = ("global/skills/s/lords.yaml", "global/skills/s/SKILL.md");
    let head = || git(&root, &["rev-parse", "HEAD"]);

    // The ordinary case: the sidecar written, the text only covered.
    f.write(&format!("priv/{meta}"), "meta 2\n");
    let made = commit_entry(
        &root,
        &[covered(meta, b"meta 1\n", Some(b"meta 2\n")), covered(body, b"body 1\n", None)],
        "Edit skill.s",
    );
    assert!(matches!(&made, Ok(Some(_))), "{made:?}");
    assert_eq!(git(&root, &["show", &format!("HEAD:{meta}")]), "meta 2\n");
    assert_eq!(git(&root, &["status", "--porcelain"]), "", "the index agrees at once");

    // A hook that sees what is committed, and refuses it.
    f.write("priv/.git/hooks/pre-commit", "#!/bin/sh\ngit diff --cached | grep -q SECRET && { echo 'refused' >&2; exit 1; }\nexit 0\n");
    let before = head();
    f.write(&format!("priv/{meta}"), "SECRET\n");
    let refused =
        commit_entry(&root, &[covered(meta, b"meta 2\n", Some(b"SECRET\n"))], "Edit skill.s");
    assert!(
        matches!(&refused, Err(Uncommitted::Refused(m)) if m.contains("refused")),
        "{refused:?}"
    );
    assert_eq!(head(), before);
    assert_eq!(git(&root, &["diff", "--cached", "--name-only"]), "");
    f.write(&format!("priv/{meta}"), "meta 2\n");
    std::fs::remove_file(root.join(".git/hooks/pre-commit")).unwrap();

    // Another writer's bytes on disk when git stages: committed, but never as this edit's success.
    f.write(&format!("priv/{meta}"), "an agent's\n");
    let theirs =
        commit_entry(&root, &[covered(meta, b"meta 2\n", Some(b"meta 3\n"))], "Edit skill.s");
    assert!(
        matches!(&theirs, Err(Uncommitted::Published { problem, .. }) if problem.contains("another writer's bytes")),
        "{theirs:?}"
    );
    let short = || git(&root, &["rev-parse", "--short", "HEAD"]).trim().to_string();

    // The covered text changed in HEAD since the version was read: a conflict naming both commits; nothing reverted.
    f.write(&format!("priv/{body}"), "body 2\n");
    git(&root, &["commit", "-q", "-am", "an agent's text"]);
    let newer = short();
    f.write(&format!("priv/{meta}"), "meta 4\n");
    let stale = commit_entry(
        &root,
        &[covered(meta, b"an agent's\n", Some(b"meta 4\n")), covered(body, b"body 1\n", None)],
        "Edit skill.s",
    );
    let mine = short();
    assert!(
        matches!(&stale, Err(Uncommitted::Conflict(m)) if m.contains(body) && m.contains(&mine) && m.contains(&newer)),
        "{stale:?}"
    );
    assert_eq!(
        git(&root, &["log", "-2", "--format=%s"]),
        "Edit skill.s\nan agent's text\n",
        "no revert"
    );
    assert_eq!(git(&root, &["show", &format!("HEAD:{meta}")]), "meta 4\n");
    assert_eq!(git(&root, &["show", &format!("HEAD:{body}")]), "body 2\n");
    assert_eq!(git(&root, &["status", "--porcelain"]), "");

    // An agent's `commit -a` already took the edit's bytes, and nothing else the version covers: the edit's commit.
    f.write(&format!("priv/{meta}"), "meta 5\n");
    git(&root, &["commit", "-q", "-am", "an agent's commit -a"]);
    let theirs = short();
    let taken = commit_entry(
        &root,
        &[covered(meta, b"meta 4\n", Some(b"meta 5\n")), covered(body, b"body 2\n", None)],
        "Edit skill.s",
    );
    assert_eq!(taken, Ok(Some(theirs)));
}

// Spec (fix check r1 R3, the review's witness; the Lords owner: check every covered file on that one identified
// commit, never revert the agent's): the panel writes a skill's sidecar (a retirement), then an agent saves the skill's
// text and `git commit -a`s both. That commit holds the change's bytes and a text the change never read: a conflict
// naming it, saying it is the one commit that carries the change (fix check r2), and nothing further changes.
#[test]
fn an_agents_commit_that_holds_the_change_and_a_text_it_never_read_is_a_conflict() {
    use lords_core::git::{commit_entry, Uncommitted};
    let f = Fixture::new();
    f.write("priv/global/skills/s/lords.yaml", "meta 1\n");
    f.write("priv/global/skills/s/SKILL.md", "body 1\n");
    f.git_init("priv");
    let root = f.priv_root();
    let (meta, body) = ("global/skills/s/lords.yaml", "global/skills/s/SKILL.md");
    f.write(&format!("priv/{meta}"), "retired\n");
    f.write(&format!("priv/{body}"), "the agent's\n");
    git(&root, &["commit", "-q", "-am", "an agent's commit -a"]);
    let (theirs, before) =
        (git(&root, &["rev-parse", "--short", "HEAD"]), git(&root, &["rev-parse", "HEAD"]));
    let mixed = commit_entry(
        &root,
        &[covered(meta, b"meta 1\n", Some(b"retired\n")), covered(body, b"body 1\n", None)],
        "Retire skill.s",
    );
    assert!(
        matches!(&mixed, Err(Uncommitted::Conflict(m)) if m.contains(body)
            && m.contains(&format!("{}, the commit that carries this change, changed it too", theirs.trim()))
            && !m.contains("both stay")),
        "{mixed:?}"
    );
    assert_eq!(
        git(&root, &["rev-parse", "HEAD"]),
        before,
        "the agent's commit stays, nothing on top"
    );
    assert_eq!(git(&root, &["status", "--porcelain"]), "");
}

// Spec (fix check r2 N3, the Lords owner: one candidate commit, named once by its id and checked whole there: its
// written files hold the change's bytes, its parent holds every covered file as read, it changes no covered file the
// change didn't write; success names only that id): the panel writes a skill's sidecar, an agent commits it
// (`commit -a`), then amends that commit with a sidecar of its own. The commit that now carries the file holds bytes
// the change didn't write: never the change's success; a conflict, and the agent's commit stays. (The review's
// schedule puts the amend between two reads of HEAD, where git runs nothing of the repo's that a test could time; with
// one read of one id there is no between, and this is the amend before it.)
#[test]
fn an_agents_amended_commit_without_the_changes_bytes_is_never_its_success() {
    use lords_core::git::{commit_entry, Uncommitted};
    let f = Fixture::new();
    f.write("priv/global/skills/s/lords.yaml", "meta 1\n");
    f.write("priv/global/skills/s/SKILL.md", "body 1\n");
    f.git_init("priv");
    let root = f.priv_root();
    let (meta, body) = ("global/skills/s/lords.yaml", "global/skills/s/SKILL.md");
    f.write(&format!("priv/{meta}"), "retired\n");
    git(&root, &["commit", "-q", "-am", "an agent's commit -a"]);
    f.write(&format!("priv/{meta}"), "the agent's own\n");
    git(&root, &["commit", "-q", "-a", "--amend", "--no-edit"]);
    let before = git(&root, &["rev-parse", "HEAD"]);
    let amended = commit_entry(
        &root,
        &[covered(meta, b"meta 1\n", Some(b"retired\n")), covered(body, b"body 1\n", None)],
        "Retire skill.s",
    );
    assert!(
        matches!(&amended, Err(Uncommitted::Conflict(m)) if m.contains(meta) && m.contains("other bytes")),
        "{amended:?}"
    );
    assert_eq!(git(&root, &["rev-parse", "HEAD"]), before, "the agent's commit stays");
}

// Spec (sol-loop r4, S5 and S6; review r1 R1 witness A): an agent's plain commit from the repo's own index that lands
// on top of the edit's (here from the repo's post-commit hook, which runs after git published the edit and before it
// returns) keeps the edit: HEAD holds the edit's text, the agent's commit stays on top, and the edit reports itself
// committed.
#[test]
fn an_agents_commit_on_top_of_the_edits_stays() {
    let f = Fixture::new();
    f.write("priv/global/rules/a.md", &entry("rule.a", "global", "private", "", "Read.\n"));
    f.git_init("priv");
    let root = f.priv_root();
    f.write(
        "priv/.git/hooks/post-commit",
        "#!/bin/sh\nunset GIT_INDEX_FILE\nmark=\"$(git rev-parse --git-dir)/agent-done\"\n[ -f \"$mark\" ] && exit 0\n: > \"$mark\"\ngit commit -q --allow-empty -m \"an agent's commit\"\n",
    );
    let base = f.stacks().find("rule.a").unwrap().version.clone();
    let change = Edit { body: Some("Mine.\n".into()), base: Some(base), ..Default::default() };
    let made = edit(&f.stacks(), "rule.a", &change, true);
    assert!(matches!(&made, Ok(Some(_))), "{made:?}");
    let log = git(&root, &["log", "-3", "--format=%s"]);
    assert_eq!(log, "an agent's commit\nEdit rule.a\nfixture\n");
    let committed = git(&root, &["show", "HEAD:global/rules/a.md"]);
    assert!(committed.ends_with("---\nMine.\n"), "HEAD holds the edit: {committed}");
    assert!(f.read("priv/global/rules/a.md").ends_with("---\nMine.\n"), "the edit stays");
    assert_eq!(git(&root, &["status", "--porcelain"]), "");
}

// Spec (the owner's private stack repo has `core.autocrlf=true` and no eol attributes, so its files are CRLF on disk
// and LF in HEAD): an edit made from the file's bytes on disk commits over HEAD's version of it, stored as `git add`
// would store it (LF).
#[test]
fn an_edit_in_a_repo_that_converts_line_ends_commits() {
    let f = Fixture::new();
    f.write("priv/global/rules/c.md", "line\r\n");
    git(&f.priv_root(), &["init", "-q"]);
    git(&f.priv_root(), &["config", "core.autocrlf", "true"]);
    git(&f.priv_root(), &["add", "-A"]);
    git(&f.priv_root(), &["commit", "-q", "-m", "fixture"]);
    let root = f.priv_root();
    // The edit's own write, as ops makes it before committing.
    f.write("priv/global/rules/c.md", "mine\r\n");
    let made = lords_core::git::commit_entry(
        &root,
        &[covered("global/rules/c.md", b"line\r\n", Some(b"mine\r\n"))],
        "Edit rule.c",
    );
    assert!(matches!(made, Ok(Some(_))), "{made:?}");
    assert_eq!(git(&root, &["cat-file", "-p", "HEAD:global/rules/c.md"]), "mine\n");
    assert_eq!(git(&root, &["status", "--porcelain"]), "");
}

// Spec (sol-loop r3 S4; the public stack lives in its repo's `stack/` dir): an edit of an entry in a stack below its
// repo's top commits that entry's own file, at its path in the repo, and leaves nothing uncommitted or staged.
#[test]
fn a_stack_below_its_repos_top_commits_its_own_file() {
    let f = Fixture::new();
    f.write("pub/global/rules/s.md", &entry("rule.s", "global", "public", "", "Read.\n"));
    // One repo at the fixture's top: the public stack is its `pub/` dir.
    git(f.dir.path(), &["init", "-q"]);
    git(f.dir.path(), &["add", "-A"]);
    git(f.dir.path(), &["commit", "-q", "-m", "fixture"]);
    let base = f.stacks().find("rule.s").unwrap().version.clone();
    let change = Edit { body: Some("Mine.\n".into()), base: Some(base), ..Default::default() };
    assert!(edit(&f.stacks(), "rule.s", &change, true).unwrap().is_some());
    let committed = git(f.dir.path(), &["show", "HEAD:pub/global/rules/s.md"]);
    assert!(committed.ends_with("---\nMine.\n"), "{committed}");
    assert_eq!(
        git(f.dir.path(), &["status", "--porcelain"]),
        "",
        "nothing left uncommitted or staged"
    );
    assert!(!f.path("global").exists(), "nothing at the repo's top");
}

// Spec (Lords owner, 31 Oct: "build the CAS at the file replacement … a write between your version check and the
// replacement is a reachable witness"): a stack file is replaced only while it holds exactly the bytes the caller
// read. A write that landed after the read stays as it is, the replacement is refused, and nothing is left beside
// the file; from the bytes there now, it goes through.
#[test]
fn a_file_is_replaced_only_while_it_holds_what_was_read() {
    let f = Fixture::new();
    let file = f.write("priv/global/rules/w.md", "read\n");
    let at = fsguard::Root::stack(&f.priv_root()).unwrap().below(&file).unwrap();
    f.write("priv/global/rules/w.md", "an agent's\n");
    let refused = fsguard::replace_file_if(&at, b"read\n", b"mine\n");
    assert!(refused.as_ref().is_err_and(|e| e.contains("changed")), "{refused:?}");
    assert_eq!(f.read("priv/global/rules/w.md"), "an agent's\n");
    let beside: Vec<String> = std::fs::read_dir(f.path("priv/global/rules"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(beside, ["w.md"]);
    fsguard::replace_file_if(&at, b"an agent's\n", b"mine\n").unwrap();
    assert_eq!(f.read("priv/global/rules/w.md"), "mine\n");
    std::fs::remove_file(&file).unwrap();
    assert!(fsguard::replace_file_if(&at, b"mine\n", b"again\n").is_err(), "a file gone since");
    assert!(!file.exists());
}
