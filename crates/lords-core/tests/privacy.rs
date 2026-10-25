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
    let r = Rules::public().with_private(None).unwrap();
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

// Spec (Sol review 25 Oct, finding 16: "Unicode case variants bypass deny and fleet matches"): deny and fleet
// terms match whatever the case, in any script, and a decomposed spelling matches the composed term.
#[test]
fn deny_and_fleet_terms_match_unicode_case_and_normal_forms() {
    let mut r = Rules::public();
    r.deny = vec!["m\u{fc}ller".into()];
    assert_eq!(classes("Ask M\u{dc}LLER first.", &r), vec![Class::Deny]);
    assert_eq!(classes("Ask Mu\u{308}ller first.", &r), vec![Class::Deny]);
    let f = Fixture::new();
    let p = f.write("fleet.toml", "[host.\"\u{d6}STE\"]\ncores = 2\n");
    r.hosts = fleet_hosts(&p).unwrap();
    assert_eq!(classes("built on \u{f6}ste", &r), vec![Class::Fleet]);
}

// Spec (Sol review 25 Oct, finding 17: "Alphabetic API tokens are explicitly excluded"): a long letters-only value
// assigned to a secret-named key is a secret. (Its exception for one ordinary word joined up went with R2-14.)
#[test]
fn a_letters_only_token_after_a_secret_key_is_a_secret() {
    let r = Rules::public();
    let token = ["QwErTyUiOpAsDfGh", "JkLzXcVbNmAbCdEf"].concat();
    assert_eq!(classes(&format!("api_key = '{token}'"), &r), vec![Class::Secret]);
    assert_eq!(
        classes(&format!("password: {}", "abcdefghijklmnopqrstuvwxyzabcdefgh"), &r),
        vec![Class::Secret]
    );
}

// Spec (Sol review 25 Oct, finding 25: "A CRLF-wrapped deny phrase bypasses scanning"): a multi-word deny term
// matches across any whitespace run, a line break (CRLF included) too.
#[test]
fn a_deny_phrase_matches_across_line_breaks() {
    let mut r = Rules::public();
    r.deny = vec!["secret project".into()];
    assert_eq!(classes("our secret\r\nproject here", &r), vec![Class::Deny]);
    assert_eq!(classes("our Secret \t\n  Project here", &r), vec![Class::Deny]);
    assert_eq!(classes("a secret\n\nprojector", &r), vec![]);
}

// Spec (Sol review r2, R2-13: "NFKD plus lowercase is not Unicode case folding"): deny terms match under full
// Unicode case folding (sharp s and SS, final and medial sigma), either way round, and the hit is the original text.
#[test]
fn deny_terms_match_under_full_case_folding() {
    let mut r = Rules::public();
    r.deny = vec!["stra\u{df}e".into(), "\u{3bf}\u{3c3}".into()];
    assert_eq!(classes("in der STRASSE hier", &r), vec![Class::Deny]);
    assert_eq!(classes("a \u{3bf}\u{3c2} b", &r), vec![Class::Deny]);
    r.deny = vec!["strasse".into()];
    let hits = scan("die Stra\u{df}e.", &r);
    assert_eq!(hits.iter().map(|h| h.text.as_str()).collect::<Vec<_>>(), ["Stra\u{df}e"]);
}

// Spec (Sol review r2, R2-14: "Shorter alphabetic API tokens still pass the generic secret gate"): after a
// secret-named key, any value of at least 16 token characters is a secret, whatever its alphabet; a shorter one is
// not.
#[test]
fn any_long_value_after_a_secret_named_key_is_a_secret() {
    let r = Rules::public();
    let lower = ["qxnwzrtplvbc", "jkhdfsmgayue"].concat();
    assert_eq!(classes(&format!("api_key='{lower}'"), &r), vec![Class::Secret]);
    assert_eq!(classes(&format!("TOKEN = \"{}\"", "QXNWZRTPLVBCJKHD"), &r), vec![Class::Secret]);
    assert_eq!(classes(&format!("secret: {}", "qxnwzrtplvbcjkh"), &r), vec![]);
}

// Spec (Sol review r2, R2-22: "The new multiline deny hit is only partly stripped"): stripping a deny phrase
// wrapped across lines replaces the whole phrase, every line of it.
#[test]
fn strip_removes_a_wrapped_private_phrase() {
    let mut r = Rules::public();
    r.deny = vec!["secret project".into()];
    let text = "our secret\r\nproject here\nnext\n";
    assert_eq!(strip(text, &scan(text, &r)), "our <private:deny> here\nnext\n");
}

/// A repo with `files` staged (not committed).
fn staged_repo(f: &Fixture, files: &[(&str, &[u8])]) -> std::path::PathBuf {
    let repo = f.path("staged");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    for (name, bytes) in files {
        std::fs::write(repo.join(name), bytes).unwrap();
    }
    git(&repo, &["add", "-A"]);
    repo
}

// Spec (Sol review 25 Oct, finding 12: "One NUL byte disables staged privacy scanning"): a staged file with a NUL
// byte or invalid UTF-8 is still scanned, UTF-16 with a byte order mark is decoded and scanned as text, and the
// history scan sees binary patches too.
#[test]
fn binary_and_utf16_content_is_still_scanned() {
    let f = Fixture::new();
    let mail = email("alice");
    let nul = [b"\0".as_slice(), mail.as_bytes(), b"\n"].concat();
    let bad = [b"\xff\xfe\xfd ".as_slice(), mail.as_bytes()].concat();
    let utf16: Vec<u8> = [0xff_u8, 0xfe]
        .into_iter()
        .chain(format!("mail {mail}\r\n").encode_utf16().flat_map(|u| u.to_le_bytes()))
        .collect();
    let repo = staged_repo(&f, &[("a.dat", &nul), ("b.dat", &bad), ("c.txt", &utf16)]);
    let scan = lords_core::privacy::scan_staged(&repo, &Rules::public()).unwrap();
    let mut files: Vec<String> =
        scan.hits.iter().map(|(p, h)| format!("{} {:?}", p.display(), h.class)).collect();
    files.dedup();
    assert_eq!(files, ["a.dat Contact", "b.dat Contact", "c.txt Contact"]);
    assert!(!scan.binary.iter().any(|p| p.ends_with("c.txt")), "UTF-16 is text: {:?}", scan.binary);

    git(&repo, &["commit", "-q", "-m", "binary"]);
    let h = lords_core::privacy::scan_history(&repo, &Rules::public()).unwrap();
    let places: Vec<&str> = h.hits.iter().map(|x| x.place.as_str()).collect();
    for p in ["a.dat", "b.dat"] {
        assert!(places.contains(&p), "{places:?}");
    }
}

// Spec (Sol review 25 Oct, finding 13: "Staged filenames are never privacy-checked"): a staged file's path is
// scanned with the same rules as its content.
#[test]
fn a_staged_file_name_is_scanned() {
    let f = Fixture::new();
    let name = format!("{}.txt", email("alice"));
    let repo = staged_repo(&f, &[(&name, b"innocuous\n")]);
    let mut r = Rules::public();
    r.deny = vec!["kestrel".into()];
    std::fs::create_dir_all(repo.join("Kestrel")).unwrap();
    std::fs::write(repo.join("Kestrel/x.md"), "fine\n").unwrap();
    git(&repo, &["add", "-A"]);
    let scan = lords_core::privacy::scan_staged(&repo, &r).unwrap();
    let classes: Vec<Class> = scan.hits.iter().map(|(_, h)| h.class).collect();
    assert_eq!(classes, [Class::Deny, Class::Contact], "{:?}", scan.hits);
}

// Spec (Sol review r2, R2-15: "Marked BOM-less UTF-16 can still leak non-ASCII deny terms"): staged UTF-16 without
// a byte order mark, little- or big-endian, is decoded and scanned, also in a file marked `lords-binary`.
#[test]
fn utf16_without_a_byte_order_mark_is_decoded() {
    let f = Fixture::new();
    let word = "Ask M\u{dc}LLER first\r\n";
    let le: Vec<u8> = word.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    let be: Vec<u8> = word.encode_utf16().flat_map(|u| u.to_be_bytes()).collect();
    let attrs = b"*.dat lords-binary\n".as_slice();
    let repo = staged_repo(&f, &[(".gitattributes", attrs), ("le.dat", &le), ("be.dat", &be)]);
    let mut r = Rules::public();
    r.deny = vec!["m\u{fc}ller".into()];
    let scan = lords_core::privacy::scan_staged(&repo, &r).unwrap();
    let mut found: Vec<String> =
        scan.hits.iter().map(|(p, h)| format!("{} {:?}", p.display(), h.class)).collect();
    found.sort();
    assert_eq!(found, ["be.dat Deny", "le.dat Deny"], "{:?}", scan.hits);
    assert!(scan.unmarked.is_empty(), "{:?}", scan.unmarked);
}
