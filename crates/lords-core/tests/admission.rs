//! The stores' commit check of the starts one hook gives (gap 11, design note `2026-11-08-lords-gap11-headroom.md`
//! §0b–§0e). I1: a commit is refused when a start is past the limit and longer than before, per place and start. I2:
//! the other store is read only once no `index.lock` is there and its status is clean, its tree after that status, and
//! one tree of it serves both sides. Each case runs on two real stores: a public repo whose stack is `stack/`, and a
//! private repo that is its stack, both with git.

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;
use common::{entry, git, isolate_git, Fixture};
use lords_core::admission::{self, AdmitStep, Count, Inputs, Outcome};
use lords_core::capture::Read;
use lords_core::stack::{Stacks, Store};
use lords_core::sync::CHILD_START_CHARS as LIMIT;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

const TODAY: &str = "2026-11-08";

/// Two stores. The private one declares repos `demo` and `other` (no checkouts: a start needs none) and the roles
/// coder and reviewer; the public one has a rule for every role.
struct Stores {
    f: Fixture,
}

impl Stores {
    fn new() -> Stores {
        let f = Fixture::new();
        let roles = "roles = [\"coder\", \"reviewer\"]\n";
        f.write(
            "public/stack/stack.toml",
            &format!("name = \"pub\"\nvisibility = \"public\"\n{roles}"),
        );
        f.write(
            "public/stack/global/rules/all.md",
            &entry("rule.all", "global", "public", "", "For everyone.\n"),
        );
        f.write(
            "private/stack.toml",
            &format!(
                "name = \"priv\"\nvisibility = \"private\"\n{roles}[repos.demo]\n[repos.other]\n"
            ),
        );
        for repo in ["public", "private"] {
            git(&f.path(repo), &["init", "-q"]);
            git(&f.path(repo), &["add", "-A"]);
            git(&f.path(repo), &["commit", "-q", "-m", "fixture"]);
        }
        Stores { f }
    }

    fn repo(&self, store: Store) -> PathBuf {
        self.f.path(match store {
            Store::Public => "public",
            Store::Private => "private",
        })
    }

    fn stack(&self, store: Store) -> PathBuf {
        match store {
            Store::Public => self.f.path("public/stack"),
            Store::Private => self.f.path("private"),
        }
    }

    /// Writes the coder-only rule `id` in `store`, scoped to `scope`, whose body is `body`.
    fn rule(&self, store: Store, id: &str, scope: &str, body: &str) {
        let (vis, dir) = match store {
            Store::Public => ("public", "public/stack"),
            Store::Private => ("private", "private"),
        };
        let layer = match scope.split_once(':') {
            Some(("repo", r)) => format!("repo/{r}"),
            _ => "global".into(),
        };
        let name = id.trim_start_matches("rule.");
        self.f.write(
            &format!("{dir}/{layer}/rules/{name}.md"),
            &entry(id, scope, vis, "roles: [coder]\n", body),
        );
    }

    fn stage(&self, store: Store) {
        git(&self.repo(store), &["add", "-A"]);
    }

    fn commit(&self, store: Store, message: &str) {
        self.stage(store);
        git(&self.repo(store), &["commit", "-q", "-m", message]);
    }

    /// The stores as their checkouts hold them now.
    fn on_disk(&self) -> Stacks {
        Stacks::load(&self.stack(Store::Public), Some(&self.stack(Store::Private))).unwrap()
    }

    /// `place`'s `file` as the checkouts would compose it now, in characters.
    fn chars(&self, place: &str, file: &str) -> usize {
        let s = self.on_disk();
        let (_, layers) = lords_core::sync::declared_places(&s)
            .into_iter()
            .find(|(p, _)| p == place)
            .unwrap_or_else(|| panic!("no place {place}"));
        let home = self.f.path("home");
        let start = lords_core::sync::hook_starts(&s, layers, &home, TODAY)
            .into_iter()
            .find(|h| h.file == file)
            .unwrap_or_else(|| panic!("no {file}"));
        start.chars()
    }

    /// Writes rule `id`'s one-line body so that `start-coder.md` of `place` is exactly `n` characters long, in the
    /// checkout (staged or not is the caller's).
    fn sized(&self, store: Store, id: &str, scope: &str, place: &str, n: usize) {
        self.rule(store, id, scope, "a\n");
        let base = self.chars(place, "start-coder.md");
        assert!(n >= base, "{n} < {base}");
        self.rule(store, id, scope, &format!("{}\n", "a".repeat(n - base + 1)));
        assert_eq!(self.chars(place, "start-coder.md"), n);
    }

    fn inputs(&self, store: Store) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        (
            self.repo(store),
            self.stack(Store::Public),
            self.stack(Store::Private),
            self.f.path("home"),
        )
    }

    fn admit(&self, store: Store) -> Result<Outcome, String> {
        self.admit_between(store, &mut |_| {})
    }

    fn admit_between(
        &self,
        store: Store,
        between: &mut dyn FnMut(AdmitStep),
    ) -> Result<Outcome, String> {
        isolate_git();
        let (checkout, public, private, home) = self.inputs(store);
        let i = Inputs {
            checkout: &checkout,
            stack: if store == Store::Public { "stack" } else { "." },
            store,
            public: &public,
            private: &private,
            home: &home,
            today: TODAY,
            limit: Duration::from_secs(60),
        };
        admission::admit_between(&i, between)
    }
}

fn counted(o: &Outcome) -> &[Count] {
    match o {
        Outcome::Admitted(c) | Outcome::Refused(c) => c,
        other => panic!("not counted: {other:?}"),
    }
}

fn coder<'a>(o: &'a Outcome, place: &str) -> &'a Count {
    counted(o).iter().find(|c| c.place == place && c.file == "start-coder.md").unwrap()
}

fn refused(o: &Outcome) -> bool {
    matches!(o, Outcome::Refused(_))
}

// Spec (design check r3, "The grow/cross rule"): 9,499 → 9,500 allowed; 9,500 → 9,501 refused; 9,700 → 9,600 allowed;
// 9,700 → 9,700 allowed; 9,700 → 9,701 refused; a start that wasn't there counts 0.
#[test]
fn the_predicate_refuses_growth_past_the_limit_and_nothing_else() {
    assert_eq!(LIMIT, 9_500);
    for (old, new, refuses) in [
        (9_499, 9_500, false),
        (9_500, 9_501, true),
        (9_700, 9_600, false),
        (9_700, 9_700, false),
        (9_700, 9_701, true),
        (9_600, 9_500, false),
        (0, 9_501, true),
        (0, 9_500, false),
    ] {
        assert_eq!(admission::refuses(new, old), refuses, "{old} → {new}");
    }
}

// Spec (§0b.3 step 6, I1): a staged rule that takes a fitting start past the limit is refused; one that reaches it
// exactly is admitted.
#[test]
fn a_commit_that_takes_a_start_over_the_limit_is_refused() {
    let s = Stores::new();
    s.sized(Store::Private, "rule.p", "global", "repo demo", LIMIT);
    s.stage(Store::Private);
    let o = s.admit(Store::Private).unwrap();
    assert!(!refused(&o), "{o:?}");
    assert_eq!(coder(&o, "repo demo").new, LIMIT);
    s.sized(Store::Private, "rule.p", "global", "repo demo", LIMIT + 1);
    s.stage(Store::Private);
    let o = s.admit(Store::Private).unwrap();
    assert!(refused(&o), "{o:?}");
    assert_eq!((coder(&o, "repo demo").new, coder(&o, "repo demo").old), (LIMIT + 1, Some(LIMIT)));
}

// Spec (I1, the master's 14:42 direction: "a commit that shrinks an over-limit start … passes"): with HEAD's start past
// the limit, growing it is refused, keeping its size or shrinking it is admitted.
#[test]
fn a_start_past_the_limit_may_shrink_or_stay_but_not_grow() {
    let s = Stores::new();
    s.sized(Store::Public, "rule.q", "global", "repo demo", LIMIT + 200);
    s.commit(Store::Public, "over the limit already");
    for (n, refuses) in [(LIMIT + 201, true), (LIMIT + 200, false), (LIMIT + 100, false)] {
        s.sized(Store::Public, "rule.q", "global", "repo demo", n);
        s.stage(Store::Public);
        let o = s.admit(Store::Public).unwrap();
        assert_eq!(refused(&o), refuses, "{n}: {o:?}");
        assert_eq!(coder(&o, "repo demo").old, Some(LIMIT + 200));
    }
}

// Spec (I1: "Growth in one (p, s) is never paid for by shrinking another"; r3 test matrix): two places, one start
// grown past the limit while another, past it too, shrinks: refused.
#[test]
fn growth_in_one_place_is_not_paid_for_by_another() {
    let s = Stores::new();
    s.sized(Store::Private, "rule.d", "repo:demo", "repo demo", LIMIT + 100);
    s.sized(Store::Private, "rule.o", "repo:other", "repo other", LIMIT + 300);
    s.commit(Store::Private, "both past the limit");
    s.sized(Store::Private, "rule.d", "repo:demo", "repo demo", LIMIT + 101);
    s.sized(Store::Private, "rule.o", "repo:other", "repo other", LIMIT + 50);
    s.stage(Store::Private);
    let o = s.admit(Store::Private).unwrap();
    assert!(refused(&o), "{o:?}");
    assert!(coder(&o, "repo demo").refused() && !coder(&o, "repo other").refused(), "{o:?}");
}

// Spec (I1: "A (p, s) absent from H counts 0"): a role declared in the commit whose start is past the limit, and a
// place declared in it with one, are refused; at the limit they are admitted.
#[test]
fn a_new_role_or_place_counts_from_nothing() {
    let s = Stores::new();
    s.sized(Store::Public, "rule.q", "global", "repo demo", LIMIT + 1);
    s.commit(Store::Public, "coder's start past the limit, no tester yet");
    let roles = "roles = [\"coder\", \"reviewer\", \"tester\"]\n";
    s.f.write(
        "public/stack/stack.toml",
        &format!("name = \"pub\"\nvisibility = \"public\"\n{roles}"),
    );
    s.f.write(
        "public/stack/global/rules/t.md",
        &entry(
            "rule.t",
            "global",
            "public",
            "roles: [tester]\n",
            &format!("{}\n", "t".repeat(LIMIT)),
        ),
    );
    s.stage(Store::Public);
    let o = s.admit(Store::Public).unwrap();
    let tester =
        counted(&o).iter().find(|c| c.place == "repo demo" && c.file == "start-tester.md").unwrap();
    assert!(tester.new > LIMIT && tester.old == Some(0) && tester.refused(), "{o:?}");
    assert!(!coder(&o, "repo demo").refused(), "coder's is unchanged: {o:?}");

    let s = Stores::new();
    s.sized(Store::Public, "rule.q", "global", "repo demo", LIMIT + 1);
    s.commit(Store::Public, "past the limit in every place");
    s.f.write(
        "private/stack.toml",
        "name = \"priv\"\nvisibility = \"private\"\nroles = [\"coder\", \"reviewer\"]\n[repos.demo]\n[repos.other]\n[repos.third]\n",
    );
    s.stage(Store::Private);
    let o = s.admit(Store::Private).unwrap();
    assert!(refused(&o) && coder(&o, "repo third").old == Some(0), "{o:?}");
    assert!(!coder(&o, "repo demo").refused(), "{o:?}");
}

// Spec (§0b.3 step 4, r1 F3: "big staged, short in the working tree, so the staged one is refused"): what the commit
// holds is its index, not its checkout.
#[test]
fn the_commit_is_what_its_index_holds() {
    let s = Stores::new();
    s.sized(Store::Private, "rule.p", "global", "repo demo", LIMIT + 1);
    s.stage(Store::Private);
    s.rule(Store::Private, "rule.p", "global", "short\n");
    let o = s.admit(Store::Private).unwrap();
    assert!(refused(&o), "{o:?}");
    assert_eq!(coder(&o, "repo demo").new, LIMIT + 1);
}

// Spec (§0b.1, the no-op skip: "a code commit … isn't tied to whether the private store is clean"): a commit that
// changes nothing in the stack dir is admitted with the other store's checkout dirty, unread.
#[test]
fn a_commit_outside_the_stack_is_admitted_unread() {
    let s = Stores::new();
    s.f.write("public/README.md", "code, not stack\n");
    s.stage(Store::Public);
    s.rule(Store::Private, "rule.p", "global", "a draft\n");
    let mut reads = Vec::new();
    let o = s.admit_between(Store::Public, &mut |step| reads.push(step)).unwrap();
    assert_eq!(o, Outcome::Unchanged);
    assert!(reads.is_empty(), "{reads:?}");
}

// Spec (§0b.3 step 3, §0c.3): the other store with work not committed in its stack (edited, staged, untracked or
// ignored) is unclean, and named with its paths; so is one with an `index.lock`, named with the lock's path and how
// to clear it, until that lock is gone (D2). A store that can't be read is an error.
#[test]
fn the_other_store_must_be_clean_and_unlocked() {
    let s = Stores::new();
    s.rule(Store::Public, "rule.q", "global", "grows\n");
    s.stage(Store::Public);
    let unclean = |o: Outcome| match o {
        Outcome::Unclean(why) => why,
        other => panic!("not unclean: {other:?}"),
    };
    s.f.write("private/global/rules/new.md", "untracked\n");
    assert!(unclean(s.admit(Store::Public).unwrap()).contains("new.md"));
    s.stage(Store::Private);
    assert!(unclean(s.admit(Store::Public).unwrap()).contains("new.md"), "staged");
    git(&s.repo(Store::Private), &["rm", "-q", "--cached", "global/rules/new.md"]);
    fs::remove_file(s.f.path("private/global/rules/new.md")).unwrap();
    s.f.write("private/.gitignore", "*.tmp\n");
    s.commit(Store::Private, "ignore temp files");
    s.f.write("private/global/x.tmp", "ignored\n");
    assert!(unclean(s.admit(Store::Public).unwrap()).contains("x.tmp"), "ignored");
    fs::remove_file(s.f.path("private/global/x.tmp")).unwrap();

    let lock = s.f.path("private/.git/index.lock");
    fs::write(&lock, "").unwrap();
    let why = unclean(s.admit(Store::Public).unwrap());
    assert!(why.contains("index.lock exists") && why.contains("remove that one file"), "{why}");
    // Its resolved path, as git gives it (the temp dir's spelling may differ, its end doesn't).
    assert!(why.replace('\\', "/").contains("private/.git/index.lock"), "{why}");
    fs::remove_file(&lock).unwrap();
    assert!(matches!(s.admit(Store::Public).unwrap(), Outcome::Admitted(_)));

    fs::remove_dir_all(s.f.path("private/.git")).unwrap();
    assert!(s.admit(Store::Public).is_err());
}

// Spec (r3, F3 source-pair boundaries: "Already committed, not delivered: read companion HEAD … Candidate and baseline
// must both use Q1"): the other store's commit counts as soon as it is in, delivered or not.
#[test]
fn the_other_store_counts_as_committed_not_as_delivered() {
    let s = Stores::new();
    s.sized(Store::Public, "rule.q", "global", "repo demo", LIMIT - 100);
    s.commit(Store::Public, "close to the limit");
    s.rule(Store::Private, "rule.p", "global", &format!("{}\n", "p".repeat(150)));
    s.commit(Store::Private, "takes it over, committed, not delivered");
    s.rule(Store::Public, "rule.q2", "global", "a little more\n");
    s.stage(Store::Public);
    let o = s.admit(Store::Public).unwrap();
    assert!(refused(&o), "{o:?}");
    let c = coder(&o, "repo demo");
    assert_eq!(c.new, s.chars("repo demo", "start-coder.md"));
    assert!(c.old.unwrap() > LIMIT, "the baseline has the private commit too: {c:?}");
}

// Spec (I1, no HEAD: "H = W's HEAD stack tree, or empty when there's no HEAD"): a store's first commit counts from
// nothing, so a start past the limit is refused.
#[test]
fn a_first_commit_counts_from_nothing() {
    let s = Stores::new();
    fs::remove_dir_all(s.f.path("public/.git")).unwrap();
    git(&s.repo(Store::Public), &["init", "-q"]);
    s.sized(Store::Public, "rule.q", "global", "repo demo", LIMIT + 1);
    s.stage(Store::Public);
    let o = s.admit(Store::Public).unwrap();
    assert!(refused(&o), "{o:?}");
    assert_eq!(coder(&o, "repo demo").old, None);
}

// Spec (r3's F3 counterexample; design check r4, R4-F2: synchronize at the real status read, so a mutant that reads
// the other store's tree before its status keeps its old tree): both HEADs at 9,400; P1 (+80) staged in the public
// store, Q1 (+80) staged in the private one. Q1's check runs, and P1 is published just before its status read. A check
// that reads the status first sees P1's tree: 9,560, refused. The pair of HEADs never ends up past the limit through
// an admitted commit.
#[test]
fn the_other_store_is_read_after_its_clean_status() {
    let s = Stores::new();
    s.sized(Store::Public, "rule.q", "global", "repo demo", 9_400);
    s.commit(Store::Public, "9,400");
    let base = s.chars("repo demo", "start-coder.md");
    s.rule(Store::Public, "rule.p1", "global", &format!("{}\n", "1".repeat(80)));
    s.stage(Store::Public);
    let p1 = s.chars("repo demo", "start-coder.md") - base;
    s.rule(Store::Private, "rule.q1", "global", &format!("{}\n", "2".repeat(80)));
    s.stage(Store::Private);
    assert!(base + p1 <= LIMIT && base + 2 * p1 > LIMIT, "{base} {p1}");
    let public = s.repo(Store::Public);
    let o = s
        .admit_between(Store::Private, &mut |step| {
            if step == AdmitStep::Read(Read::Status) {
                git(&public, &["commit", "-q", "-m", "P1, published just before Q1's status read"]);
            }
        })
        .unwrap();
    assert!(refused(&o), "{o:?}");
    assert_eq!(coder(&o, "repo demo").new, s.chars("repo demo", "start-coder.md"));
}

// Spec (I2: "the second of two overlapping commits sees the first as uncommitted work and is refused"): P1 still staged
// in the public store when Q1's check reads it: Q1's check finds the public store unclean, naming P1's file.
#[test]
fn a_pending_commit_in_the_other_store_is_seen() {
    let s = Stores::new();
    s.rule(Store::Public, "rule.p1", "global", "pending\n");
    s.stage(Store::Public);
    s.rule(Store::Private, "rule.q1", "global", "mine\n");
    s.stage(Store::Private);
    match s.admit(Store::Private).unwrap() {
        Outcome::Unclean(why) => assert!(why.contains("p1.md"), "{why}"),
        other => panic!("{other:?}"),
    }
}

// Spec (§0b.3 step 5, "Type-level single capture"; design check r5, T5d: "a one-shot callback immediately before the
// captured blob read … can move live HEAD without changing the selected trees"): the other store's commit landing
// after its tree is resolved changes neither side. HEAD at 9,700, the candidate at 9,650, and a public commit that
// takes about 100 off lands just before the blob read: admitted, with old and new as the captured trees compose them.
// A check that read the other store again for its baseline would count old as about 9,600 and refuse.
#[test]
fn one_tree_of_the_other_store_serves_both_sides() {
    let s = Stores::new();
    s.rule(Store::Public, "rule.cut", "global", &format!("{}\n", "c".repeat(100)));
    s.commit(Store::Public, "a public rule the next commit removes");
    s.sized(Store::Private, "rule.p", "global", "repo demo", 9_700);
    s.commit(Store::Private, "9,700");
    s.sized(Store::Private, "rule.p", "global", "repo demo", 9_650);
    s.stage(Store::Private);
    let public = s.repo(Store::Public);
    let mut once = true;
    let o = s
        .admit_between(Store::Private, &mut |step| {
            if once && step == AdmitStep::Read(Read::Blobs) {
                once = false;
                git(&public, &["rm", "-q", "stack/global/rules/cut.md"]);
                git(&public, &["commit", "-q", "-m", "lands after the tree was resolved"]);
            }
        })
        .unwrap();
    assert!(!once, "the seam ran");
    assert!(!refused(&o), "{o:?}");
    assert_eq!((coder(&o, "repo demo").new, coder(&o, "repo demo").old), (9_650, Some(9_700)));
}

// Spec (§0d.2 I3 with §0b.3 step 6: "the start function … install and H3 both call"; r4 T2: "use child_context to
// measure child.md; deliverable is a Launched-start API"): what the check counts for a commit is what install writes
// from it and the role guard measures, for a role's start and the child's, with the role's own rule in it.
#[test]
fn the_check_counts_what_install_writes() {
    let s = Stores::new();
    let place = s.f.path("demo");
    s.f.write("demo/README.md", "a checkout\n");
    git(&place, &["init", "-q"]);
    git(&place, &["add", "-A"]);
    git(&place, &["commit", "-q", "-m", "a place"]);
    s.f.write(
        "private/stack.toml",
        &format!(
            "name = \"priv\"\nvisibility = \"private\"\nroles = [\"coder\", \"reviewer\"]\nchild_role = \"coder\"\n[repos.demo]\ncheckouts = ['{}']\nworktrees = false\n[repos.other]\n",
            place.display()
        ),
    );
    s.rule(Store::Private, "rule.p", "global", "Coder's own rule. Ünïcödé.\r\nSecond line.\n");
    s.stage(Store::Private);
    let o = s.admit(Store::Private).unwrap();
    s.commit(Store::Private, "installed next");
    let opts = lords_core::sync::Options { home: s.f.path("home"), ..Default::default() };
    lords_core::sync::import_and_apply(
        &s.stack(Store::Public),
        Some(&s.stack(Store::Private)),
        &opts,
    )
    .unwrap();
    let lords = place.join(".lords");
    let start = fs::read_to_string(lords.join("start-coder.md")).unwrap();
    assert!(
        start.contains("Coder's own rule. Ünïcödé.") && start.contains("Second line."),
        "{start}"
    );
    let given = lords_core::guard::deliverable(&lords, &start, "coder", None).unwrap();
    assert_eq!(coder(&o, "repo demo").new, given.chars().count());
    let child = fs::read_to_string(lords.join("child.md")).unwrap();
    let counted_child =
        counted(&o).iter().find(|c| c.place == "repo demo" && c.file == "child.md").unwrap();
    assert_eq!(counted_child.new, lords_core::guard::child_context(&child).chars().count());
}

/// A pre-commit hook that waits until `signal` exists, at most a minute: a commit held while its index lock is taken.
fn holding_hook(repo: &Path, signal: &Path) {
    let hook = format!(
        "#!/bin/sh\ni=0\nwhile [ ! -f '{}' ] && [ $i -lt 600 ]; do sleep 0.1; i=$((i+1)); done\n",
        signal.display().to_string().replace('\\', "/")
    );
    let at = repo.join(".git/hooks/pre-commit");
    fs::create_dir_all(at.parent().unwrap()).unwrap();
    fs::write(&at, hook).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&at, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

// Spec (§0c.2 T5e, D2: `commit -a` and `commit <paths>` hold the real index lock from preparing their index; restoring
// the working tree meanwhile hides the change from a status): a private commit held in its pre-commit hook, its working
// tree put back, makes the public store's check find the lock, though the private status is clean. Released, the
// commit lands, and the next check counts it.
#[test]
fn a_commit_under_way_in_the_other_store_is_seen_by_its_lock() {
    for mode in ["-a", "paths"] {
        let s = Stores::new();
        let private = s.repo(Store::Private);
        s.rule(Store::Private, "rule.p", "global", "committed first\n");
        s.commit(Store::Private, "a rule to edit");
        let signal = s.f.path("release");
        holding_hook(&private, &signal);
        s.rule(Store::Private, "rule.p", "global", "under way\n");
        let mut args = vec!["commit", "-q", "-m", "held"];
        args.extend(match mode {
            "-a" => vec!["-a"],
            _ => vec!["--", "global/rules/p.md"],
        });
        isolate_git();
        let mut held =
            std::process::Command::new("git").arg("-C").arg(&private).args(&args).spawn().unwrap();
        let lock = private.join(".git/index.lock");
        let until = std::time::Instant::now() + Duration::from_secs(30);
        while !lock.exists() && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(lock.exists(), "{mode}: the commit holds its lock");
        s.rule(Store::Private, "rule.p", "global", "committed first\n");
        assert_eq!(git(&private, &["status", "--porcelain"]), "", "{mode}: the status reads clean");
        s.rule(Store::Public, "rule.q", "global", "public change\n");
        s.stage(Store::Public);
        match s.admit(Store::Public).unwrap() {
            Outcome::Unclean(why) => assert!(why.contains("index.lock exists"), "{mode}: {why}"),
            other => panic!("{mode}: {other:?}"),
        }
        fs::write(&signal, "").unwrap();
        assert!(held.wait().unwrap().success(), "{mode}: the held commit lands");
        let o = s.admit(Store::Public).unwrap();
        assert!(
            matches!(o, Outcome::Unclean(_)),
            "{mode}: the put-back tree is now an edit: {o:?}"
        );
        git(&private, &["checkout", "--", "."]);
        assert!(matches!(s.admit(Store::Public).unwrap(), Outcome::Admitted(_)), "{mode}");
    }
}
