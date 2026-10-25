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
        // An `\n` escape before an include line in source, not an address.
        (format!("\"-->{}n{}AGENTS.md\"", char::from(92), '@'), vec![]),
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

// Spec (orchestrator, 25 Oct: "Show me the before/after privacy output over all commits (`git log -p` through the
// privacy scanner), not only the tree"): a private detail a later commit removed is still found, in the commit
// that added it, and so is one in a commit message; a clean history gives none.
#[test]
fn the_history_scan_finds_what_the_tree_no_longer_shows() {
    let f = Fixture::new();
    let repo = f.path("repo");
    f.write("repo/a.md", &format!("mail {}\n", email("alice")));
    git(&repo, &["init", "-q"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "first"]);
    f.write("repo/a.md", "nothing here\n");
    git(&repo, &["commit", "-q", "-am", "Ask Kestrel about it"]);
    let mut rules = Rules::public();
    rules.deny = vec!["kestrel".into()];
    let tree = lords_core::privacy::scan_tree(&repo, &rules).unwrap();
    assert!(tree.hits.is_empty(), "the tree is clean");
    let h = lords_core::privacy::scan_history(&repo, &rules).unwrap();
    assert_eq!(h.commits, 2);
    let places: Vec<(&str, Class)> =
        h.hits.iter().map(|x| (x.place.as_str(), x.hit.class)).collect();
    assert!(places.contains(&("a.md", Class::Contact)), "{places:?}");
    assert!(places.contains(&("message", Class::Deny)), "{places:?}");

    let clean = f.path("clean");
    f.write("clean/a.md", "fine\n");
    git(&clean, &["init", "-q"]);
    git(&clean, &["add", "-A"]);
    git(&clean, &["commit", "-q", "-m", "fine"]);
    assert!(lords_core::privacy::scan_history(&clean, &rules).unwrap().hits.is_empty());
}
