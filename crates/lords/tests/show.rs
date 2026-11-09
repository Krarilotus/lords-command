//! `lords show <id>` (the Lords owner, 31 Oct, plan L8: the body "opened by id with `lords show <id>` (the body,
//! without frontmatter)"). The child gets an empty home, so no real config or stack is read.

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

use std::fs;
use std::path::Path;
use std::process::Output;
use tempfile::TempDir;

fn entry(id: &str, visibility: &str, extra: &str, body: &str) -> String {
    let kind = id.split('.').next().unwrap();
    format!(
        "---\nid: {id}\nkind: {kind}\ntitle: The {id}\naudience: [all]\nscope: global\nstatus: active\nvisibility: {visibility}\norigin:\n  type: own\n{extra}---\n{body}"
    )
}

fn put(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

/// Two stacks: a public rule with a core line and frontmatter evidence, a skill, a retired rule with its successor, an
/// ended temporary one, and one id in both stacks.
fn stacks() -> TempDir {
    let d = TempDir::new().unwrap();
    let (public, private) = (d.path().join("pub"), d.path().join("priv"));
    put(&public, "stack.toml", "name = \"pub\"\nvisibility = \"public\"\n");
    put(&private, "stack.toml", "name = \"priv\"\nvisibility = \"private\"\n");
    let evidence =
        "evidence:\n- summary: METADATA-ONLY\n  date: 2026-10-31\nstart: core\ncore: The line.\n";
    put(
        &public,
        "global/rules/kept.md",
        &entry("rule.kept", "public", evidence, "\nFirst line.\r\nSecond line.\n\n"),
    );
    put(&public, "global/rules/next.md", &entry("rule.next", "public", "", "The successor.\n"));
    put(
        &public,
        "global/rules/gone.md",
        &entry(
            "rule.gone",
            "public",
            "superseded_by: rule.next\nretired:\n  date: 2026-10-30\n  reason: folded into rule.next\n",
            "Old text.\n",
        )
        .replace("status: active", "status: retired"),
    );
    put(
        &public,
        "global/rules/ended.md",
        &entry(
            "rule.ended",
            "public",
            "temporary:\n  until: 2000-01-01\n  review_by: 2999-12-31\n  reason: a trial\n",
            "Ended text.\n",
        ),
    );
    put(&public, "global/rules/twice.md", &entry("rule.twice", "public", "", "Public copy.\n"));
    put(&private, "global/rules/twice.md", &entry("rule.twice", "private", "", "Private copy.\n"));
    put(
        &public,
        "global/skills/tool/lords.yaml",
        "id: skill.tool\nkind: skill\ntitle: A tool\naudience: [all]\nscope: global\nstatus: active\nvisibility: public\norigin:\n  type: own\n",
    );
    put(
        &public,
        "global/skills/tool/SKILL.md",
        "---\nname: tool\ndescription: SKILL-FRONTMATTER\n---\nUse the tool.\n",
    );
    d
}

fn show(d: &TempDir, id: &str) -> Output {
    let home = d.path().join("home");
    fs::create_dir_all(&home).unwrap();
    std::process::Command::new(env!("CARGO_BIN_EXE_lords"))
        .current_dir(d.path())
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .args(["show", id, "--public-stack"])
        .arg(d.path().join("pub"))
        .arg("--private-stack")
        .arg(d.path().join("priv"))
        .output()
        .unwrap()
}

fn out(o: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

// Spec (plan L8): `show` prints the entry's body as written, without frontmatter (the entry's or a skill's own),
// line endings as LF and no outer blank lines, and exits 0.
#[test]
fn show_prints_the_body_without_frontmatter() {
    let d = stacks();
    let o = show(&d, "rule.kept");
    assert_eq!(out(&o).0, "First line.\nSecond line.\n", "{:?}", out(&o));
    assert!(o.status.success());
    let o = show(&d, "skill.tool");
    assert_eq!(out(&o).0, "Use the tool.\n", "{:?}", out(&o));
    assert!(o.status.success());
}

// Spec (the brief, inputs to attack: "`lords show` for an unknown id, a retired id, and an id that exists in both
// stores"): each prints nothing on stdout, says why on stderr and fails: an unknown id; a retired one, naming its
// successor; an ended one; an id two entries hold, naming both files, so neither is taken for the other. The two
// files are named under their stacks' places, `<argument 3>` (`--public-stack`) and `<argument 5>` (`--private-stack`),
// since a stack's directory is a path, which no error repeats (the master, 9 Nov: "paths stay redacted; the private
// stack's location must never be echoed, so stack directories are not keys"; design note privacy-own-allow).
#[test]
fn show_refuses_an_unknown_retired_ended_or_doubled_id() {
    let d = stacks();
    let cases: &[(&str, &[&str])] = &[
        ("rule.none", &["no entry `rule.none`"]),
        ("rule.gone", &["retired", "rule.next"]),
        ("rule.ended", &["ended"]),
        ("rule.twice", &["2 entries", "<argument 3>", "<argument 5>"]),
    ];
    for (id, says) in cases {
        let o = show(&d, id);
        let (stdout, stderr) = out(&o);
        assert!(!o.status.success(), "{id} fails");
        assert_eq!(stdout, "", "{id}: nothing on stdout");
        for s in *says {
            assert!(stderr.contains(s), "{id}: `{s}` in {stderr}");
        }
        if *id == "rule.twice" {
            assert_eq!(stderr.matches("twice.md").count(), 2, "both files named: {stderr}");
        }
    }
}
