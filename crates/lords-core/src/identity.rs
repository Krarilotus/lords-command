//! Which project a working directory belongs to, and the registry of project stores (v2 design §2.1, identity
//! algorithm). One owner, shared by the hook, the CLI and the MCP server.
//!
//! Every path the registry holds and every path resolve compares is canonical (`fsguard::canonical`), and paths
//! are compared by their components, never as strings. Resolving reads the filesystem and git and never writes;
//! only `register_store` and `set_core` write, and only the registry file, through fsguard.

use crate::fsguard::{self, Kind, Part, Root};
use crate::git;
use crate::stack::Stack;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The registry's file in the registry home.
pub const REGISTRY: &str = "stores.toml";
/// The lock file every registry read and change holds, beside it.
const LOCK: &str = "stores.lock";

/// `<home>/stores.toml`: where the stores are and the bindings their owners approved, never their content.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub core: Option<Core>,
    #[serde(default)]
    pub stores: Vec<StoreReg>,
    #[serde(default)]
    pub repos: Vec<RepoBinding>,
    #[serde(default)]
    pub workspaces: Vec<WorkspaceBinding>,
}

/// The core: the public stack and the private overlay, of which only `global/` is read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Core {
    pub public: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub private: Option<PathBuf>,
}

/// A project's store: its dir, canonical.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreReg {
    pub project: String,
    pub path: PathBuf,
}

/// A checkout of a repo the project's store declares: its canonical absolute git common dir and checkout, and the
/// origin remote without credentials, when it has one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoBinding {
    pub project: String,
    pub name: String,
    pub common_dir: PathBuf,
    pub checkout: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
}

/// A non-git workspace the project's store declares: its canonical path and the non-git dirs below it (relative,
/// `/`-separated) that inherit it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceBinding {
    pub project: String,
    pub name: String,
    pub path: PathBuf,
    #[serde(default)]
    pub descendants: Vec<String>,
}

/// What `register_store` recorded for the store's project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Registration {
    pub project: String,
    pub store: PathBuf,
    pub repos: Vec<RepoBinding>,
    pub workspaces: Vec<WorkspaceBinding>,
    /// Whether the registry file changed (`false`: the same registration was there already).
    pub changed: bool,
}

/// Where a working dir sits in its project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    Repo { name: String, toplevel: PathBuf },
    Workspace { name: String },
}

/// What a working dir is bound to: one project, or the core alone (with why).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Binding {
    Project { project: String, store: PathBuf, place: Place, cwd: PathBuf },
    CoreOnly { cwd: Option<PathBuf>, reason: String },
}

/// The registry home: `$LORDS_HOME`, else `~/.lords`.
pub fn default_home() -> Option<PathBuf> {
    match std::env::var_os("LORDS_HOME") {
        Some(h) if !h.is_empty() => Some(PathBuf::from(h)),
        _ => crate::config::home().map(|h| h.join(".lords")),
    }
}

impl Registry {
    /// The registry in `home`: empty when it has no file; an error when the file can't be read or isn't a
    /// registry. Conflicting claims load as they are: `resolve` takes them for ambiguity. Read under the registry's
    /// lock, so a registration being saved is never read half done or missing (Sol review r1, F3).
    pub fn load(home: &Path) -> Result<Registry, String> {
        if fsguard::kind_here(home)? == fsguard::Kind::Missing {
            return Ok(Registry::default());
        }
        let _held = lock(home)?;
        Registry::load_held(home)
    }

    /// `load`, by a caller that holds the lock.
    fn load_held(home: &Path) -> Result<Registry, String> {
        let file = home.join(REGISTRY);
        match fsguard::read_text(&file)? {
            Some(text) => toml::from_str(&text).map_err(|e| format!("{}: {e}", file.display())),
            // No registry is first use only if no save was cut off after moving one aside (Sol fix check 1, F3): such
            // a copy is the last acknowledged registry, and nothing builds on its absence.
            None => match fsguard::asides(&Root::home(home)?.join(&[Part::new(REGISTRY)?]))?.first() {
                Some(aside) => Err(format!(
                    "{}: a registry save was interrupted; the last registry is {}: move it back to {} (one copy is \
                     enough if there are several), then retry",
                    file.display(),
                    aside.display(),
                    file.display()
                )),
                None => Ok(Registry::default()),
            },
        }
    }

    /// Writes the registry to `home` in one step (`fsguard::publish`): a reader and an interruption see the old
    /// registry or the new one, never none.
    fn save(&self, home: &Path) -> Result<(), String> {
        let text = toml::to_string(self).map_err(|e| format!("the registry: {e}"))?;
        let root = Root::home(home)?;
        root.create()?;
        fsguard::publish(&root.join(&[Part::new(REGISTRY)?]), text.as_bytes())
    }

    /// Its lists in one order, so the same registrations always make the same file.
    fn sort(&mut self) {
        self.stores.sort_by(|a, b| (&a.project, &a.path).cmp(&(&b.project, &b.path)));
        self.repos.sort_by(|a, b| {
            (&a.project, &a.name, &a.checkout).cmp(&(&b.project, &b.name, &b.checkout))
        });
        self.repos.dedup();
        self.workspaces.sort_by(|a, b| (&a.project, &a.name).cmp(&(&b.project, &b.name)));
    }
}

/// The registry's lock: every read and every read-modify-write of `stores.toml` holds it, so concurrent
/// registrations each see the one before and none is lost (Sol review r1, F3).
fn lock(home: &Path) -> Result<fsguard::Lock, String> {
    let root = Root::home(home)?;
    root.create()?;
    fsguard::lock(&root.join(&[Part::new(LOCK)?]))
}

/// Records the core's public stack and private overlay (both canonical).
pub fn set_core(home: &Path, public: &Path, private: Option<&Path>) -> Result<(), String> {
    let core = Core {
        public: fsguard::canonical(public)?,
        private: private.map(fsguard::canonical).transpose()?,
    };
    let _held = lock(home)?;
    let mut reg = Registry::load_held(home)?;
    if reg.core.as_ref() != Some(&core) {
        reg.core = Some(core);
        reg.save(home)?;
    }
    Ok(())
}

/// Registers the project store at `store_dir`: the store, and each checkout and workspace its manifest declares
/// for its project, replacing what the registry held for that project. Refused, with nothing written, when another
/// project already holds the project id, the store, a common dir or a workspace path.
pub fn register_store(home: &Path, store_dir: &Path) -> Result<Registration, String> {
    let store = fsguard::canonical(store_dir)?;
    let stack = Stack::load(&store)?;
    let project = stack.manifest.project.clone().ok_or_else(|| {
        format!("{}: its stack.toml names no `project`, so it is no project store", store.display())
    })?;
    let mut repos = Vec::new();
    for (name, r) in &stack.manifest.repos {
        if r.project.as_deref() == Some(project.as_str()) {
            for c in &r.checkouts {
                repos.push(bind_checkout(&project, name, &crate::config::expand_home(c))?);
            }
        }
    }
    let mut workspaces = Vec::new();
    for (name, ws) in &stack.manifest.workspaces {
        if ws.project.as_deref() == Some(project.as_str()) {
            for d in &ws.descendants {
                descendant_parts(d)?;
            }
            workspaces.push(WorkspaceBinding {
                project: project.clone(),
                name: name.clone(),
                path: fsguard::canonical(&crate::config::expand_home(&ws.path))?,
                descendants: ws.descendants.clone(),
            });
        }
    }
    let _held = lock(home)?;
    let old = Registry::load_held(home)?;
    refuse_conflicts(&old, &project, &store, &repos, &workspaces)?;
    let mut reg = old.clone();
    reg.stores.retain(|s| s.project != project);
    reg.repos.retain(|r| r.project != project);
    reg.workspaces.retain(|w| w.project != project);
    reg.stores.push(StoreReg { project: project.clone(), path: store.clone() });
    reg.repos.extend(repos.iter().cloned());
    reg.workspaces.extend(workspaces.iter().cloned());
    reg.sort();
    let changed = reg != old;
    if changed {
        reg.save(home)?;
    }
    Ok(Registration { project, store, repos, workspaces, changed })
}

/// A declared checkout's binding: it must be a checkout's root; its common dir canonical, its origin remote
/// without credentials.
fn bind_checkout(project: &str, name: &str, path: &Path) -> Result<RepoBinding, String> {
    let checkout = fsguard::canonical(path)?;
    let top = git_path(&checkout, &["rev-parse", "--show-toplevel"])?;
    if top != checkout {
        return Err(format!(
            "repo {name}: {} is no checkout's root (git's top level there is {})",
            checkout.display(),
            top.display()
        ));
    }
    Ok(RepoBinding {
        project: project.to_string(),
        name: name.to_string(),
        common_dir: git_path(
            &checkout,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?,
        checkout,
        remote: origin(&top)?,
    })
}

/// The parts of a workspace's descendant: one or more plain names, `/`-separated.
fn descendant_parts(d: &str) -> Result<Vec<Part>, String> {
    d.split('/')
        .map(Part::new)
        .collect::<Result<_, _>>()
        .map_err(|e| format!("descendant {d:?}: {e}"))
}

/// What another project already holds of what this registration claims.
fn refuse_conflicts(
    reg: &Registry,
    project: &str,
    store: &Path,
    repos: &[RepoBinding],
    workspaces: &[WorkspaceBinding],
) -> Result<(), String> {
    for s in &reg.stores {
        if s.project == project && s.path != store {
            return Err(format!(
                "project {project} is registered with another store, {}; refused",
                s.path.display()
            ));
        }
        if s.project != project && s.path == store {
            return Err(format!(
                "{} is registered as project {}'s store; refused",
                store.display(),
                s.project
            ));
        }
    }
    let others = || reg.repos.iter().filter(|r| r.project != project);
    for r in repos {
        if let Some(o) = others().find(|o| o.common_dir == r.common_dir) {
            return Err(format!(
                "repo {}: its git dir {} is project {}'s (repo {}); refused",
                r.name,
                r.common_dir.display(),
                o.project,
                o.name
            ));
        }
        if let Some(o) = repos.iter().find(|o| o.common_dir == r.common_dir && o.name != r.name) {
            return Err(format!(
                "repos {} and {} share the git dir {}; one repo, one name",
                r.name,
                o.name,
                r.common_dir.display()
            ));
        }
    }
    for w in workspaces {
        let mut held = reg.workspaces.iter().filter(|o| o.project != project);
        let mut same = workspaces.iter().filter(|o| o.name != w.name);
        if let Some(o) = held.find(|o| o.path == w.path).or_else(|| same.find(|o| o.path == w.path))
        {
            return Err(format!(
                "workspace {}: {} is project {}'s workspace {}; refused",
                w.name,
                w.path.display(),
                o.project,
                o.name
            ));
        }
    }
    Ok(())
}

/// The project and place `cwd` is bound to, by the design's five steps (§2.1, identity algorithm), or the core
/// alone with the reason. A `hint` must name the bound project; it never chooses one.
pub fn resolve(reg: &Registry, cwd: &Path, hint: Option<&str>) -> Binding {
    let cwd = match fsguard::canonical(cwd) {
        Ok(c) => c,
        Err(e) => {
            return Binding::CoreOnly { cwd: None, reason: format!("the working dir: {e}") };
        }
    };
    let core = |reason: String| Binding::CoreOnly { cwd: Some(cwd.clone()), reason };
    let (project, place) = match bind(reg, &cwd) {
        Ok(found) => found,
        Err(reason) => return core(reason),
    };
    if let Some(h) = hint.filter(|h| *h != project) {
        return core(format!(
            "the hint names project {h}, but {} is project {project}'s",
            cwd.display()
        ));
    }
    let stores: Vec<&StoreReg> = reg.stores.iter().filter(|s| s.project == project).collect();
    match stores.as_slice() {
        [s] => Binding::Project { project, store: s.path.clone(), place, cwd: cwd.clone() },
        [] => core(format!("project {project} has no registered store")),
        _ => core(format!("{} stores claim project {project}", stores.len())),
    }
}

/// Steps 2 to 5 for a canonical dir.
fn bind(reg: &Registry, cwd: &Path) -> Result<(String, Place), String> {
    match fsguard::kind_here(cwd)? {
        Kind::Dir => {}
        _ => return Err(format!("{} is no dir", cwd.display())),
    }
    match git_boundary(cwd)? {
        Some(boundary) => bind_repo(reg, cwd, boundary),
        None => {
            let (project, name) = one(workspaces_covering(reg, cwd), || {
                format!("no project binds {}", cwd.display())
            })?;
            Ok((project, Place::Workspace { name }))
        }
    }
}

/// The nearest dir at or above `cwd` that holds a `.git` (a dir, or a worktree's or submodule's file).
fn git_boundary(cwd: &Path) -> Result<Option<&Path>, String> {
    for dir in cwd.ancestors() {
        match fsguard::kind_here(&dir.join(".git"))? {
            Kind::Missing => {}
            _ => return Ok(Some(dir)),
        }
    }
    Ok(None)
}

/// Steps 2, 3 and 5 inside git: the repo bound by its common dir, consistent with every other claim on it.
fn bind_repo(reg: &Registry, cwd: &Path, boundary: &Path) -> Result<(String, Place), String> {
    let top = git_path(cwd, &["rev-parse", "--show-toplevel"])?;
    if top != boundary {
        return Err(format!(
            "git's top level {} isn't the nearest .git, at {}",
            top.display(),
            boundary.display()
        ));
    }
    let common = git_path(cwd, &["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
    let bound: Vec<&RepoBinding> = reg.repos.iter().filter(|r| r.common_dir == common).collect();
    let (project, name) = one(bound.iter().map(|r| (r.project.as_str(), r.name.as_str())), || {
        format!(
            "the git repo at {} is bound to no project (a fresh clone, a submodule or a nested repo needs its own registration)",
            top.display()
        )
    })?;
    if let Some(r) = reg.repos.iter().find(|r| r.checkout == top && r.common_dir != common) {
        return Err(format!(
            "{} is registered as a checkout of project {}'s repo {} with another git dir",
            top.display(),
            r.project,
            r.name
        ));
    }
    if bound.iter().any(|r| r.remote.is_some()) {
        let now = origin(&top)?;
        if bound.iter().any(|r| r.remote.is_some() && r.remote != now) {
            return Err(format!(
                "the origin remote of {} isn't the one recorded for project {project}'s repo {name}; register it again",
                top.display()
            ));
        }
    }
    if let Some((p, w)) = workspaces_covering(reg, cwd).find(|(p, _)| *p != project) {
        return Err(format!(
            "{} is project {project}'s repo {name}, and project {p}'s workspace {w} claims it too",
            cwd.display()
        ));
    }
    Ok((project, Place::Repo { name, toplevel: top }))
}

/// The workspaces that bind `cwd`: their own dir, or a listed descendant and the dirs below it.
fn workspaces_covering<'a>(
    reg: &'a Registry,
    cwd: &'a Path,
) -> impl Iterator<Item = (&'a str, &'a str)> + 'a {
    reg.workspaces
        .iter()
        .filter(move |w| {
            cwd == w.path
                || w.descendants
                    .iter()
                    .any(|d| descendant_parts(d).is_ok() && cwd.starts_with(w.path.join(d)))
        })
        .map(|w| (w.project.as_str(), w.name.as_str()))
}

/// The one (project, name) the claims name: none or several is an error.
fn one<'a>(
    claims: impl Iterator<Item = (&'a str, &'a str)>,
    none: impl FnOnce() -> String,
) -> Result<(String, String), String> {
    let claims: BTreeSet<(&str, &str)> = claims.collect();
    let mut it = claims.iter();
    match (it.next(), it.next()) {
        (None, _) => Err(none()),
        (Some((p, n)), None) => Ok((p.to_string(), n.to_string())),
        _ => Err(format!(
            "ambiguous: claimed by {}",
            claims.iter().map(|(p, n)| format!("project {p}'s {n}")).collect::<Vec<_>>().join(", ")
        )),
    }
}

/// A path git prints, canonical.
fn git_path(dir: &Path, args: &[&str]) -> Result<PathBuf, String> {
    let out = git::run(dir, args)?;
    fsguard::canonical(Path::new(out.trim()))
}

/// The origin remote's identity, if it has one.
fn origin(top: &Path) -> Result<Option<String>, String> {
    Ok(git::config_get(top, "remote.origin.url")?.map(|u| remote_identity(&u)))
}

/// A remote URL as an identity without credentials or transport: `host/path` for a URL or an scp-like address
/// (any user and password dropped, the host lower-cased, a final `.git` and `/` dropped), so the same repo by
/// https and by ssh is one identity; a local path as it is, with `/` separators.
pub fn remote_identity(url: &str) -> String {
    let url = url.trim();
    let (host, path) = match url.split_once("://") {
        Some((scheme, _)) if scheme.eq_ignore_ascii_case("file") => {
            return url.replace('\\', "/").trim_end_matches('/').to_string();
        }
        Some((_, rest)) => rest.split_once('/').unwrap_or((rest, "")),
        // scp-like: a `:` before any `/`, after more than a drive letter.
        None => match url.find(':') {
            Some(i) if i > 1 && !url[..i].contains(['/', '\\']) => (&url[..i], &url[i + 1..]),
            _ => return url.replace('\\', "/").trim_end_matches('/').to_string(),
        },
    };
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h).to_ascii_lowercase();
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path).trim_end_matches('/');
    format!("{host}/{path}")
}

#[cfg(test)]
mod tests {
    use super::remote_identity;

    // Spec (design §2.1, step 2: "strip embedded credentials from comparisons/logs"): every URL form loses its user
    // and password, and the forms of one repo are one identity; a drive path is no scp address.
    #[test]
    fn a_remote_identity_has_no_credentials() {
        let id = "example.invalid/org/repo";
        // Assembled at run time, so the repo's own privacy check doesn't take them for real credentials.
        let pw = ["p", "w"].concat();
        for url in [
            "https://example.invalid/org/repo.git".to_string(),
            ["https://user:", &pw, "@Example.invalid/org/repo.git/"].concat(),
            ["ssh://git", "example.invalid/org/repo"].join("@"),
            ["git", "example.invalid:org/repo.git"].join("@"),
        ] {
            assert_eq!(remote_identity(&url), id, "{url}");
        }
        assert_eq!(remote_identity(r"C:\src\repo"), "C:/src/repo");
        assert_eq!(remote_identity("/srv/repo.git"), "/srv/repo.git");
    }
}
