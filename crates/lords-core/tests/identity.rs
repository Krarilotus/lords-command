//! The registry and the identity resolver (v2 design §2.1, "Identity algorithm"; docs/v2-slice1.md, the
//! "Identity" inputs). Each test makes its own git repos and its own registry home in a temp dir.

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;

use common::git;
use lords_core::fsguard::canonical;
use lords_core::identity::{register_store, resolve, set_core, Binding, Place, Registry, REGISTRY};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// A temp dir with a registry home (`home/`) and room for repos, workspaces and stores.
struct World {
    dir: TempDir,
}

impl World {
    fn new() -> World {
        common::isolate_git();
        World { dir: TempDir::new().unwrap() }
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn mkdir(&self, rel: &str) -> PathBuf {
        let p = self.path(rel);
        fs::create_dir_all(&p).unwrap();
        p
    }

    /// A repo with one commit.
    fn repo(&self, rel: &str) -> PathBuf {
        let p = self.mkdir(rel);
        git(&p, &["init", "-q"]);
        fs::write(p.join("README"), "x\n").unwrap();
        git(&p, &["add", "-A"]);
        git(&p, &["commit", "-q", "-m", "fixture"]);
        p
    }

    /// A project store for `project` at `rel`, its manifest's body after the header given as `rest`.
    fn store(&self, rel: &str, project: &str, rest: &str) -> PathBuf {
        let p = self.mkdir(rel);
        fs::write(
            p.join("stack.toml"),
            format!(
                "name = \"{project}-store\"\nvisibility = \"private\"\nproject = \"{project}\"\n[projects.{project}]\n{rest}"
            ),
        )
        .unwrap();
        p
    }

    fn registry(&self) -> Registry {
        Registry::load(&self.home()).unwrap()
    }

    fn registry_text(&self) -> String {
        fs::read_to_string(self.home().join(REGISTRY)).unwrap_or_default()
    }
}

/// A TOML literal string for a path.
fn lit(p: &Path) -> String {
    format!("'{}'", p.display())
}

fn repo_decl(name: &str, project: &str, checkout: &Path) -> String {
    format!("[repos.{name}]\nproject = \"{project}\"\ncheckouts = [{}]\n", lit(checkout))
}

fn project_of(b: &Binding) -> Option<&str> {
    match b {
        Binding::Project { project, .. } => Some(project),
        Binding::CoreOnly { .. } => None,
    }
}

#[track_caller]
fn assert_core_only(b: &Binding, why: &str) {
    assert!(matches!(b, Binding::CoreOnly { .. }), "{why}: expected core only, got {b:?}");
}

#[track_caller]
fn assert_repo(b: &Binding, project: &str, name: &str, toplevel: &Path) {
    match b {
        Binding::Project { project: p, place: Place::Repo { name: n, toplevel: t }, .. } => {
            assert_eq!((p.as_str(), n.as_str()), (project, name));
            assert_eq!(t, &canonical(toplevel).unwrap());
        }
        _ => panic!("expected repo {name} of {project}, got {b:?}"),
    }
}

/// `alpha`'s store, declaring the repo `app` at `checkout`, registered.
fn alpha_with_app(w: &World, checkout: &Path) -> Registry {
    let store = w.store("stores/alpha", "alpha", &repo_decl("app", "alpha", checkout));
    register_store(&w.home(), &store).unwrap();
    w.registry()
}

// Spec (docs/v2-slice1.md, Registry home: "It holds `stores.toml`"; the brief: "no file = empty registry; an
// unreadable/invalid one is an error"): a home without the file loads as an empty registry, a broken file doesn't.
#[test]
fn a_missing_registry_is_empty_and_a_broken_one_is_an_error() {
    let w = World::new();
    assert_eq!(Registry::load(&w.home()).unwrap(), Registry::default());
    fs::create_dir_all(w.home()).unwrap();
    fs::write(w.home().join(REGISTRY), "[[stores]]\nproject = \n").unwrap();
    assert!(Registry::load(&w.home()).is_err());
    fs::write(w.home().join(REGISTRY), "[[stores]]\nproject = \"a\"\npath = \"x\"\nbogus = 1\n")
        .unwrap();
    assert!(Registry::load(&w.home()).is_err(), "an unknown field is refused, not ignored");
}

// Spec (design §2.1, step 2: "Match a registered local repository binding: common directory"): a dir inside a
// registered checkout binds to its project, with the checkout as the place's top level; `register` recorded the
// checkout's canonical common dir.
#[test]
fn a_dir_in_a_registered_checkout_binds_to_its_project() {
    let w = World::new();
    let app = w.repo("work/app");
    let sub = w.mkdir("work/app/src/deep");
    let reg = alpha_with_app(&w, &app);
    assert_eq!(reg.repos.len(), 1);
    assert_eq!(reg.repos[0].common_dir, canonical(&app.join(".git")).unwrap());
    assert_eq!(reg.stores[0].path, canonical(&w.path("stores/alpha")).unwrap());
    let b = resolve(&reg, &sub, None);
    assert_repo(&b, "alpha", "app", &app);
    match b {
        Binding::Project { store, cwd, .. } => {
            assert_eq!(store, canonical(&w.path("stores/alpha")).unwrap());
            assert_eq!(cwd, canonical(&sub).unwrap());
        }
        _ => unreachable!(),
    }
}

// Spec (design §2.1, step 1: "Canonicalise the existing cwd using filesystem identity, resolving
// junctions/symlinks"): a cwd reached through a junction binds as its target does, and the binding names the
// target, not the alias.
#[cfg(windows)]
#[test]
fn a_cwd_through_a_junction_binds_as_its_target() {
    let w = World::new();
    let app = w.repo("work/app");
    w.mkdir("work/app/src");
    let reg = alpha_with_app(&w, &app);
    let alias = w.path("alias");
    let out = std::process::Command::new("cmd")
        .args(["/c", "mklink", "/J"])
        .arg(&alias)
        .arg(app.join("src"))
        .output()
        .unwrap();
    assert!(out.status.success(), "mklink: {}", String::from_utf8_lossy(&out.stderr));
    let b = resolve(&reg, &alias, None);
    assert_repo(&b, "alpha", "app", &app);
    match b {
        Binding::Project { cwd, .. } => assert_eq!(cwd, canonical(&app.join("src")).unwrap()),
        _ => unreachable!(),
    }
    // The alias's own spelling, with other case and separators, settles the same way.
    let spelt = PathBuf::from(alias.to_string_lossy().to_uppercase().replace('\\', "/"));
    assert_repo(&resolve(&reg, &spelt, None), "alpha", "app", &app);
}

// Spec (design §2.1, step 3: "A worktree sharing the registered common directory belongs to that repository even
// outside" the project's worktree dir): a linked worktree outside every declared dir binds to the repo, with the
// worktree as the place's top level.
#[test]
fn a_linked_worktree_outside_any_root_belongs_to_its_repo() {
    let w = World::new();
    let app = w.repo("work/app");
    let reg = alpha_with_app(&w, &app);
    let wt = w.path("elsewhere/far/wt");
    git(&app, &["worktree", "add", "-q", "-b", "side", &wt.to_string_lossy()]);
    assert_repo(&resolve(&reg, &wt, None), "alpha", "app", &wt);
}

// Spec (design §2.1, step 3: "a nested unrelated Git repo needs its own binding ... it never inherits its
// parent's project"): a repo inside a bound checkout is core only, and so is a dir inside it.
#[test]
fn a_nested_unrelated_repo_in_a_bound_checkout_is_core_only() {
    let w = World::new();
    let app = w.repo("work/app");
    let nested = w.repo("work/app/vendor/other");
    let inner = w.mkdir("work/app/vendor/other/lib");
    let reg = alpha_with_app(&w, &app);
    assert_repo(&resolve(&reg, &w.path("work/app/vendor"), None), "alpha", "app", &app);
    assert_core_only(&resolve(&reg, &nested, None), "nested repo");
    assert_core_only(&resolve(&reg, &inner, None), "dir in the nested repo");
}

// Spec (design §2.1, step 3: "a ... submodule ... needs its own binding"): a submodule of a bound checkout is core
// only.
#[test]
fn a_submodule_of_a_bound_checkout_is_core_only() {
    let w = World::new();
    let lib = w.repo("src/lib");
    let app = w.repo("work/app");
    git(
        &app,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            &lib.to_string_lossy(),
            "lib",
        ],
    );
    let reg = alpha_with_app(&w, &app);
    assert_core_only(&resolve(&reg, &app.join("lib"), None), "submodule");
}

// Spec (design §2.1, step 2: "A remote URL alone never grants identity"; step 3: "A fresh clone ... needs its own
// binding"): a fresh clone of the bound repo's remote, whose origin matches the recorded one, is core only.
#[test]
fn a_fresh_clone_of_the_same_remote_is_core_only() {
    let w = World::new();
    let upstream = w.repo("upstream/app");
    let app = w.path("work/app");
    git(w.dir.path(), &["clone", "-q", &upstream.to_string_lossy(), &app.to_string_lossy()]);
    let reg = alpha_with_app(&w, &app);
    assert!(reg.repos[0].remote.is_some(), "the origin remote is recorded");
    let clone = w.path("work/app-again");
    git(w.dir.path(), &["clone", "-q", &upstream.to_string_lossy(), &clone.to_string_lossy()]);
    assert_core_only(&resolve(&reg, &clone, None), "fresh clone");
    assert_repo(&resolve(&reg, &app, None), "alpha", "app", &app);
}

// Spec (design §2.1, step 2: "common directory + ... owner-recorded remote identity, where present"; step 3: "A
// ... remote change ... needs its own binding/reconfirmation"; step 5: "contradictory ... bindings: core only"):
// the bound checkout whose origin now points elsewhere is core only until it is registered again.
#[test]
fn a_changed_remote_contradicts_the_binding() {
    let w = World::new();
    let upstream = w.repo("upstream/app");
    let app = w.path("work/app");
    git(w.dir.path(), &["clone", "-q", &upstream.to_string_lossy(), &app.to_string_lossy()]);
    let reg = alpha_with_app(&w, &app);
    git(&app, &["remote", "set-url", "origin", "https://example.invalid/other/repo.git"]);
    let b = resolve(&reg, &app, None);
    assert_core_only(&b, "remote changed");
    git(&app, &["remote", "remove", "origin"]);
    assert_core_only(&resolve(&reg, &app, None), "recorded remote gone");
}

/// `alpha`'s store with the workspace `desk` at `work/desk`, `descendants` listed, registered.
fn alpha_with_desk(w: &World, descendants: &str) -> (Registry, PathBuf) {
    let desk = w.mkdir("work/desk");
    let store = w.store(
        "stores/alpha",
        "alpha",
        &format!(
            "[workspaces.desk]\nproject = \"alpha\"\npath = {}\ndescendants = [{descendants}]\n",
            lit(&desk)
        ),
    );
    register_store(&w.home(), &store).unwrap();
    (w.registry(), desk)
}

#[track_caller]
fn assert_desk(b: &Binding) {
    assert!(
        matches!(b, Binding::Project { project, place: Place::Workspace { name }, .. }
            if project == "alpha" && name == "desk"),
        "expected the workspace desk of alpha, got {b:?}"
    );
}

// Spec (design §2.1, step 4: "Outside Git, match an explicitly registered canonical workspace ... Only explicitly
// allowed non-Git descendants inherit it"): the workspace's own dir and a listed descendant (and dirs below it)
// bind; an unlisted subdir is core only; a dir merely sharing the workspace path's text as a prefix is core only.
#[test]
fn a_workspace_binds_itself_and_listed_descendants_only() {
    let w = World::new();
    let (reg, desk) = alpha_with_desk(&w, "\"notes\", \".local/log\"");
    assert_eq!(reg.workspaces[0].descendants, vec!["notes".to_string(), ".local/log".to_string()]);
    assert_desk(&resolve(&reg, &desk, None));
    assert_desk(&resolve(&reg, &w.mkdir("work/desk/notes"), None));
    assert_desk(&resolve(&reg, &w.mkdir("work/desk/notes/today"), None));
    assert_desk(&resolve(&reg, &w.mkdir("work/desk/.local/log"), None));
    assert_core_only(&resolve(&reg, &w.mkdir("work/desk/scratch"), None), "unlisted subdir");
    assert_core_only(&resolve(&reg, &w.mkdir("work/desk/.local"), None), "parent of a listed one");
    assert_core_only(&resolve(&reg, &w.mkdir("work/desk-2"), None), "string prefix only");
    assert_core_only(
        &resolve(&reg, &w.mkdir("work/desk/notesx"), None),
        "string prefix of a descendant",
    );
}

// Spec (design §2.1, step 4: "Discovering a Git boundary cancels workspace inheritance unless that Git repo is
// itself bound"): an unbound repo inside a workspace, even at a listed descendant, is core only.
#[test]
fn a_git_boundary_cancels_workspace_inheritance() {
    let w = World::new();
    let (reg, _) = alpha_with_desk(&w, "\"notes\"");
    let repo = w.repo("work/desk/notes/tool");
    assert_core_only(&resolve(&reg, &repo, None), "unbound repo in a listed descendant");
    assert_core_only(&resolve(&reg, &w.mkdir("work/desk/notes/tool/sub"), None), "dir in it");
}

// Spec (design §2.1, step 5: "Zero matches ...: core only"; step 1: "Reject unreadable paths"): an unknown dir is
// core only with its canonical path; a dir that doesn't exist is core only with none.
#[test]
fn an_unknown_dir_and_a_missing_dir_are_core_only() {
    let w = World::new();
    let app = w.repo("work/app");
    let reg = alpha_with_app(&w, &app);
    let unknown = w.mkdir("unknown");
    match resolve(&reg, &unknown, None) {
        Binding::CoreOnly { cwd, reason } => {
            assert_eq!(cwd, Some(canonical(&unknown).unwrap()));
            assert!(!reason.is_empty());
        }
        b => panic!("unknown dir: {b:?}"),
    }
    match resolve(&reg, &w.path("not/there"), None) {
        Binding::CoreOnly { cwd, .. } => assert_eq!(cwd, None),
        b => panic!("missing dir: {b:?}"),
    }
    assert_core_only(&resolve(&Registry::default(), &app, None), "empty registry");
}

// Spec (design §2.1, Registry: "Conflicting registration is refused"; the brief: "Re-registering the same store is
// idempotent"): the same store registered twice leaves the same file; a second store claiming the project is
// refused and nothing is written.
#[test]
fn a_store_registers_once_and_a_second_store_for_its_project_is_refused() {
    let w = World::new();
    let app = w.repo("work/app");
    let store = w.store("stores/alpha", "alpha", &repo_decl("app", "alpha", &app));
    let first = register_store(&w.home(), &store).unwrap();
    assert!(first.changed);
    let text = w.registry_text();
    let again = register_store(&w.home(), &store).unwrap();
    assert!(!again.changed);
    assert_eq!(w.registry_text(), text);
    let other = w.store("stores/alpha-2", "alpha", "");
    let err = register_store(&w.home(), &other).unwrap_err();
    assert!(err.contains("alpha"), "{err}");
    assert_eq!(w.registry_text(), text);
}

// Spec (design §2.1, Registry: "Conflicting registration is refused"; step 5: "multiple projects claiming one
// identity: core only"): a second project declaring the same checkout (one common dir) is refused, and a registry
// that holds such a claim anyway (written by hand) resolves the checkout to core only.
#[test]
fn two_projects_claiming_one_common_dir() {
    let w = World::new();
    let app = w.repo("work/app");
    let reg = alpha_with_app(&w, &app);
    let text = w.registry_text();
    let beta = w.store("stores/beta", "beta", &repo_decl("app", "beta", &app));
    let err = register_store(&w.home(), &beta).unwrap_err();
    assert!(err.contains("alpha"), "{err}");
    assert_eq!(w.registry_text(), text);
    let mut forged = reg.clone();
    forged.stores.push(lords_core::identity::StoreReg {
        project: "beta".into(),
        path: canonical(&beta).unwrap(),
    });
    let mut claim = reg.repos[0].clone();
    claim.project = "beta".into();
    forged.repos.push(claim);
    assert_core_only(&resolve(&forged, &app, None), "two projects, one common dir");
}

// Spec (design §2.1, Registry: "Conflicting registration is refused"): a workspace path another project holds is
// refused.
#[test]
fn two_projects_claiming_one_workspace_is_refused() {
    let w = World::new();
    let (_, desk) = alpha_with_desk(&w, "");
    let text = w.registry_text();
    let beta = w.store(
        "stores/beta",
        "beta",
        &format!("[workspaces.desk]\nproject = \"beta\"\npath = {}\n", lit(&desk)),
    );
    assert!(register_store(&w.home(), &beta).is_err());
    assert_eq!(w.registry_text(), text);
}

// Spec (design §2.1, step 5: "A `--project` hint must agree with the binding; it is no escape hatch"): a hint that
// names the bound project keeps the binding; one that names another project, or names one for an unbound dir,
// gives core only.
#[test]
fn a_disagreeing_hint_gives_core_only() {
    let w = World::new();
    let app = w.repo("work/app");
    let reg = alpha_with_app(&w, &app);
    assert_eq!(project_of(&resolve(&reg, &app, Some("alpha"))), Some("alpha"));
    assert_core_only(&resolve(&reg, &app, Some("beta")), "hint names another project");
    assert_core_only(&resolve(&reg, &w.mkdir("unknown"), Some("alpha")), "hint for an unbound dir");
}

// Spec (design §2.1, step 2: "strip embedded credentials from comparisons/logs"; docs/v2-slice1.md: `remote` is
// "credential-free"): a remote URL with a user and a password is recorded without either, the registry file holds
// neither, and the checkout still binds.
#[test]
fn credentials_in_a_remote_url_are_never_stored() {
    let w = World::new();
    let app = w.repo("work/app");
    // Assembled at run time, so the repo's own privacy check doesn't take it for a real credential.
    let password = ["hunter", "2x"].concat();
    let url = ["https://someone:", &password, "@Example.invalid/org/repo.git"].concat();
    git(&app, &["remote", "add", "origin", &url]);
    let reg = alpha_with_app(&w, &app);
    assert_eq!(reg.repos[0].remote.as_deref(), Some("example.invalid/org/repo"));
    let text = w.registry_text();
    assert!(!text.contains(&password) && !text.contains("someone"), "{text}");
    assert_repo(&resolve(&reg, &app, None), "alpha", "app", &app);
    // The same remote by ssh is the same identity.
    let ssh = ["git", "example.invalid:org/repo"].join("@");
    git(&app, &["remote", "set-url", "origin", &ssh]);
    assert_repo(&resolve(&reg, &app, None), "alpha", "app", &app);
}

// Spec (the brief: "requires `manifest.project`"; "records the store"): a stack without a project id doesn't
// register; `set_core` records the core's two canonical dirs.
#[test]
fn a_store_needs_a_project_and_the_core_is_recorded() {
    let w = World::new();
    let plain = w.mkdir("stores/plain");
    fs::write(plain.join("stack.toml"), "name = \"plain\"\nvisibility = \"private\"\n").unwrap();
    assert!(register_store(&w.home(), &plain).is_err());
    assert_eq!(w.registry_text(), "");
    let (public, private) = (w.mkdir("core/public"), w.mkdir("core/private"));
    set_core(&w.home(), &public, Some(&private)).unwrap();
    let core = w.registry().core.unwrap();
    assert_eq!(core.public, canonical(&public).unwrap());
    assert_eq!(core.private, Some(canonical(&private).unwrap()));
}
