//! The privacy check, against docs/design.md (Privacy). Samples are assembled at run time so this file itself
//! passes the repo's privacy check.

mod common;
use common::*;
use lords_core::privacy::{fleet_hosts, scan, strip, Class, Rules};

fn classes(text: &str, rules: &Rules) -> Vec<Class> {
    scan(text, rules).into_iter().map(|h| h.class).collect()
}

// Spec (Privacy, classes): each class matches its shape; placeholders and generic paths don't.
#[test]
fn each_class_matches_its_shape() {
    let r = Rules::public();
    let users = "Users";
    let cases: Vec<(String, Vec<Class>)> = vec![
        (format!("see C:\\{users}\\alice\\x"), vec![Class::Path]),
        (format!("see C:\\\\{users}\\\\alice in JSON"), vec![Class::Path]),
        (format!("cd /mnt/c/{users}/alice/x"), vec![Class::Path]),
        (format!("cd /c/{users}/alice/x"), vec![Class::Path]),
        (format!("ssh to /{}/alice/repo", "home"), vec![Class::Path]),
        (format!("mail {}", email("alice")), vec![Class::Contact]),
        (format!("{}_{}", "ghp", "a1".repeat(18)), vec![Class::Secret]),
        (format!("https://x.org/hook?token={}", "z9".repeat(6)), vec![Class::Secret]),
        (format!("https://hooks.slack.com/{}/T0/B0/xyz", "services"), vec![Class::Secret]),
        (format!("api_key: {}", "q7".repeat(10)), vec![Class::Secret]),
        // Not private: placeholders, the home shorthand, generic words.
        (
            format!("X:\\{users}\\<name>, /mnt/<d>/{users}/<name>, /home/<name>, %USERPROFILE%"),
            vec![],
        ),
        ("~/.claude/CLAUDE.md and ~/.codex/AGENTS.md".to_string(), vec![]),
        ("token = \"anunbrokenwordwithoutdigits\"".to_string(), vec![]),
    ];
    for (text, want) in cases {
        assert_eq!(classes(&text, &r), want, "{text}");
    }
}

// Spec (Privacy): the private stack is scanned for secrets only.
#[test]
fn secrets_only_ignores_paths_and_contacts() {
    let text = format!("{} {}", user_path("alice"), email("alice"));
    assert_eq!(classes(&text, &Rules::secrets_only()), vec![]);
}

// Spec (Privacy, deny): each deny-list term matches as a whole word, any case; the allow-list suppresses exact
// texts.
#[test]
fn deny_terms_match_whole_words_and_allow_suppresses() {
    let mut r = Rules::public();
    r.deny = vec!["kestrel".into()];
    assert_eq!(classes("Ask Kestrel first.", &r), vec![Class::Deny]);
    assert_eq!(classes("the kestrels flew", &r), vec![]);
    r.allow.insert("Kestrel".into());
    assert_eq!(classes("Ask Kestrel first.", &r), vec![]);
}

// Spec (Privacy, fleet): host names are the fleet config's host and machine keys plus each link target.
#[test]
fn fleet_hosts_come_from_the_config() {
    let f = Fixture::new();
    let p = f.write(
        "fleet.toml",
        "[fleet]\nname = \"x\"\n[host.alpha-pc]\nlink = { kind = \"ssh\", target = \"Alpha-PC\" }\n[machine.beta]\ncores = 2\n",
    );
    assert_eq!(fleet_hosts(&p).unwrap(), ["alpha-pc", "beta"]);
    let mut r = Rules::public();
    r.hosts = fleet_hosts(&p).unwrap();
    assert_eq!(classes("built on alpha-pc and Beta", &r), vec![Class::Fleet, Class::Fleet]);
}

// Spec (Privacy, promotion): stripping replaces each finding by a `<private:class>` placeholder; nothing else
// changes.
#[test]
fn strip_replaces_only_the_spans() {
    let text = format!("a\nmail {} or {} now\nb\n", email("x"), email("y"));
    let hits = scan(&text, &Rules::public());
    assert_eq!(strip(&text, &hits), "a\nmail <private:contact> or <private:contact> now\nb\n");
}

// Spec (Privacy): without a private stack the deny and fleet classes can't run, and that is reported.
#[test]
fn a_missing_private_stack_is_reported() {
    let r = Rules::public().with_private(None);
    assert_eq!(r.notes.len(), 1);
}
