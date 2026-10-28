//! A stack on disk: `stack.toml` plus layer dirs (design: On-disk layout).

use crate::finding::Finding;
use crate::frontmatter;
use crate::fsguard;
use crate::model::{Entry, Kind, Meta, Scope, Visibility};
use crate::schema;
use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
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
    /// Worktrees outside these dirs are skipped (and listed); empty: every worktree. Checkouts always count.
    #[serde(default)]
    pub worktree_roots: Vec<String>,
    /// The adapters that get this repo's files (default: every adapter).
    #[serde(default)]
    pub adapters: Vec<String>,
    /// Skill dirs tracked in the repo (paths in a checkout): indexed in place, never linked, copied or moved.
    #[serde(default)]
    pub skills: Vec<String>,
    /// The role a session here has when no brief names one: its entries go inline in the place's files.
    #[serde(default)]
    pub default_role: Option<String>,
}

/// A working dir that is no git repo (e.g. a harness dir): it gets the same untracked files as a repo checkout.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workspace {
    pub path: String,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub adapters: Vec<String>,
    /// As for a repo: the role of a session here without a brief.
    #[serde(default)]
    pub default_role: Option<String>,
    /// Non-git dirs below `path` (relative, `/`-separated) that belong to this workspace too (v2 design §2.1,
    /// identity step 4); none by default.
    #[serde(default)]
    pub descendants: Vec<String>,
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub name: String,
    /// The adapter that writes this target (adapters.rs: `claude`, `codex`).
    pub adapter: String,
    pub kind: String,
    pub path: String,
    #[serde(default)]
    pub audience: Vec<String>,
    /// The role whoever reads this file has; empty: unknown.
    #[serde(default)]
    pub roles: Vec<String>,
    /// `replace`: the whole file becomes the managed block. Default `keep`.
    #[serde(default)]
    pub own_text: Option<String>,
    /// instructions: write the composed text to this shared file and only an include line here (adapters that
    /// expand includes).
    #[serde(default)]
    pub via: Option<String>,
    /// settings: instruction files the harness must skip (its settings' exclude list).
    #[serde(default)]
    pub exclude: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryStore {
    pub name: String,
    pub path: String,
    pub scope: String,
    #[serde(default)]
    pub audience: Vec<String>,
    /// Adapters whose harness already loads this store by itself: left out of their memory index.
    #[serde(default)]
    pub native: Vec<String>,
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
    /// A project store (v2 design §2.1): the one project it holds. `None`: a v1 stack or the core.
    #[serde(default)]
    pub project: Option<String>,
    /// The controlled list of topics (design: Entry metadata).
    #[serde(default)]
    pub topics: Vec<String>,
    /// The controlled list of roles (design: Entry metadata).
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub projects: BTreeMap<String, Project>,
    #[serde(default)]
    pub repos: BTreeMap<String, Repo>,
    /// private only: non-git working dirs.
    #[serde(default)]
    pub workspaces: BTreeMap<String, Workspace>,
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
        let text = fsguard::read_string(&manifest_path)
            .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
        let value: Value =
            toml::from_str(&text).map_err(|e| format!("{}: {e}", manifest_path.display()))?;
        let errs = schema::stack_errors(&value);
        if !errs.is_empty() {
            return Err(format!("{}: {}", manifest_path.display(), errs.join("; ")));
        }
        let manifest: Manifest = serde_json::from_value(value)
            .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
        // Target names are unique, so a `target:<name>` ledger scope names one target (Sol review r5, R5-06).
        let mut names: Vec<&str> = manifest.targets.iter().map(|t| t.name.as_str()).collect();
        names.sort();
        if let Some(w) = names.windows(2).find(|w| w[0] == w[1]) {
            return Err(format!(
                "{}: two targets are named `{}`; target names must be unique",
                manifest_path.display(),
                w[0]
            ));
        }
        let mut stack =
            Stack { root: root.to_path_buf(), manifest, entries: Vec::new(), problems: Vec::new() };
        if stack.visibility() == Visibility::Public {
            stack.check_public_manifest(&manifest_path);
        }
        for (dir, scope) in layer_dirs(root)? {
            stack.load_layer(&dir, &scope);
        }
        // A dir that couldn't be read leaves the stack incomplete: it doesn't load (R5-04's class, decision 4).
        if let Some(p) = stack.problems.iter().find(|p| p.code == UNREADABLE) {
            return Err(format!("{}: {}", root.display(), p.message));
        }
        Ok(stack)
    }

    /// A dir's children, sorted; an error is recorded as an `unreadable` problem, which fails the load.
    fn children(&mut self, dir: &Path) -> Vec<PathBuf> {
        match sorted_children(dir) {
            Ok(v) => v,
            Err(e) => {
                self.problems.push(Finding::error(UNREADABLE, e).at(dir, None));
                Vec::new()
            }
        }
    }

    /// A dir's plain files, as `all_files`; an error is recorded as an `unreadable` problem.
    fn files_of(&mut self, dir: &Path) -> Vec<PathBuf> {
        match all_files(dir) {
            Ok(v) => v,
            Err(e) => {
                self.problems.push(Finding::error(UNREADABLE, e).at(dir, None));
                Vec::new()
            }
        }
    }

    fn check_public_manifest(&mut self, path: &Path) {
        let m = &self.manifest;
        let private_parts = !m.targets.is_empty()
            || !m.memory_stores.is_empty()
            || m.privacy.fleet_config.is_some()
            || m.repos.values().any(|r| !r.checkouts.is_empty())
            || !m.workspaces.is_empty();
        if private_parts {
            self.problems.push(
                Finding::error("public-manifest", "targets, memory stores, checkouts and the fleet config belong in the private stack")
                    .at(path, None),
            );
        }
    }

    fn load_layer(&mut self, dir: &Path, scope: &Scope) {
        for child in self.children(dir) {
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
                for skill in self.children(&child) {
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
        for child in self.children(dir) {
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
        let text = match fsguard::read_string(file) {
            Ok(t) => t,
            Err(e) => {
                self.problems.push(Finding::error("read", e.to_string()).at(file, None));
                return;
            }
        };
        let parts = frontmatter::split(&text);
        // A file in a memory store is the agent's own: if its frontmatter isn't strict YAML (an unquoted colon
        // the agent's harness reads fine), read it line by line instead of refusing it.
        let parsed = parts.yaml.map(|y| {
            parse_yaml(y).or_else(|e| if defaults.is_some() { Ok(lenient(y)) } else { Err(e) })
        });
        let own = match parsed.transpose() {
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
        let body = match fsguard::read_string(&skill_file) {
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
        let files = self.files_of(dir);
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

    /// backbone requirement BB-8: skills tracked in a repo (stack.toml `skills` on a repo) are entries where they are: id
    /// `skill.<repo>.<dir>`, origin in-repo with their path and hash. Nothing is copied into the stack, and sync
    /// never links them. Only a dir whose `SKILL.md` git tracks is the repo's: sync's own copies, or anything else
    /// untracked in that dir, are not (orchestrator, 26 Oct). A git that fails there indexes nothing and is a
    /// problem, never an index of whatever is on disk.
    fn index_repo_skills(&mut self) {
        let repos: Vec<(String, Repo)> =
            self.manifest.repos.iter().map(|(n, r)| (n.clone(), r.clone())).collect();
        for (name, repo) in repos {
            let Some(checkout) = repo.checkouts.first().map(|c| crate::config::expand_home(c))
            else {
                continue;
            };
            for rel in &repo.skills {
                let tracked = match crate::git::run(&checkout, &["ls-files", "-z", "--", rel]) {
                    Ok(out) => out
                        .split('\0')
                        .filter(|f| !f.is_empty())
                        .map(|f| checkout.join(f))
                        .collect::<Vec<PathBuf>>(),
                    Err(e) => {
                        self.problems.push(
                            Finding::error(UNREADABLE, format!("repo {name}'s skills: {e}"))
                                .at(checkout.join(rel), None),
                        );
                        continue;
                    }
                };
                let is_tracked = |file: &Path| {
                    tracked.iter().any(|t| crate::ledger::key(t) == crate::ledger::key(file))
                };
                let dirs = self.children(&checkout.join(rel));
                for dir in dirs.into_iter().filter(|d| is_tracked(&d.join(SKILL_FILE))) {
                    let file = dir.join(SKILL_FILE);
                    // Only shown in the skills index: an unreadable SKILL.md shows no hook (inventory: lossy).
                    let body = fsguard::read_string_lossy(&file).unwrap_or_default();
                    let front = frontmatter::split(&body)
                        .yaml
                        .map(parse_yaml)
                        .and_then(Result::ok)
                        .unwrap_or_default();
                    let text = |k: &str| front.get(k).and_then(Value::as_str).map(str::to_string);
                    let dir_name = file_name(&dir);
                    let path = format!("{}/{dir_name}", rel.trim_end_matches(['/', '\\']));
                    let meta = Meta {
                        id: format!("skill.{}.{}", slug(&name), slug(&dir_name)),
                        kind: Kind::Skill,
                        title: text("description")
                            .unwrap_or_else(|| dir_name.clone())
                            .chars()
                            .take(120)
                            .collect(),
                        audience: vec![crate::model::Audience::All],
                        roles: Vec::new(),
                        scope: format!("repo:{name}"),
                        status: crate::model::Status::Active,
                        superseded_by: None,
                        retired: None,
                        visibility: self.visibility(),
                        basis: Vec::new(),
                        evidence: Vec::new(),
                        temporary: None,
                        state: None,
                        origin: crate::model::Origin::InRepo {
                            repo: name.clone(),
                            path,
                            sha256: crate::check::dir_hash_or_unreadable(&dir),
                        },
                        topics: Vec::new(),
                        overrides: Vec::new(),
                        extends: Vec::new(),
                        order: None,
                        links: Vec::new(),
                        name: text("name").or(Some(dir_name.clone())),
                        description: text("description"),
                        metadata: None,
                    };
                    let files = self.files_of(&dir);
                    self.entries.push(Entry {
                        meta,
                        scope: Scope::Repo(name.clone()),
                        stack: self.visibility(),
                        meta_file: file.clone(),
                        body_file: file,
                        body,
                        body_line: 1,
                        files,
                    });
                }
            }
        }
    }

    /// The layer dir for a scope (path scopes live in their repo's dir).
    pub fn layer_dir(&self, scope: &Scope) -> PathBuf {
        match scope {
            Scope::Global => self.root.join("global"),
            Scope::Project(p) => self.root.join("project").join(p),
            Scope::Repo(r) | Scope::Path { repo: r, .. } => self.root.join("repo").join(r),
            Scope::Workspace(w) => self.root.join("workspace").join(w),
        }
    }
}

/// When a store's defaults supply the metadata, a memory file's name gives its id, its `description` (else
/// `name`) its title, and the file name completes an imported origin.
fn derive_store_fields(merged: &mut Map<String, Value>, file: &Path) {
    let stem = file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let name = merged
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| stem.clone());
    let kind = merged.get("kind").and_then(Value::as_str).unwrap_or("memory").to_string();
    // The id comes from the file name, not the agent's `name`: an agent may reword a name, and an id never
    // changes meaning. A renamed file arrives as a new note; the old one stays.
    // With the store's name in it, the id is unique across stores (two agents may both write `notes.md`).
    let store = merged
        .get("origin")
        .and_then(|o| o.get("store"))
        .and_then(Value::as_str)
        .map(|s| format!("{}.", slug(s)))
        .unwrap_or_default();
    merged.entry("id").or_insert_with(|| Value::String(format!("{kind}.{store}{}", slug(&stem))));
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

/// Whether `s` is a slug as the entry schema's ids have them after `<kind>.`: lowercase letters, digits and `-`,
/// in dot-separated parts that each start with a letter or digit (schema/entry.schema.json, `id`).
pub fn is_slug(s: &str) -> bool {
    !s.is_empty()
        && s.split('.').all(|part| {
            part.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                && part.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        })
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

/// Top-level `key: value` lines as strings (nested lines are skipped): for agent-written frontmatter that strict
/// YAML refuses.
fn lenient(text: &str) -> Map<String, Value> {
    let mut out = Map::new();
    for line in text.lines().filter(|l| !l.starts_with([' ', '\t', '-', '#'])) {
        if let Some((k, v)) = line.split_once(':') {
            let v = v.trim().trim_matches(['"', '\'']);
            if !k.trim().is_empty() && !v.is_empty() {
                out.insert(k.trim().to_string(), Value::String(v.to_string()));
            }
        }
    }
    out
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
    match fsguard::read_string(path) {
        Ok(t) => parse_yaml(&t).map(Some).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn layer_dirs(root: &Path) -> Result<Vec<(PathBuf, Scope)>, String> {
    let mut out = vec![(root.join("global"), Scope::Global)];
    for p in sorted_children(&root.join("project"))? {
        if p.is_dir() {
            out.push((p.clone(), Scope::Project(file_name(&p))));
        }
    }
    for r in sorted_children(&root.join("repo"))? {
        if r.is_dir() {
            out.push((r.clone(), Scope::Repo(file_name(&r))));
        }
    }
    for w in sorted_children(&root.join("workspace"))? {
        if w.is_dir() {
            out.push((w.clone(), Scope::Workspace(file_name(&w))));
        }
    }
    Ok(out)
}

/// The code of a problem that a dir or file of the stack couldn't be read: the stack doesn't load.
pub const UNREADABLE: &str = "unreadable";

/// A dir's children, sorted; none for a dir that isn't there, an error for one that can't be listed.
pub fn sorted_children(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut v = fsguard::list_dir(dir)?;
    v.sort();
    Ok(v)
}

/// Every plain file under `dir`. Links (junctions, symlinks) are never followed, so a walk stays inside `dir` and
/// ends (Sol review, finding 18). Anything that can't be listed or told apart is an error, never skipped (r5,
/// R5-04's class).
pub fn all_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    for c in sorted_children(dir)? {
        if fsguard::link_at(&c)? {
            continue;
        }
        match fsguard::kind_here(&c)? {
            fsguard::Kind::Dir => out.extend(all_files(&c)?),
            fsguard::Kind::File => out.push(c),
            fsguard::Kind::Missing => {}
        }
    }
    Ok(out)
}

/// Whether `dir` holds a link anywhere below it (not following links); an error when that can't be told.
pub fn has_links(dir: &Path) -> Result<bool, String> {
    for c in sorted_children(dir)? {
        if fsguard::link_at(&c)? || (c.is_dir() && has_links(&c)?) {
            return Ok(true);
        }
    }
    Ok(false)
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
        let mut private = private.map(Stack::load).transpose()?;
        if let Some(p) = private.as_mut() {
            p.index_repo_skills();
        }
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

    pub fn declares_workspace(&self, w: &str) -> bool {
        self.all().any(|s| s.manifest.workspaces.contains_key(w))
    }

    /// The project a workspace belongs to.
    pub fn workspace_project(&self, w: &str) -> Option<String> {
        self.all().find_map(|s| s.manifest.workspaces.get(w).and_then(|x| x.project.clone()))
    }
}
