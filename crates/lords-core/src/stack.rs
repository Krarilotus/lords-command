//! A stack on disk: `stack.toml` plus layer dirs (design: On-disk layout).

use crate::finding::Finding;
use crate::frontmatter;
use crate::model::{Entry, Kind, Meta, Scope, Visibility};
use crate::schema;
use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

pub const MANIFEST: &str = "stack.toml";
pub const SIDECAR: &str = "lords.yaml";
pub const DEFAULTS: &str = "lords.defaults.yaml";
pub const SKILL_FILE: &str = "SKILL.md";
/// Index files in a memory store (agents' own tables of contents): kept, not entries.
pub const INDEX_FILES: [&str; 2] = ["MEMORY.md", "INDEX.md"];

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Repo {
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub checkouts: Vec<String>,
    #[serde(default = "yes")]
    pub worktrees: bool,
    #[serde(default)]
    pub families: Vec<String>,
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub name: String,
    pub family: String,
    pub kind: String,
    pub path: String,
    #[serde(default)]
    pub audience: Vec<String>,
    /// `replace`: the whole file becomes the managed block. Default `keep`.
    #[serde(default)]
    pub own_text: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryStore {
    pub name: String,
    pub path: String,
    pub scope: String,
    #[serde(default)]
    pub audience: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivacyConfig {
    #[serde(default)]
    pub fleet_config: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub name: String,
    pub visibility: Visibility,
    #[serde(default)]
    pub projects: BTreeMap<String, Project>,
    #[serde(default)]
    pub repos: BTreeMap<String, Repo>,
    #[serde(default)]
    pub targets: Vec<Target>,
    #[serde(default)]
    pub memory_stores: Vec<MemoryStore>,
    #[serde(default)]
    pub privacy: PrivacyConfig,
}

#[derive(Clone, Debug)]
pub struct Stack {
    pub root: PathBuf,
    pub manifest: Manifest,
    pub entries: Vec<Entry>,
    /// Problems found while loading (schema, placement, unreadable files).
    pub problems: Vec<Finding>,
}

impl Stack {
    pub fn visibility(&self) -> Visibility {
        self.manifest.visibility
    }

    /// Loads a stack. Fails only when `stack.toml` is missing or invalid; entry problems are collected.
    pub fn load(root: &Path) -> Result<Stack, String> {
        let manifest_path = root.join(MANIFEST);
        let text = fs::read_to_string(&manifest_path)
            .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
        let value: Value =
            toml::from_str(&text).map_err(|e| format!("{}: {e}", manifest_path.display()))?;
        let errs = schema::stack_errors(&value);
        if !errs.is_empty() {
            return Err(format!("{}: {}", manifest_path.display(), errs.join("; ")));
        }
        let manifest: Manifest = serde_json::from_value(value)
            .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
        let mut stack =
            Stack { root: root.to_path_buf(), manifest, entries: Vec::new(), problems: Vec::new() };
        if stack.visibility() == Visibility::Public {
            stack.check_public_manifest(&manifest_path);
        }
        for (dir, scope) in layer_dirs(root) {
            stack.load_layer(&dir, &scope);
        }
        Ok(stack)
    }

    fn check_public_manifest(&mut self, path: &Path) {
        let m = &self.manifest;
        let private_parts = !m.targets.is_empty()
            || !m.memory_stores.is_empty()
            || m.privacy.fleet_config.is_some()
            || m.repos.values().any(|r| !r.checkouts.is_empty());
        if private_parts {
            self.problems.push(
                Finding::error("public-manifest", "targets, memory stores, checkouts and the fleet config belong in the private stack")
                    .at(path, None),
            );
        }
    }

    fn load_layer(&mut self, dir: &Path, scope: &Scope) {
        for child in sorted_children(dir) {
            let name = file_name(&child);
            if child.is_file() {
                self.problems.push(
                    Finding::warning("layout", format!("a file directly in a layer dir: {name}"))
                        .at(&child, None),
                );
                continue;
            }
            let Some(kind) = Kind::from_dir(&name) else {
                self.problems.push(
                    Finding::error("layout", format!("unknown kind dir `{name}`")).at(&child, None),
                );
                continue;
            };
            if kind == Kind::Skill {
                for skill in sorted_children(&child) {
                    if skill.is_dir() {
                        self.load_skill(&skill, scope);
                    } else {
                        self.problems.push(
                            Finding::warning("layout", "a file directly in skills/")
                                .at(&skill, None),
                        );
                    }
                }
            } else {
                self.load_kind_dir(&child, kind, scope);
            }
        }
    }

    fn load_kind_dir(&mut self, dir: &Path, kind: Kind, scope: &Scope) {
        let defaults = match read_yaml_file(&dir.join(DEFAULTS)) {
            Ok(v) => v,
            Err(e) => {
                self.problems.push(Finding::error("schema", e).at(dir.join(DEFAULTS), None));
                return;
            }
        };
        for child in sorted_children(dir) {
            let name = file_name(&child);
            let not_entry = name == DEFAULTS
                || name == ".gitkeep"
                || (defaults.is_some() && INDEX_FILES.contains(&name.as_str()));
            if child.is_dir() {
                self.load_kind_dir(&child, kind, scope);
            } else if not_entry {
                continue;
            } else if name.ends_with(".md") {
                self.load_markdown(&child, kind, scope, defaults.as_ref());
            } else {
                self.problems.push(
                    Finding::warning("layout", format!("not an entry: {name}")).at(&child, None),
                );
            }
        }
    }

    fn load_markdown(
        &mut self,
        file: &Path,
        kind: Kind,
        scope: &Scope,
        defaults: Option<&Map<String, Value>>,
    ) {
        let text = match fs::read_to_string(file) {
            Ok(t) => t,
            Err(e) => {
                self.problems.push(Finding::error("read", e.to_string()).at(file, None));
                return;
            }
        };
        let parts = frontmatter::split(&text);
        let own = match parts.yaml.map(parse_yaml).transpose() {
            Ok(v) => v.unwrap_or_default(),
            Err(e) => {
                self.problems.push(Finding::error("schema", e).at(file, Some(1)));
                return;
            }
        };
        if parts.yaml.is_none() && defaults.is_none() {
            self.problems.push(Finding::error("schema", "no frontmatter").at(file, Some(1)));
            return;
        }
        let mut merged = defaults.cloned().unwrap_or_default();
        merged.extend(own);
        if defaults.is_some() {
            derive_store_fields(&mut merged, file);
        }
        let body = parts.body.to_string();
        self.push_entry(
            merged,
            kind,
            scope,
            file,
            file,
            body,
            parts.body_line,
            vec![file.to_path_buf()],
        );
    }

    fn load_skill(&mut self, dir: &Path, scope: &Scope) {
        let sidecar = dir.join(SIDECAR);
        let meta = match read_yaml_file(&sidecar) {
            Ok(Some(m)) => m,
            Ok(None) => {
                self.problems.push(
                    Finding::error("schema", "a skill needs a lords.yaml sidecar").at(dir, None),
                );
                return;
            }
            Err(e) => {
                self.problems.push(Finding::error("schema", e).at(&sidecar, None));
                return;
            }
        };
        let skill_file = dir.join(SKILL_FILE);
        let body = match fs::read_to_string(&skill_file) {
            Ok(t) => t,
            Err(_) => {
                self.problems
                    .push(Finding::error("layout", "a skill needs a SKILL.md").at(dir, None));
                return;
            }
        };
        if meta.get("id").and_then(Value::as_str).and_then(|id| id.strip_prefix("skill."))
            != Some(&file_name(dir))
        {
            self.problems.push(
                Finding::error("layout", "a skill's id must be skill.<its dir name>")
                    .at(&sidecar, None),
            );
        }
        let files = all_files(dir);
        self.push_entry(meta, Kind::Skill, scope, &sidecar, &skill_file, body, 1, files);
    }

    #[allow(clippy::too_many_arguments)]
    fn push_entry(
        &mut self,
        value: Map<String, Value>,
        dir_kind: Kind,
        layer: &Scope,
        meta_file: &Path,
        body_file: &Path,
        body: String,
        body_line: usize,
        files: Vec<PathBuf>,
    ) {
        let value = Value::Object(value);
        let errs = schema::entry_errors(&value);
        if !errs.is_empty() {
            for e in errs {
                self.problems.push(Finding::error("schema", e).at(meta_file, None));
            }
            return;
        }
        let meta: Meta = match serde_json::from_value(value) {
            Ok(m) => m,
            Err(e) => {
                self.problems.push(Finding::error("schema", e.to_string()).at(meta_file, None));
                return;
            }
        };
        let Some(scope) = Scope::parse(&meta.scope) else {
            self.problems.push(
                Finding::error("schema", format!("bad scope `{}`", meta.scope)).at(meta_file, None),
            );
            return;
        };
        let at = |f: Finding| f.at(meta_file, None).entry(meta.id.clone());
        if meta.kind != dir_kind {
            self.problems.push(at(Finding::error(
                "placement",
                format!("kind {} in a `{}` dir", meta.kind.name(), dir_kind.dir()),
            )));
        }
        let fits = match (layer, &scope) {
            (Scope::Repo(r), Scope::Path { repo, .. }) => r == repo,
            (l, s) => l == s,
        };
        if !fits {
            self.problems.push(at(Finding::error(
                "placement",
                format!("scope {scope} in the {layer} layer dir"),
            )));
        }
        if meta.visibility != self.visibility() {
            self.problems.push(at(Finding::error(
                "placement",
                format!("visibility {} in the {} stack", meta.visibility, self.visibility()),
            )));
        }
        self.entries.push(Entry {
            meta,
            scope,
            stack: self.visibility(),
            meta_file: meta_file.to_path_buf(),
            body_file: body_file.to_path_buf(),
            body,
            body_line,
            files,
        });
    }

    pub fn find(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id() == id)
    }

    /// The layer dir for a scope (path scopes live in their repo's dir).
    pub fn layer_dir(&self, scope: &Scope) -> PathBuf {
        match scope {
            Scope::Global => self.root.join("global"),
            Scope::Project(p) => self.root.join("project").join(p),
            Scope::Repo(r) | Scope::Path { repo: r, .. } => self.root.join("repo").join(r),
        }
    }
}

/// When a store's defaults supply the metadata, a memory file's own `name` and `description` give its id
/// and title, and its file name completes an imported origin.
fn derive_store_fields(merged: &mut Map<String, Value>, file: &Path) {
    let stem = file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let name = merged
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| stem.clone());
    let kind = merged.get("kind").and_then(Value::as_str).unwrap_or("memory").to_string();
    merged.entry("id").or_insert_with(|| Value::String(format!("{kind}.{}", slug(&name))));
    if !merged.contains_key("title") {
        let title = merged.get("description").and_then(Value::as_str).unwrap_or(&name);
        let title: String = title.chars().take(120).collect();
        merged.insert("title".into(), Value::String(title));
    }
    if let Some(Value::Object(origin)) = merged.get_mut("origin") {
        if origin.get("type").and_then(Value::as_str) == Some("imported")
            && !origin.contains_key("file")
        {
            origin.insert("file".into(), Value::String(file_name(file)));
        }
    }
}

pub fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

fn parse_yaml(text: &str) -> Result<Map<String, Value>, String> {
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_yaml_ng::from_str::<Value>(text) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(Value::Null) => Ok(Map::new()),
        Ok(_) => Err("frontmatter is not a mapping".into()),
        Err(e) => Err(format!("frontmatter: {e}")),
    }
}

fn read_yaml_file(path: &Path) -> Result<Option<Map<String, Value>>, String> {
    match fs::read_to_string(path) {
        Ok(t) => parse_yaml(&t).map(Some).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn layer_dirs(root: &Path) -> Vec<(PathBuf, Scope)> {
    let mut out = vec![(root.join("global"), Scope::Global)];
    for p in sorted_children(&root.join("project")) {
        if p.is_dir() {
            out.push((p.clone(), Scope::Project(file_name(&p))));
        }
    }
    for r in sorted_children(&root.join("repo")) {
        if r.is_dir() {
            out.push((r.clone(), Scope::Repo(file_name(&r))));
        }
    }
    out
}

pub fn sorted_children(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> =
        fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
    v.sort();
    v
}

pub fn all_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for c in sorted_children(dir) {
        if c.is_dir() {
            out.extend(all_files(&c));
        } else {
            out.push(c);
        }
    }
    out
}

pub fn file_name(p: &Path) -> String {
    p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// The public and (optionally) private stack, loaded together.
pub struct Stacks {
    pub public: Stack,
    pub private: Option<Stack>,
}

impl Stacks {
    pub fn load(public: &Path, private: Option<&Path>) -> Result<Stacks, String> {
        let public = Stack::load(public)?;
        if public.visibility() != Visibility::Public {
            return Err(format!(
                "{}: the public stack's stack.toml says visibility = private",
                public.root.display()
            ));
        }
        let private = private.map(Stack::load).transpose()?;
        if let Some(p) = &private {
            if p.visibility() != Visibility::Private {
                return Err(format!(
                    "{}: the private stack's stack.toml says visibility = public",
                    p.root.display()
                ));
            }
        }
        Ok(Stacks { public, private })
    }

    pub fn all(&self) -> impl Iterator<Item = &Stack> {
        std::iter::once(&self.public).chain(self.private.iter())
    }

    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.all().flat_map(|s| s.entries.iter())
    }

    pub fn find(&self, id: &str) -> Option<&Entry> {
        self.entries().find(|e| e.id() == id)
    }

    pub fn stack(&self, v: Visibility) -> Option<&Stack> {
        match v {
            Visibility::Public => Some(&self.public),
            Visibility::Private => self.private.as_ref(),
        }
    }

    /// The project a repo belongs to, from either manifest (private wins).
    pub fn repo_project(&self, repo: &str) -> Option<String> {
        let project = |s: &Stack| s.manifest.repos.get(repo).and_then(|r| r.project.clone());
        self.private.as_ref().and_then(project).or_else(|| project(&self.public))
    }

    pub fn declares_project(&self, p: &str) -> bool {
        self.all().any(|s| s.manifest.projects.contains_key(p))
    }

    pub fn declares_repo(&self, r: &str) -> bool {
        self.all().any(|s| s.manifest.repos.contains_key(r))
    }
}
