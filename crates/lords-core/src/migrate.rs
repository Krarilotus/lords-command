//! Moving one project's entries out of the old mixed private stack into its own store (v2 design §5, slice 1a),
//! with an id and hash manifest. The source is never changed: it stays the rollback source. The project's owner
//! runs this, reviews the new store and commits it in their own repo; nothing here commits.

use crate::fsguard::{self, Part, Root, Tree};
use crate::model::Scope;
use crate::stack::{self, Stack, MANIFEST};
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The manifest's file name, in the new store.
pub const RECORD: &str = "migration-manifest.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Moved {
    /// The file, relative to both stacks (`/`-separated).
    pub file: String,
    /// The entry whose metadata the file holds, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub sha256: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Migration {
    pub project: String,
    pub source: PathBuf,
    /// The source's git commit, and whether its tree had changes then.
    pub source_commit: Option<String>,
    pub source_dirty: Option<bool>,
    pub store: PathBuf,
    pub at: String,
    /// Every entry id moved, sorted.
    pub ids: Vec<String>,
    pub files: Vec<Moved>,
    /// What the new store's check found at once (placement, foreign scopes); the owner settles these.
    pub problems: Vec<String>,
}

/// The layer dirs of `project` in `source`: its project layer, and the layers of the repos and workspaces the
/// source declares for it.
fn layers(source: &Stack, project: &str) -> Vec<String> {
    let m = &source.manifest;
    let owned = |s: Scope| m.owns(project, &s);
    let mut out = vec![format!("project/{project}")];
    out.extend(
        m.repos.keys().filter(|n| owned(Scope::Repo((*n).clone()))).map(|n| format!("repo/{n}")),
    );
    out.extend(
        m.workspaces
            .keys()
            .filter(|n| owned(Scope::Workspace((*n).clone())))
            .map(|n| format!("workspace/{n}")),
    );
    out
}

/// The new store's `stack.toml`: the source's roles and topics, and its tables for this project alone.
fn manifest(source: &Path, project: &str) -> Result<String, String> {
    let path = source.join(MANIFEST);
    let text = fsguard::read_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let src: toml::Table = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let mine = |t: &toml::Value| t.get("project").and_then(toml::Value::as_str) == Some(project);
    let own = |key: &str| -> toml::Table {
        src.get(key)
            .and_then(toml::Value::as_table)
            .map(|t| {
                t.iter().filter(|(_, v)| mine(v)).map(|(k, v)| (k.clone(), v.clone())).collect()
            })
            .unwrap_or_default()
    };
    let repos = own("repos");
    let workspaces = own("workspaces");
    let places: BTreeSet<String> = std::iter::once(format!("project:{project}"))
        .chain(repos.keys().map(|r| format!("repo:{r}")))
        .chain(workspaces.keys().map(|w| format!("workspace:{w}")))
        .collect();
    let mut out = toml::Table::new();
    out.insert("name".into(), format!("{project}-lords").into());
    out.insert("visibility".into(), "private".into());
    out.insert("project".into(), project.into());
    for key in ["roles", "topics"] {
        if let Some(v) = src.get(key) {
            out.insert(key.into(), v.clone());
        }
    }
    if let Some(p) = src.get("projects").and_then(|p| p.get(project)) {
        let mut t = toml::Table::new();
        t.insert(project.into(), p.clone());
        out.insert("projects".into(), t.into());
    }
    out.insert("repos".into(), repos.into());
    out.insert("workspaces".into(), workspaces.into());
    let stores: Vec<toml::Value> = src
        .get("memory_stores")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter(|s| {
            s.get("scope").and_then(toml::Value::as_str).is_some_and(|sc| places.contains(sc))
        })
        .cloned()
        .collect();
    out.insert("memory_stores".into(), stores.into());
    toml::to_string(&out).map_err(|e| e.to_string())
}

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).unwrap_or(p).to_string_lossy().replace('\\', "/")
}

/// Copies `project`'s layers from the old stack at `source` into a new store at `to` (where nothing may be),
/// writes the manifest there, and checks that the new store loads with exactly the same entry ids and file bytes.
pub fn migrate(source: &Path, project: &str, to: &Path) -> Result<Migration, String> {
    if !stack::is_slug(project) || project.contains('.') {
        return Err(format!("`{project}` is no project id"));
    }
    let old = Stack::load(source)?;
    if !old.manifest.projects.contains_key(project) {
        return Err(format!("{} declares no project `{project}`", source.display()));
    }
    let to = std::path::absolute(to).map_err(|e| format!("{}: {e}", to.display()))?;
    let (parent, name) = match (to.parent(), to.file_name()) {
        (Some(p), Some(n)) => (p.to_path_buf(), n.to_string_lossy().into_owned()),
        _ => return Err(format!("{}: no place for a store", to.display())),
    };
    let mut tree = Tree::new();
    for layer in layers(&old, project) {
        let dir = source.join(&layer);
        if fsguard::kind_here(&dir)? == fsguard::Kind::Missing {
            continue;
        }
        for (r, bytes) in fsguard::read_tree(&dir)? {
            tree.insert(format!("{layer}/{r}"), bytes);
        }
    }
    let moved_ids: BTreeSet<String> = old
        .entries
        .iter()
        .filter(|e| old.manifest.owns(project, &e.scope))
        .map(|e| e.id().to_string())
        .collect();
    let id_of = |file: &str| {
        old.entries.iter().find(|e| rel(source, &e.meta_file) == file).map(|e| e.id().to_string())
    };
    let mut files: Vec<Moved> = tree
        .iter()
        .filter(|(f, _)| !fsguard::is_dir_entry(f))
        .map(|(f, b)| Moved { file: f.clone(), id: id_of(f), sha256: fsguard::sha(b) })
        .collect();
    files.sort_by(|a, b| a.file.cmp(&b.file));
    // A tree names every dir it holds (fsguard::Tree), so the dirs above each layer's files too.
    let dirs: Vec<String> = tree
        .keys()
        .flat_map(|k| {
            let parts: Vec<&str> = k.trim_end_matches('/').split('/').collect();
            (1..parts.len()).map(move |n| format!("{}/", parts[..n].join("/")))
        })
        .collect();
    for d in dirs {
        tree.entry(d).or_default();
    }
    tree.insert(MANIFEST.into(), manifest(source, project)?.into_bytes());
    tree.insert(".gitignore".into(), b".lords.lock\n".to_vec());
    let commit = crate::git::run(source, &["rev-parse", "HEAD"]).ok().map(|c| c.trim().to_string());
    let dirty =
        crate::git::run(source, &["status", "--porcelain"]).ok().map(|s| !s.trim().is_empty());
    let mut record = Migration {
        project: project.into(),
        source: source.to_path_buf(),
        source_commit: commit,
        source_dirty: dirty,
        store: to.clone(),
        at: crate::ops::now(),
        ids: moved_ids.iter().cloned().collect(),
        files,
        problems: Vec::new(),
    };
    let json = serde_json::to_string_pretty(&record).map_err(|e| e.to_string())?;
    tree.insert(RECORD.into(), format!("{json}\n").into_bytes());
    let home = Root::home(&parent)?;
    fsguard::put_tree(&home.join(&[Part::new(&name)?]), &tree)?;
    // The copy, read back as a project store: the same ids, the same bytes.
    let new = Stack::check_store(&to)?;
    let new_ids: BTreeSet<String> = new.entries.iter().map(|e| e.id().to_string()).collect();
    for id in moved_ids.symmetric_difference(&new_ids) {
        record.problems.push(format!("`{id}` is in only one of the source and the store"));
    }
    for m in &record.files {
        let now =
            fsguard::read_bytes(to.join(&m.file)).map(|b| fsguard::sha(&b)).unwrap_or_default();
        if now != m.sha256 {
            record.problems.push(format!("{} differs from its source", m.file));
        }
    }
    record.problems.extend(new.problems.iter().map(|p| p.to_string()));
    Ok(record)
}
