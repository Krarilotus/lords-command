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
    /// settings: guards registered as the harness's hooks (guard.rs): `rm` (PreToolUse, Bash), `queue` (Stop),
    /// `session` (SessionStart), `turn` (UserPromptSubmit), `child`, `role`, `time` (PreToolUse, editor writes and T3
    /// messages; time_guard.rs), `helper` (PreToolUse, Claude Code's native helper tool; helper_guard.rs).
    #[serde(default)]
    pub guards: Vec<String>,
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
    /// Agents write this store's own `MEMORY.md` (orchestrator, 30 Oct, item 3): memory indexes name that file, the
    /// one source, instead of listing the imported copies, which lag until the next import.
    #[serde(default)]
    pub live: bool,
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
    /// `check` refuses an entry of this stack that has neither a core line nor `start: detail` (design: Check).
    #[serde(default)]
    pub require_core: bool,
    /// The role a native subagent starts as (design: Sync, a native child's start file).
    #[serde(default)]
    pub child_role: Option<String>,
    /// Per adapter id: how its place files are composed and measured (design: Sync, place files per adapter).
    #[serde(default)]
    pub place_files: BTreeMap<String, PlaceFiles>,
    /// What `lords sync --safe-only` applies besides what is always safe (design: Sync, the safe-only stage).
    #[serde(default)]
    pub safe_only: Option<SafeOnly>,
    /// How a delivery runs, and how long every sync that applies waits for another (design: Sync, delivery).
    #[serde(default)]
    pub delivery: Option<Delivery>,
}

/// stack.toml `[delivery]` (F8, design note Harness/notes/2026-11-07-lords-f8-delivery.md §5; the owner, 7 Nov: "no
/// harded coded shit that will bite later"). Every limit a delivery has is here, with its default.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct Delivery {
    /// Which stores' commits start a delivery (their post-commit hook); none switches the trigger off.
    pub on_commit: Vec<Store>,
    /// How long a delivery, or any sync that applies, waits for a running one; also the longest a swap is tried again
    /// while a reader blocks it.
    pub lock_wait_secs: u64,
    /// Plans and applies in all, when one ends with the ledger changed under it or its new worktree not ready.
    pub attempts: u32,
    /// The pause before another attempt for a new worktree that isn't ready.
    pub retry_pause_secs: u64,
    /// From a delivery's start: no attempt, plan or place starts past it, and the lock wait ends at it.
    pub deadline_secs: u64,
    /// How long one git read of a store may take before it is stopped.
    pub store_git_secs: u64,
    /// How long a delivery waits for the state file's lock, and tries the state file's rename while a reader blocks it.
    pub state_lock_secs: u64,
    /// Whether a whole delivery first imports memory and retires ended entries, each as its own store commit.
    pub store_work: bool,
    /// The state file, relative to the tool home, in plain parts.
    pub state: String,
}

impl Default for Delivery {
    fn default() -> Delivery {
        Delivery {
            on_commit: vec![Store::Public, Store::Private],
            lock_wait_secs: 120,
            attempts: 3,
            retry_pause_secs: 2,
            deadline_secs: 600,
            store_git_secs: 30,
            state_lock_secs: 10,
            store_work: true,
            state: "state/delivery.json".into(),
        }
    }
}

impl Delivery {
    pub fn lock_wait(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.lock_wait_secs)
    }

    /// The state file's parts below the tool home; refused unless each is one plain name.
    pub fn state_parts(&self) -> Result<Vec<fsguard::Part>, String> {
        self.state.split('/').map(fsguard::Part::new).collect()
    }
}

/// A store, as `[delivery] on_commit` names it.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Store {
    Public,
    Private,
}

/// stack.toml `[safe_only]` (D13; the owner, 7 Nov: "make thigns more configurable").
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SafeOnly {
    /// Default none. Only what can't overwrite or remove an owner's bytes, and writes no destination other places share,
    /// can be named (review r1: a checkout's git exclude, which its worktrees read too, can't).
    #[serde(default)]
    pub applies: Vec<SafeAlso>,
}

/// What a stack may let `--safe-only` apply besides what is always safe.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SafeAlso {
    /// Tool-owned files where nothing is, in a declared place where the ledger records nothing yet.
    NewPlace,
}

/// One adapter's place files (stack.toml `[place_files.<adapter>]`).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlaceFiles {
    /// Whether the default role's lines for only some roles show there, labelled (default), or stay in its role file.
    #[serde(default)]
    pub default_role_lines: Option<bool>,
    /// The warning line for those files in `check`, in bytes, in place of 75 % of the adapter's limit.
    #[serde(default)]
    pub warn_bytes: Option<usize>,
    /// How those files carry the Memory section: whole (default), or as one line naming the place's
    /// `.lords/memory-<adapter>.md`, which holds it (the Lords owner, 2 Nov: Codex headroom).
    #[serde(default)]
    pub memory: MemoryForm,
}

/// A place file's Memory section (`PlaceFiles::memory`).
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MemoryForm {
    #[default]
    List,
    Pointer,
}

impl Stacks {
    /// An adapter's place-file settings: the private stack's, else the public stack's, else the defaults.
    pub fn place_files(&self, adapter: &str) -> PlaceFiles {
        self.private
            .iter()
            .chain([&self.public])
            .find_map(|s| s.manifest.place_files.get(adapter))
            .cloned()
            .unwrap_or_default()
    }

    /// The safe-only setting: the private stack's, else the public stack's, else the default (nothing more).
    pub fn safe_only(&self) -> SafeOnly {
        self.private
            .iter()
            .chain([&self.public])
            .find_map(|s| s.manifest.safe_only.clone())
            .unwrap_or_default()
    }

    /// The delivery settings: the private stack's, else the public stack's, else the defaults.
    pub fn delivery(&self) -> Delivery {
        self.private
            .iter()
            .chain([&self.public])
            .find_map(|s| s.manifest.delivery.clone())
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug)]
pub struct Stack {
    pub root: PathBuf,
    pub manifest: Manifest,
    pub entries: Vec<Entry>,
    /// Problems found while loading (schema, placement, unreadable files).
    pub problems: Vec<Finding>,
    /// The committed tree it was loaded from (`Stacks::load_captured`), which sync reads a skill's files from too;
    /// `None` for a stack loaded from its dir on disk.
    pub(crate) captured: Option<std::sync::Arc<crate::capture::Captured>>,
}

impl Stack {
    pub fn visibility(&self) -> Visibility {
        self.manifest.visibility
    }

    /// Loads a stack. Fails only when `stack.toml` is missing or invalid; entry problems are collected.
    pub fn load(root: &Path) -> Result<Stack, String> {
        Self::load_from(root, &Fs)
    }

    /// Loads the stack at `root` from `src`: every file, dir listing and kind the loader asks about comes from it.
    pub(crate) fn load_from(root: &Path, src: &dyn Source) -> Result<Stack, String> {
        let manifest_path = root.join(MANIFEST);
        let text =
            src.read(&manifest_path).map_err(|e| format!("{}: {e}", manifest_path.display()))?;
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
        // No two targets at one path, or one inside the other: an item of both could be applied for one and held for
        // the other (D24; F8, design note C8).
        let at: Vec<(&str, String)> = manifest
            .targets
            .iter()
            .map(|t| (t.name.as_str(), crate::ledger::key(&crate::config::expand_home(&t.path))))
            .collect();
        for (i, (a, pa)) in at.iter().enumerate() {
            for (b, pb) in &at[i + 1..] {
                let inside =
                    |x: &str, y: &str| y.starts_with(&format!("{}/", x.trim_end_matches('/')));
                if pa == pb || inside(pa, pb) || inside(pb, pa) {
                    return Err(format!(
                        "{}: targets `{a}` and `{b}` are at one path, or one lies inside the other; give each its own",
                        manifest_path.display()
                    ));
                }
            }
        }
        if let Some(d) = &manifest.delivery {
            d.state_parts().map_err(|e| {
                format!(
                    "{}: [delivery] state = {:?}: a path below the tool home, in plain parts ({e})",
                    manifest_path.display(),
                    d.state
                )
            })?;
        }
        let mut stack = Stack {
            root: root.to_path_buf(),
            manifest,
            entries: Vec::new(),
            problems: Vec::new(),
            captured: None,
        };
        if stack.visibility() == Visibility::Public {
            stack.check_public_manifest(&manifest_path);
        }
        for (dir, scope) in layer_dirs(src, root)? {
            stack.load_layer(src, &dir, &scope);
        }
        // A dir that couldn't be read leaves the stack incomplete: it doesn't load (R5-04's class, decision 4).
        if let Some(p) = stack.problems.iter().find(|p| p.code == UNREADABLE) {
            return Err(format!("{}: {}", root.display(), p.message));
        }
        Ok(stack)
    }

    /// A dir's children, sorted; an error is recorded as an `unreadable` problem, which fails the load.
    fn children(&mut self, src: &dyn Source, dir: &Path) -> Vec<PathBuf> {
        match sorted_children_in(src, dir) {
            Ok(v) => v,
            Err(e) => {
                self.problems.push(Finding::error(UNREADABLE, e).at(dir, None));
                Vec::new()
            }
        }
    }

    /// A dir's plain files, as `all_files`; an error is recorded as an `unreadable` problem.
    fn files_of(&mut self, src: &dyn Source, dir: &Path) -> Vec<PathBuf> {
        match all_files_in(src, dir) {
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

    fn load_layer(&mut self, src: &dyn Source, dir: &Path, scope: &Scope) {
        for child in self.children(src, dir) {
            let name = file_name(&child);
            if src.is_file(&child) {
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
                for skill in self.children(src, &child) {
                    if src.is_dir(&skill) {
                        self.load_skill(src, &skill, scope);
                    } else {
                        self.problems.push(
                            Finding::warning("layout", "a file directly in skills/")
                                .at(&skill, None),
                        );
                    }
                }
            } else {
                self.load_kind_dir(src, &child, kind, scope);
            }
        }
    }

    fn load_kind_dir(&mut self, src: &dyn Source, dir: &Path, kind: Kind, scope: &Scope) {
        let defaults = match read_yaml_file(src, &dir.join(DEFAULTS)) {
            Ok(v) => v.map(|(m, _)| m),
            Err(e) => {
                self.problems.push(Finding::error("schema", e).at(dir.join(DEFAULTS), None));
                return;
            }
        };
        for child in self.children(src, dir) {
            let name = file_name(&child);
            let not_entry = name == DEFAULTS
                || name == ".gitkeep"
                || (defaults.is_some() && INDEX_FILES.contains(&name.as_str()));
            if src.is_dir(&child) {
                self.load_kind_dir(src, &child, kind, scope);
            } else if not_entry {
                continue;
            } else if name.ends_with(".md") {
                self.load_markdown(src, &child, kind, scope, defaults.as_ref());
            } else {
                self.problems.push(
                    Finding::warning("layout", format!("not an entry: {name}")).at(&child, None),
                );
            }
        }
    }

    fn load_markdown(
        &mut self,
        src: &dyn Source,
        file: &Path,
        kind: Kind,
        scope: &Scope,
        defaults: Option<&Map<String, Value>>,
    ) {
        let text = match src.read(file) {
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
            crate::model::version(&[text.as_bytes()]),
        );
    }

    fn load_skill(&mut self, src: &dyn Source, dir: &Path, scope: &Scope) {
        let sidecar = dir.join(SIDECAR);
        let (meta, meta_text) = match read_yaml_file(src, &sidecar) {
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
        let body = match src.read(&skill_file) {
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
        let files = self.files_of(src, dir);
        let version = crate::model::version(&[meta_text.as_bytes(), body.as_bytes()]);
        self.push_entry(meta, Kind::Skill, scope, &sidecar, &skill_file, body, 1, files, version);
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
        version: String,
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
            version,
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
                // A repo's own skills are read from its checkout, whatever the stack's source: they are the repo's.
                let dirs = self.children(&Fs, &checkout.join(rel));
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
                        start: None,
                        core: None,
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
                    let files = self.files_of(&Fs, &dir);
                    self.entries.push(Entry {
                        meta,
                        scope: Scope::Repo(name.clone()),
                        stack: self.visibility(),
                        meta_file: file.clone(),
                        body_file: file,
                        version: crate::model::version(&[body.as_bytes()]),
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

/// A YAML file's mapping, and the text it was read from.
type YamlFile = (Map<String, Value>, String);

fn read_yaml_file(src: &dyn Source, path: &Path) -> Result<Option<YamlFile>, String> {
    match src.read(path) {
        Ok(t) => {
            parse_yaml(&t).map(|m| Some((m, t))).map_err(|e| format!("{}: {e}", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn layer_dirs(src: &dyn Source, root: &Path) -> Result<Vec<(PathBuf, Scope)>, String> {
    let mut out = vec![(root.join("global"), Scope::Global)];
    for p in sorted_children_in(src, &root.join("project"))? {
        if src.is_dir(&p) {
            out.push((p.clone(), Scope::Project(file_name(&p))));
        }
    }
    for r in sorted_children_in(src, &root.join("repo"))? {
        if src.is_dir(&r) {
            out.push((r.clone(), Scope::Repo(file_name(&r))));
        }
    }
    for w in sorted_children_in(src, &root.join("workspace"))? {
        if src.is_dir(&w) {
            out.push((w.clone(), Scope::Workspace(file_name(&w))));
        }
    }
    Ok(out)
}

/// The code of a problem that a dir or file of the stack couldn't be read: the stack doesn't load.
pub const UNREADABLE: &str = "unreadable";

/// Where the loader reads a stack from (gap 11, the captured-source refactor): its dir on disk (`Fs`), or one tree a
/// store committed (`capture::Captured`), which has no way back to the disk. Every read, listing and kind the loader
/// asks about a stack's files goes through one; a declared repo's own skills are read from its checkout (`Fs`)
/// whatever the stack's source.
pub(crate) trait Source {
    /// A file's text; an error of kind `NotFound` where nothing is there.
    fn read(&self, path: &Path) -> std::io::Result<String>;
    /// A dir's children, in no order; none for a dir that isn't there, an error for one that can't be listed.
    fn list(&self, dir: &Path) -> Result<Vec<PathBuf>, String>;
    /// Whether a dir is at `path`, as `Path::is_dir` tells (on disk a link to a dir is one).
    fn is_dir(&self, path: &Path) -> bool;
    /// Whether a plain file is at `path`, as `Path::is_file` tells.
    fn is_file(&self, path: &Path) -> bool;
    /// As `fsguard::link_at`.
    fn link_at(&self, path: &Path) -> Result<bool, String>;
    /// As `fsguard::kind_here`.
    fn kind_here(&self, path: &Path) -> Result<fsguard::Kind, String>;
}

/// The stack as its dir on disk holds it: the loader's reads as they were before there was a `Source`.
pub(crate) struct Fs;

impl Source for Fs {
    fn read(&self, path: &Path) -> std::io::Result<String> {
        fsguard::read_string(path)
    }
    fn list(&self, dir: &Path) -> Result<Vec<PathBuf>, String> {
        fsguard::list_dir(dir)
    }
    fn is_dir(&self, path: &Path) -> bool {
        path.is_dir()
    }
    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }
    fn link_at(&self, path: &Path) -> Result<bool, String> {
        fsguard::link_at(path)
    }
    fn kind_here(&self, path: &Path) -> Result<fsguard::Kind, String> {
        fsguard::kind_here(path)
    }
}

/// A dir's children, sorted; none for a dir that isn't there, an error for one that can't be listed.
pub fn sorted_children(dir: &Path) -> Result<Vec<PathBuf>, String> {
    sorted_children_in(&Fs, dir)
}

fn sorted_children_in(src: &dyn Source, dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut v = src.list(dir)?;
    v.sort();
    Ok(v)
}

/// Every plain file under `dir`. Links (junctions, symlinks) are never followed, so a walk stays inside `dir` and
/// ends (Sol review, finding 18). Anything that can't be listed or told apart is an error, never skipped (r5,
/// R5-04's class).
pub fn all_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    all_files_in(&Fs, dir)
}

fn all_files_in(src: &dyn Source, dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    for c in sorted_children_in(src, dir)? {
        if src.link_at(&c)? {
            continue;
        }
        match src.kind_here(&c)? {
            fsguard::Kind::Dir => out.extend(all_files_in(src, &c)?),
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
        let private = private.map(|p| move || Stack::load(p));
        Self::load_with(|| Stack::load(public), private, true)
    }

    /// The two stacks as their stores committed them (`capture::Captured`), read from nothing else. `repo_skills`:
    /// whether the declared repos' own skills are indexed from their checkouts, as `load` does; a start doesn't take
    /// them (gap 11 design note §0c.4), so the store commit check leaves them out.
    pub fn load_captured(
        public: crate::capture::Captured,
        private: Option<crate::capture::Captured>,
        repo_skills: bool,
    ) -> Result<Stacks, String> {
        fn kept(c: crate::capture::Captured) -> Result<Stack, String> {
            let mut s = Stack::load_from(c.root(), &c)?;
            s.captured = Some(std::sync::Arc::new(c));
            Ok(s)
        }
        Self::load_with(|| kept(public), private.map(|p| move || kept(p)), repo_skills)
    }

    /// The committed tree `e`'s files are read from, where its stack was loaded from one and the entry is the
    /// stack's own (a declared repo's skill is its checkout's).
    pub(crate) fn captured_for(
        &self,
        e: &Entry,
    ) -> Option<std::sync::Arc<crate::capture::Captured>> {
        let own = !matches!(e.meta.origin, crate::model::Origin::InRepo { .. });
        self.stack(e.stack).and_then(|s| s.captured.clone()).filter(|_| own)
    }

    /// Loads the public stack, checks it, then the private one, in that order (whose error comes first).
    fn load_with(
        public: impl FnOnce() -> Result<Stack, String>,
        private: Option<impl FnOnce() -> Result<Stack, String>>,
        repo_skills: bool,
    ) -> Result<Stacks, String> {
        let public = public()?;
        if public.visibility() != Visibility::Public {
            return Err(format!(
                "{}: the public stack's stack.toml says visibility = private",
                public.root.display()
            ));
        }
        let mut private = private.map(|load| load()).transpose()?;
        if let Some(p) = private.as_mut().filter(|_| repo_skills) {
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
