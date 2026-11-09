//! A privacy hook takes allowances only from its own checkout, and `privacy --explain` says what it checks (design
//! note 2026-11-08-lords-privacy-own-allow; D28 review r6, R6-F01 and R6-F02; the self-service audit, journey 7).

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

struct Env {
    dir: TempDir,
}

impl Env {
    /// An empty home and global git config, so no real home dir, hook or setting is read or touched.
    fn new() -> Env {
        let dir = TempDir::new().unwrap();
        let email = ["test", "example.invalid"].join("@");
        fs::write(
            dir.path().join("gitconfig"),
            format!("[user]\n\tname = Test\n\temail = {email}\n"),
        )
        .unwrap();
        fs::create_dir_all(dir.path().join("home")).unwrap();
        Env { dir }
    }

    fn cmd(&self, program: &str, cwd: &Path) -> Command {
        let mut c = Command::new(program);
        c.current_dir(cwd)
            .env("GIT_CONFIG_GLOBAL", self.dir.path().join("gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", self.dir.path().join("home"))
            .env("USERPROFILE", self.dir.path().join("home"))
            .env("LORDS_CODEX", env!("CARGO_BIN_EXE_lords-codex-stub"));
        c
    }

    fn lords(&self, cwd: &Path, args: &[&str]) -> Output {
        self.cmd(env!("CARGO_BIN_EXE_lords"), cwd).args(args).output().unwrap()
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> Output {
        self.cmd("git", cwd).args(args).output().unwrap()
    }

    fn ok_git(&self, cwd: &Path, args: &[&str]) {
        let o = self.git(cwd, args);
        assert!(o.status.success(), "git {args:?}: {}", text(&o));
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    /// A repo at `rel` with these files, committed (no hooks are installed in it).
    fn repo(&self, rel: &str, files: &[(&str, &str)]) -> PathBuf {
        let r = self.path(rel);
        fs::create_dir_all(&r).unwrap();
        self.ok_git(&r, &["init", "-q"]);
        for (f, body) in files {
            write(&r.join(f), body);
        }
        self.ok_git(&r, &["add", "-A"]);
        self.ok_git(&r, &["commit", "-q", "-m", "start"]);
        r
    }

    /// config.txt naming a public stack `elsewhere/stack` in a repo of its own, whose allow-list holds
    /// `public_allow`, and a private stack outside any repo with these lists, each left out where empty; its manifest
    /// names a fleet config holding `fleet`, or one that isn't there for `None`. Neither is the checkout under test.
    fn configure(&self, public_allow: &str, allow: &str, deny: &str, fleet: Option<&str>) {
        let mut public = vec![("stack/stack.toml", PUBLIC), ("README.md", "clean\n")];
        if !public_allow.is_empty() {
            public.push(("stack/privacy/allow.txt", public_allow));
        }
        let elsewhere = self.repo("elsewhere", &public);
        let private = self.path("privstack");
        let fleet_path = self.path("fleet.toml");
        write(
            &private.join("stack.toml"),
            &format!("{PRIVATE}[privacy]\nfleet_config = '{}'\n", fleet_path.display()),
        );
        for (file, body) in [("allow.txt", allow), ("deny.txt", deny)] {
            if !body.is_empty() {
                write(&private.join("privacy").join(file), body);
            }
        }
        if let Some(body) = fleet {
            write(&fleet_path, body);
        }
        write(
            &self.path("home/.lords-command/config.txt"),
            &format!(
                "public = {}\nprivate = {}\n",
                elsewhere.join("stack").display(),
                private.display()
            ),
        );
    }
}

const PUBLIC: &str = "name = \"pub\"\nvisibility = \"public\"\n";
const PRIVATE: &str = "name = \"priv\"\nvisibility = \"private\"\n";

fn write(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

/// A private detail that is no secret: a user path, whose private part is `alice`. Built from parts, so this file
/// holds none.
fn detail() -> String {
    format!("logs in C:\\{}\\alice\\logs", "Users")
}

/// A committed checkout at `rel` with a public stack (`public`) or none, and, unless `own_allow` is empty, its own
/// `stack/privacy/allow.txt`.
fn checkout(env: &Env, rel: &str, public: bool, own_allow: &str) -> PathBuf {
    let mut files = vec![("README.md", "a repo\n")];
    if public {
        files.push(("stack/stack.toml", PUBLIC));
    }
    if !own_allow.is_empty() {
        files.push(("stack/privacy/allow.txt", own_allow));
    }
    env.repo(rel, &files)
}

/// `privacy` with `flags`, as the hook named by `how` runs it in `repo`, over content saying `body`: `staged`, a
/// staged `notes.md`; `message`, a commit message; `history`, a commit holding `notes.md`.
fn run_privacy(env: &Env, repo: &Path, how: &str, body: &str, flags: &[&str]) -> Output {
    let notes = repo.join("notes.md");
    match how {
        "staged" => {
            write(&notes, &format!("{body}\n"));
            env.ok_git(repo, &["add", "notes.md"]);
            env.lords(repo, &[&["privacy", "--staged"][..], flags].concat())
        }
        "message" => {
            let msg = env.path("msg");
            write(&msg, &format!("{body}\n"));
            env.lords(
                repo,
                &[&["privacy", "--message", &msg.to_string_lossy()][..], flags].concat(),
            )
        }
        _ => {
            write(&notes, &format!("{body}\n"));
            env.ok_git(repo, &["add", "notes.md"]);
            env.ok_git(repo, &["commit", "-q", "-m", "notes"]);
            env.lords(repo, &[&["privacy", "--history"][..], flags].concat())
        }
    }
}

// Spec (D28 r6, R6-F01: "A public checkout's own message containing a user-path match accepted only by a foreign
// `alice` allowance remains a source witness ... foreign stacks supply terms only"; the master, 23:43; design note
// privacy-own-allow, invariant and attack items 1-4): under `--by-stack`, neither configured stack's allow-list
// accepts a hit in this checkout's staged files, message or history. Rows: the configured private allow-list in a
// public checkout, each way a hook scans; the configured public allow-list in a public checkout; both in a checkout
// with no stack. Every row runs before the refusals are asserted.
#[test]
fn a_privacy_hook_takes_no_configured_allowance() {
    let rows = [
        ("private allow-list, staged", "", "alice\n", true, "staged"),
        ("private allow-list, message", "", "alice\n", true, "message"),
        ("private allow-list, history", "", "alice\n", true, "history"),
        ("public allow-list, staged", "alice\n", "", true, "staged"),
        ("both, no stack, staged", "alice\n", "alice\n", false, "staged"),
    ];
    let outputs: Vec<_> = rows
        .iter()
        .map(|&(row, public_allow, private_allow, public, how)| {
            let env = Env::new();
            env.configure(public_allow, private_allow, "", Some("[host.alpha]\n"));
            let repo = checkout(&env, "repo", public, "");
            (row, run_privacy(&env, &repo, how, &detail(), &["--by-stack"]))
        })
        .collect();
    let let_through: Vec<_> = outputs
        .iter()
        .filter(|(_, o)| o.status.success())
        .map(|(row, o)| format!("{row}: {}", text(o)))
        .collect();
    assert!(let_through.is_empty(), "refused in every row, but let through: {let_through:#?}");
    for (row, o) in &outputs {
        assert!(text(o).contains("path: `alice`"), "{row}: names the hit: {}", text(o));
    }
}

// Spec (design note privacy-own-allow, attack items 5-7): under `--by-stack` a public checkout's own allow-list still
// accepts its false positive; the configured private stack's deny term and fleet host are still refused, naming both;
// and a fleet config its manifest names that isn't there fails the check, naming it.
#[test]
fn a_privacy_hook_keeps_its_own_allowance_and_the_configured_terms() {
    let env = Env::new();
    env.configure("", "", "zephyr plan\n", Some("[host.alpha]\n"));
    let own = checkout(&env, "own", true, "alice\n");
    let o = run_privacy(&env, &own, "staged", &detail(), &["--by-stack"]);
    assert!(o.status.success(), "its own allow-list accepts it: {}", text(&o));
    let repo = checkout(&env, "terms", true, "");
    let o = run_privacy(&env, &repo, "staged", "the zephyr plan runs on alpha", &["--by-stack"]);
    assert!(!o.status.success(), "the terms: refused: {}", text(&o));
    assert!(text(&o).contains("deny: `") && text(&o).contains("fleet: `"), "{}", text(&o));

    let env = Env::new();
    env.configure("", "", "", None);
    let repo = checkout(&env, "repo", true, "");
    let o = run_privacy(&env, &repo, "staged", "clean", &["--by-stack"]);
    assert!(!o.status.success(), "a missing fleet config: refused: {}", text(&o));
    // Named by its role, not its path (the master, 9 Nov 02:36; design note privacy-own-allow, sol-loop r4).
    let out = text(&o);
    assert!(out.contains("fleet config") && !out.contains("fleet.toml"), "names it: {out}");
}

// Spec (package review privownallow r1, R1-F01: "a plain repo's full-policy check adds configured-private allowances
// ... a private-store allowance can let a private term into an unrelated public plain repo"; the master, 00:23: "no
// configured-private allowances, only the repo's own allow-list if it has one. Configured-public allowances stay. Invert
// that test into a fails-without test"; design note privacy-own-allow, r1 attack items 24-26): without `--by-stack`,
// the configured private stack's allow-list accepts no hit in a plain repo's staged files, message or history, nor at
// a commit through the plain hooks `lords hooks install` writes, which leaves HEAD as it was; the configured public
// stack's allow-list still accepts it. Every row runs before the refusals are asserted.
#[test]
fn a_plain_privacy_check_takes_no_configured_private_allowance() {
    let mut outputs: Vec<(String, Output)> = ["staged", "message", "history"]
        .iter()
        .map(|how| {
            let env = Env::new();
            env.configure("", "alice\n", "", Some("[host.alpha]\n"));
            let repo = checkout(&env, "plain", false, "");
            (format!("plain {how}"), run_privacy(&env, &repo, how, &detail(), &[]))
        })
        .collect();
    let env = Env::new();
    env.configure("", "alice\n", "", Some("[host.alpha]\n"));
    let repo = checkout(&env, "hooked", false, "");
    let o = env.lords(&repo, &["hooks", "install"]);
    assert!(o.status.success(), "setup: {}", text(&o));
    let before = env.git(&repo, &["rev-parse", "HEAD"]).stdout;
    write(&repo.join("notes.md"), &format!("{}\n", detail()));
    env.ok_git(&repo, &["add", "notes.md"]);
    outputs.push((
        "the installed plain hook".into(),
        env.git(&repo, &["commit", "-q", "-m", "notes"]),
    ));
    let let_through: Vec<_> = outputs
        .iter()
        .filter(|(_, o)| o.status.success())
        .map(|(row, o)| format!("{row}: {}", text(o)))
        .collect();
    assert!(let_through.is_empty(), "refused in every row, but let through: {let_through:#?}");
    for (row, o) in &outputs {
        assert!(text(o).contains("path: `alice`"), "{row}: names the hit: {}", text(o));
    }
    assert_eq!(env.git(&repo, &["rev-parse", "HEAD"]).stdout, before, "HEAD is as it was");

    let env = Env::new();
    env.configure("alice\n", "", "", Some("[host.alpha]\n"));
    let repo = checkout(&env, "plain", false, "");
    let o = run_privacy(&env, &repo, "staged", &detail(), &[]);
    assert!(o.status.success(), "the configured public allow-list still accepts it: {}", text(&o));
}

// Spec (D28 r6, R6-F02: "With no private stack it can pass without saying deny/fleet were not checked ... render the
// standard privacy-scope findings/notes on this branch and add one output check"; design note privacy-own-allow,
// attack item 16): `check --public --by-stack` in a checkout with no stack, and no private stack configured, says that
// no private stack was there.
#[test]
fn a_public_check_without_a_stack_says_what_it_didnt_check() {
    let env = Env::new();
    let elsewhere = env.repo("elsewhere", &[("stack/stack.toml", PUBLIC)]);
    let cfg = format!("public = {}\n", elsewhere.join("stack").display());
    write(&env.path("home/.lords-command/config.txt"), &cfg);
    let repo = checkout(&env, "repo", false, "");
    let o = env.lords(&repo, &["check", "--public", "--by-stack"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("no private stack"), "says it: {}", text(&o));
}

// Spec (the self-service audit, journey 7: "provide a read-only command that reports effective policy/hook chain";
// design note privacy-own-allow, attack items 12 and 15): `privacy --explain --by-stack` reports what a hook here
// checks, from the same composition. In a public checkout with a configured private stack: the whole policy, the
// private deny-list with its count, its own allow-list, and neither configured allow-list; in a private checkout,
// secrets only; in one whose stack can't be told, why. It prints no deny term and no allowed text.
#[test]
fn explain_says_what_a_privacy_hook_here_checks() {
    let env = Env::new();
    let terms = ["allowedpublicq", "allowedprivateq", "deniedtermq", "fleethostq", "allowedownq"];
    env.configure(
        &format!("{}\n", terms[0]),
        &format!("{}\n", terms[1]),
        &format!("{}\n", terms[2]),
        Some(&format!("[host.{}]\n", terms[3])),
    );
    let repo = checkout(&env, "repo", true, &format!("{}\n", terms[4]));
    let o = env.lords(&repo, &["privacy", "--explain", "--by-stack"]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    let lines = |p: &str| out.lines().filter(|l| l.starts_with(p)).collect::<Vec<_>>();
    assert!(out.contains("this checkout's stack: public"), "{out}");
    assert!(out.contains("mode: the whole policy"), "{out}");
    let allow = lines("allow-list: ");
    let own = |l: &&str| l.contains("repo") && l.contains("allow.txt") && l.contains("(1 ");
    assert!(allow.len() == 1 && allow.iter().all(own), "only its own allow-list: {out}");
    let deny = lines("deny-list: ");
    assert!(
        deny.len() == 1 && deny[0].contains("privstack") && deny[0].contains("(1 "),
        "the private deny-list: {out}"
    );
    let fleet = lines("fleet: ");
    assert!(
        fleet.len() == 1 && fleet[0].contains("fleet.toml") && fleet[0].contains("(1 "),
        "the fleet hosts: {out}"
    );
    assert!(!out.contains("elsewhere"), "the configured public stack adds nothing: {out}");
    for term in terms {
        assert!(!out.contains(term), "prints no term ({term}): {out}");
    }

    let private = env.repo("private", &[("stack.toml", PRIVATE)]);
    let o = env.lords(&private, &["privacy", "--explain", "--by-stack"]);
    assert!(o.status.success() && text(&o).contains("mode: secrets only"), "{}", text(&o));

    let unknown = env.repo("unknown", &[("stack.toml", PRIVATE), ("docs/stack.toml", PUBLIC)]);
    let o = env.lords(&unknown, &["privacy", "--explain", "--by-stack"]);
    let out = text(&o);
    assert!(
        o.status.success() && out.contains("can't be told") && out.contains("docs/stack.toml"),
        "{out}"
    );
}

// Spec (package review privownallow r1, R1-01: "the ordinary pre-commit hook contains `touch explain-mutated.txt` ...
// a read-only explanation must leave it unchanged"; the master, 00:23: "`--explain` doesn't probe live hooks. It
// reports the hook chain as 'not verified here' and names `lords check` as the command that probes"; design note
// privacy-own-allow, r1 attack item 17; this replaces r0's hook-chain test, whose probe the master took out): with an
// ordinary pre-commit hook that makes a file, `privacy --explain` runs no hook, so the checkout's status and HEAD are as
// before and the file isn't made, and it says the hooks are not verified here, naming `lords check`.
#[test]
fn explain_runs_no_hook_and_changes_nothing() {
    let env = Env::new();
    let repo = checkout(&env, "repo", true, "");
    let hook = repo.join(".git/hooks/pre-commit");
    write(&hook, "#!/bin/sh\ntouch explain-mutated.txt\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let state = || {
        let s = env.git(&repo, &["status", "--porcelain", "--ignored"]);
        let h = env.git(&repo, &["rev-parse", "HEAD"]);
        (text(&s), text(&h))
    };
    let before = state();
    let o = env.lords(&repo, &["privacy", "--explain"]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(!repo.join("explain-mutated.txt").exists(), "no hook ran: {out}");
    assert_eq!(state(), before, "the checkout is as it was");
    assert!(out.contains("not verified here") && out.contains("lords check"), "{out}");
}

// Spec (R1-03: "Plain `privacy --explain` should model the stack hook policy (`--by-stack`) unless `--secrets-only` is
// given ... Test it in a private and a public stack checkout, with foreign allowances"; design note privacy-own-allow,
// r1 attack items 22 and 23): with stack hooks installed, plain `privacy --explain` says secrets only in a private stack
// checkout, and in a public one lists only its own allow-list, with both configured ones there.
#[test]
fn plain_explain_models_the_stack_hooks() {
    let env = Env::new();
    env.configure("alice\n", "alice\n", "", Some("[host.alpha]\n"));
    let private = env.repo("private", &[("stack.toml", PRIVATE)]);
    let public = checkout(&env, "public", true, "carol\n");
    for repo in [&private, &public] {
        let o = env.lords(repo, &["hooks", "install"]);
        assert!(o.status.success(), "setup: {}", text(&o));
    }
    let o = env.lords(&private, &["privacy", "--explain"]);
    assert!(o.status.success() && text(&o).contains("mode: secrets only"), "{}", text(&o));
    let o = env.lords(&public, &["privacy", "--explain"]);
    let out = text(&o);
    assert!(o.status.success() && out.contains("mode: the whole policy"), "{out}");
    let allow: Vec<_> = out.lines().filter(|l| l.starts_with("allow-list: ")).collect();
    assert!(allow.len() == 1 && allow[0].contains("public"), "only its own allow-list: {out}");
    assert!(!out.contains("elsewhere") && !out.contains("privstack"), "{out}");
}

// Spec (R1-02: "a malformed fleet file, a malformed private manifest or an invalid visibility value prints the offending
// text on `--explain`'s stderr ... Tests: those three inputs with a sentinel term, absent from both stdout and stderr";
// "Keep refusal and Unknown as they are"; design note privacy-own-allow, r1 attack items 19-21): a fleet config that
// isn't TOML and a private manifest that isn't either refuse `--explain`, naming its role, not its path; a checkout manifest whose
// visibility is no visibility is a stack that can't be told; and the sentinel each holds appears in neither stream.
#[test]
fn explain_prints_no_content_from_a_broken_file() {
    let sentinel = "sentinelvalueq";
    let fleet = Env::new();
    fleet.configure("", "", "", Some(&format!("[host.{sentinel}\n")));
    let manifest = Env::new();
    manifest.configure("", "", "", Some("[host.alpha]\n"));
    let bad = format!("{PRIVATE}{sentinel} = [\n");
    write(&manifest.path("privstack/stack.toml"), &bad);
    let visibility = Env::new();
    // Named by the flag or the file's role, never by its path (the master, 9 Nov 02:36: "Load errors name the flag and
    // the kind of failure, never the path"; this row named the path until then).
    let rows = [
        ("fleet", &fleet, "fleet config", "fleet.toml"),
        ("manifest", &manifest, "--private-stack", "privstack"),
    ];
    for (row, env, names, path) in rows {
        let repo = checkout(env, "repo", true, "");
        let o = env.lords(&repo, &["privacy", "--explain"]);
        let out = text(&o);
        assert!(
            !out.contains(sentinel) && !out.contains(path),
            "{row}: no content, no path: {out}"
        );
        assert!(
            !o.status.success() && out.contains(names),
            "{row}: refused, naming {names}: {out}"
        );
    }
    let manifest = format!("name = \"x\"\nvisibility = \"{sentinel}\"\n");
    let repo = visibility.repo("repo", &[("stack.toml", &manifest)]);
    let o = visibility.lords(&repo, &["privacy", "--explain"]);
    let out = text(&o);
    assert!(!out.contains(sentinel), "visibility: no content: {out}");
    assert!(o.status.success() && out.contains("can't be told"), "visibility: Unknown: {out}");
}

// Spec (sol-loop r1, S1: "With `.git/config` holding `core.bare = sentinelvalueq`, Git's diagnostic quotes the value;
// `checkout_stack` keeps it ... `a.done()` also echoes unexpected arguments. Make explain's discovery and argument
// diagnostics content-free (category plus permitted path), with sentinel tests for both"; design note
// privacy-own-allow, attack items 28 and 29): where git's own error quotes a config value, `privacy --explain` says the
// stack can't be told and prints no sentinel; an argument it doesn't take is refused without being echoed.
#[test]
fn explain_echoes_neither_git_nor_an_argument() {
    let sentinel = "sentinelvalueq";
    let env = Env::new();
    let repo = checkout(&env, "repo", true, "");
    let config = repo.join(".git/config");
    let before = fs::read_to_string(&config).unwrap();
    write(&config, &format!("{before}[core]\n\tbare = {sentinel}\n"));
    let o = env.lords(&repo, &["privacy", "--explain"]);
    let out = text(&o);
    assert!(!out.contains(sentinel), "git's words: no content: {out}");
    assert!(o.status.success() && out.contains("can't be told"), "git's words: Unknown: {out}");

    let env = Env::new();
    let repo = checkout(&env, "repo", true, "");
    let o = env.lords(&repo, &["privacy", "--explain", sentinel]);
    let out = text(&o);
    assert!(!out.contains(sentinel), "an argument: not echoed: {out}");
    assert!(!o.status.success(), "an argument: refused: {out}");
}

// Spec (sol-loop r1, S2: "`public_rules` keeps `.with_private(private)`, which imports the private allow-list. `check
// --public` reaches it ... use restriction-only composition at all three, with a public-check refusal test"; design
// note privacy-own-allow, attack item 31): `check --public` without `--by-stack`, in a public stack repo whose tree holds
// a user path that only the configured private stack's allow-list would accept, refuses, naming the hit.
#[test]
fn a_public_check_takes_no_configured_private_allowance() {
    let env = Env::new();
    env.configure("", "alice\n", "", Some("[host.alpha]\n"));
    let repo =
        env.repo("repo", &[("stack/stack.toml", PUBLIC), ("notes.md", &format!("{}\n", detail()))]);
    let o = env.lords(&repo, &["check", "--public"]);
    assert!(!o.status.success(), "refused: {}", text(&o));
    assert!(text(&o).contains("path: `alice`"), "names the hit: {}", text(&o));
}

// Spec (sol-loop r2, S1: "`privacy --message --explain --sentinelvalueq` falls through to ordinary handling, and
// `Args::done` echoes `--sentinelvalueq`. Recognise explain mode before consuming any option's value ... add that
// malformed-argument row asserting refusal with the sentinel absent from both stdout and stderr. Better still, make
// `Args::done` itself never echo an argument's text"; design note privacy-own-allow, attack items 35 and 36): that
// command line, and any command's leftover argument, are refused without the argument's text. Every row runs before
// the refusals are asserted.
#[test]
fn no_argument_is_echoed() {
    let sentinel = "sentinelvalueq";
    let option = format!("--{sentinel}");
    let env = Env::new();
    let repo = checkout(&env, "repo", true, "");
    let rows: [(&str, Vec<&str>); 2] = [
        ("privacy --message --explain", vec!["privacy", "--message", "--explain", &option]),
        ("check", vec!["check", &option]),
    ];
    let outputs: Vec<_> = rows.iter().map(|(row, args)| (*row, env.lords(&repo, args))).collect();
    for (row, o) in &outputs {
        assert!(!text(o).contains(sentinel), "{row}: not echoed: {}", text(o));
    }
    for (row, o) in &outputs {
        assert!(!o.status.success(), "{row}: refused: {}", text(o));
    }
}

// Spec (sol-loop r3, S1: "`Args::value` takes another option as a value. With `lords privacy --private-stack --explain
// --sentinelvalueq`, the sentinel becomes the path, and the load-error formatter prints it verbatim ... Same class, fix
// it at its owner: no error prints a user-supplied value ... `sync --stage SENTINEL` ... invalid panel ports ... Load
// errors name the flag and the kind of failure, never the path"; design note privacy-own-allow, attack items 39-41 and
// 50): on the surfaces whose output can travel, each of these is refused, and the value given appears in neither
// stream: an option where a value goes, a port, a command, and a private stack's path that isn't there. Every row runs
// before anything is asserted, and every row that echoes is listed.
#[test]
fn no_error_repeats_a_value() {
    let sentinel = "sentinelvalueq";
    let option = format!("--{sentinel}");
    let env = Env::new();
    let repo = checkout(&env, "repo", true, "");
    // Only the surfaces whose output can travel (the master, 9 Nov 05:31, the invariant re-scoped; design note
    // privacy-own-allow, attack item 50): r3's `sync` rows are out of scope, and may name what they were given.
    let rows: [(&str, Vec<&str>); 4] = [
        ("an option as a value", vec!["privacy", "--private-stack", "--explain", &option]),
        ("panel serve --port", vec!["panel", "serve", "--port", sentinel]),
        ("a command", vec![sentinel]),
        ("check --private-stack", vec!["check", "--private-stack", sentinel]),
    ];
    let outputs: Vec<_> = rows.iter().map(|(row, args)| (*row, env.lords(&repo, args))).collect();
    let echoed: Vec<_> = outputs
        .iter()
        .filter(|(_, o)| text(o).contains(sentinel))
        .map(|(row, o)| format!("{row}: {}", text(o)))
        .collect();
    assert!(echoed.is_empty(), "no value repeated, but: {echoed:#?}");
    for (row, o) in &outputs {
        assert!(!o.status.success(), "{row}: refused: {}", text(o));
    }
}

// Spec (sol-loop r4, S8: "A private stack's manifest content can reach a parser diagnostic. Render a load or parse
// failure as 'the private stack's manifest doesn't parse', with no parser text"; design note privacy-own-allow, attack
// item 47): with a private stack whose stack.toml holds the sentinel inside a longer word, `check --private-stack`, a
// scan with it configured and a promotion with it configured are each refused, say the private stack's manifest
// doesn't parse, and repeat none of it. Every row runs before anything is asserted.
#[test]
fn a_private_manifest_that_doesnt_parse_is_named_not_quoted() {
    let sentinel = "sentinelvalueq";
    let env = Env::new();
    env.configure("", "", "", Some("[host.alpha]\n"));
    write(&env.path("privstack/stack.toml"), &format!("{PRIVATE}{sentinel}suffix = [\n"));
    let repo = checkout(&env, "repo", true, "");
    write(&repo.join("notes.md"), "clean\n");
    env.ok_git(&repo, &["add", "notes.md"]);
    let private = env.path("privstack");
    let private = private.to_string_lossy();
    let rows: [(&str, Vec<&str>); 3] = [
        ("check --private-stack", vec!["check", "--private-stack", &private]),
        ("privacy --staged", vec!["privacy", "--staged"]),
        ("promote", vec!["promote", "rule.a"]),
    ];
    let outputs: Vec<_> = rows.iter().map(|(row, args)| (*row, env.lords(&repo, args))).collect();
    let quoted: Vec<_> = outputs
        .iter()
        .filter(|(_, o)| text(o).contains(sentinel))
        .map(|(row, o)| format!("{row}: {}", text(o)))
        .collect();
    assert!(quoted.is_empty(), "no manifest text, but: {quoted:#?}");
    for (row, o) in &outputs {
        assert!(!o.status.success(), "{row}: refused: {}", text(o));
        assert!(
            text(o).contains("the private stack's manifest doesn't parse"),
            "{row}: {}",
            text(o)
        );
    }
}

/// `prefix` and then what can't be Unicode: a byte no UTF-8 holds on Unix, a lone surrogate on Windows.
fn not_unicode(prefix: &str) -> std::ffi::OsString {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let mut bytes = prefix.as_bytes().to_vec();
        bytes.push(0xff);
        std::ffi::OsString::from_vec(bytes)
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        let mut wide: Vec<u16> = prefix.encode_utf16().collect();
        wide.push(0xD800);
        std::ffi::OsString::from_wide(&wide)
    }
}

// Spec (sol-loop r4, S10: "Read arguments with `args_os()`, and refuse bad Unicode with a content-free error, before any
// guard or explain handling"; design note privacy-own-allow, attack item 48): an argument that isn't Unicode, holding
// the sentinel, is refused under `privacy --explain` (exit 2) and noted by `guard rm`, which lets the call through
// (exit 0), and neither panics nor repeats it.
#[test]
fn an_argument_that_isnt_unicode_is_refused_unrepeated() {
    let sentinel = "sentinelvalueq";
    let env = Env::new();
    let repo = checkout(&env, "repo", true, "");
    let bad = not_unicode(sentinel);
    let run = |words: &[&str]| {
        env.cmd(env!("CARGO_BIN_EXE_lords"), &repo).args(words).arg(&bad).output().unwrap()
    };
    let rows = [
        ("privacy --explain", run(&["privacy", "--explain"]), 2),
        ("guard rm", run(&["guard", "rm"]), 0),
    ];
    for (row, o, _) in &rows {
        assert!(!text(o).contains(sentinel), "{row}: not repeated: {}", text(o));
    }
    for (row, o, code) in &rows {
        assert_eq!(o.status.code(), Some(*code), "{row}: refused, no panic: {}", text(o));
    }
}

// Spec (sol-loop r4, S1 for the guard at run time: "errors on those surfaces render as typed diagnostics, a category
// plus our own flag or role name only, never a wrapped message"; design note privacy-own-allow, attack item 49): the
// helper guard, given a helper start in a session whose `CLAUDE_PID` is the sentinel, lets the call through with a note
// that T3's start can't be told, and repeats nothing of what it was given.
#[test]
fn the_guard_notes_a_failure_without_its_text() {
    use std::io::Write;
    let sentinel = "sentinelvalueq";
    let env = Env::new();
    let repo = checkout(&env, "repo", true, "");
    let mut child = env
        .cmd(env!("CARGO_BIN_EXE_lords"), &repo)
        .args(["guard", "helper"])
        .env("T3CODE_THREAD_ID", "thread")
        .env("CLAUDE_PID", sentinel)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(br#"{"tool_name":"Agent"}"#).unwrap();
    let o = child.wait_with_output().unwrap();
    assert!(!text(&o).contains(sentinel), "not repeated: {}", text(&o));
    assert!(
        o.status.success() && text(&o).contains("can't be told"),
        "noted, let through: {}",
        text(&o)
    );
}

// Spec (sol-loop r5, S11: "`check` bypasses `Fail` through drift findings. A malformed ledger exposes its TOML
// diagnostic; a valid private manifest containing an unknown target adapter exposes that target's name and adapter.
// Raw import failures also enter findings through `plan.notes`"; the master, 06:50: "All three get typed categories at
// the boundary"; design note privacy-own-allow, attack item 55): a full `check` with a ledger that doesn't parse, with
// a private target whose name and adapter are no harness's, and with a private repo whose name is in a plan note,
// repeats none of that text on either stream. Every row runs before anything is asserted.
#[test]
fn checks_drift_names_no_plan_failure_text() {
    let sentinel = "sentinelvalueq";
    let target = format!(
        "[[targets]]\nname = \"{sentinel}name\"\nadapter = \"{sentinel}adapter\"\nkind = \"settings\"\npath = \"~/.t/settings.json\"\n"
    );
    let rows: [(&str, String, String); 3] = [
        ("a ledger that doesn't parse", String::new(), format!("{sentinel} = [\n")),
        ("a target no harness is", target, String::new()),
        ("a repo in a plan note", format!("[repos.{sentinel}repo]\n"), String::new()),
    ];
    let outputs: Vec<_> = rows
        .iter()
        .map(|(row, extra, ledger)| {
            let env = Env::new();
            env.configure("", "", "", Some("[host.alpha]\n"));
            let manifest = env.path("privstack/stack.toml");
            let before = fs::read_to_string(&manifest).unwrap();
            write(&manifest, &format!("{before}{extra}"));
            if !ledger.is_empty() {
                write(&env.path("home/.lords-command/state/ledger.toml"), ledger);
            }
            let repo = checkout(&env, "repo", false, "");
            (*row, env.lords(&repo, &["check"]))
        })
        .collect();
    let repeated: Vec<_> = outputs
        .iter()
        .filter(|(_, o)| text(o).contains(sentinel))
        .map(|(row, o)| format!("{row}: {}", text(o)))
        .collect();
    assert!(repeated.is_empty(), "no plan text, but: {repeated:#?}");
}

// Spec (sol-loop r5, S13: "discovery marks filesystem errors as trusted `Untold::said`. A locked or unreadable checkout
// manifest therefore puts the complete IO diagnostic into `brief` ... Fix: categorise disk failures before
// constructing `Untold`"; design note privacy-own-allow, attack item 56): in a checkout whose dir holds the sentinel and
// whose `stack/stack.toml` on disk is a dir, `privacy --explain` says the stack can't be told and repeats no path.
#[test]
fn a_manifest_that_cant_be_read_is_named_without_its_path() {
    let sentinel = "sentinelvalueq";
    let env = Env::new();
    let repo = checkout(&env, &format!("{sentinel}-repo"), true, "");
    let manifest = repo.join("stack/stack.toml");
    fs::remove_file(&manifest).unwrap();
    fs::create_dir(&manifest).unwrap();
    let o = env.lords(&repo, &["privacy", "--explain"]);
    let out = text(&o);
    assert!(!out.contains(sentinel), "no path: {out}");
    assert!(o.status.success() && out.contains("can't be told"), "{out}");
}
