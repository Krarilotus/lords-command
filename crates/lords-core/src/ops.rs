//! The commands that change a stack: `retire`, `import`, `promote`. Each commits what it changed, so the stack's
//! git log is the history; nothing is deleted silently.

use crate::config::expand_home;
use crate::frontmatter::{self, quote};
use crate::git;
use crate::model::{Entry, Kind, Origin, Scope, Visibility};
use crate::privacy::{self, Hit, Rules};
use crate::stack::{self, Stack, Stacks, DEFAULTS, SIDECAR};
use std::fs;
use std::path::{Path, PathBuf};

/// Today's date (UTC) as YYYY-MM-DD.
pub fn today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    // Civil date from days since 1970-01-01 (H. Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

fn is_repo(dir: &Path) -> bool {
    git::toplevel(dir).is_some()
}

fn rel<'a>(root: &Path, p: &'a Path) -> &'a Path {
    p.strip_prefix(root).unwrap_or(p)
}

/// Writes keys into an entry's metadata file: frontmatter for Markdown, the whole file for a sidecar.
fn set_meta_keys(meta_file: &Path, keys: &[(&str, Option<String>)]) -> Result<(), String> {
    let text =
        fs::read_to_string(meta_file).map_err(|e| format!("{}: {e}", meta_file.display()))?;
    let new = if stack::file_name(meta_file) == SIDECAR {
        frontmatter::set_keys(&text, keys)
    } else {
        frontmatter::rewrite(&text, keys)
    };
    fs::write(meta_file, new).map_err(|e| format!("{}: {e}", meta_file.display()))
}

// ---------------------------------------------------------------- retire

pub struct Retire<'a> {
    pub id: &'a str,
    pub reason: &'a str,
    pub superseded_by: Option<&'a str>,
    pub date: String,
    pub commit: bool,
}

#[derive(Debug)]
pub struct Retired {
    pub file: PathBuf,
    pub commit: Option<String>,
}

pub fn retire(stacks: &Stacks, r: &Retire) -> Result<Retired, String> {
    let e = stacks.find(r.id).ok_or_else(|| format!("no entry `{}`", r.id))?;
    if !e.active() {
        return Err(format!("`{}` is already retired", r.id));
    }
    if r.reason.trim().is_empty() {
        return Err("retiring needs a reason".into());
    }
    if let Some(s) = r.superseded_by {
        let succ = stacks.find(s).ok_or_else(|| format!("no successor `{s}`"))?;
        if s == r.id {
            return Err("an entry can't supersede itself".into());
        }
        if !succ.active() {
            return Err(format!("the successor `{s}` is retired"));
        }
        if e.stack == Visibility::Public && succ.stack == Visibility::Private {
            return Err(format!("a public entry can't name the private `{s}` as its successor"));
        }
    }
    let before = fs::read(&e.meta_file).map_err(|e| e.to_string())?;
    let keys = [
        ("status", Some("retired".to_string())),
        ("superseded_by", r.superseded_by.map(str::to_string)),
        ("retired", Some(format!("date: {}\nreason: {}", r.date, quote(r.reason)))),
    ];
    set_meta_keys(&e.meta_file, &keys)?;
    // The stack must still load the entry, now retired; otherwise put the file back.
    let root = stacks.stack(e.stack).expect("the entry's stack is loaded").root.clone();
    let ok =
        Stack::load(&root).ok().and_then(|s| s.find(r.id).map(|x| !x.active())).unwrap_or(false);
    if !ok {
        fs::write(&e.meta_file, before).map_err(|e| e.to_string())?;
        return Err(format!("retiring `{}` would leave invalid metadata; nothing changed", r.id));
    }
    let commit = if r.commit && is_repo(&root) {
        git::commit(&root, &[rel(&root, &e.meta_file)], &format!("Retire {}: {}", r.id, r.reason))?
    } else {
        None
    };
    Ok(Retired { file: e.meta_file.clone(), commit })
}

// ---------------------------------------------------------------- import

/// The keys Lords Command adds to a memory file; an import keeps them when the agent's copy changes.
const LORDS_KEYS: [&str; 16] = [
    "id",
    "kind",
    "title",
    "audience",
    "scope",
    "status",
    "superseded_by",
    "retired",
    "visibility",
    "basis",
    "origin",
    "topics",
    "overrides",
    "extends",
    "order",
    "links",
];

#[derive(Debug, Default)]
pub struct Imported {
    pub store: String,
    pub added: Vec<String>,
    pub changed: Vec<String>,
    pub unchanged: usize,
    /// In the stack but no longer in the agent's dir: kept, reported.
    pub absent_in_source: Vec<String>,
    pub commit: Option<String>,
}

/// The agent's new text, with the Lords Command keys of the stack's copy carried over.
fn carry_keys(incoming: &str, existing: &str) -> String {
    let Some(old_yaml) = frontmatter::split(existing).yaml else { return incoming.to_string() };
    let keys: Vec<(&str, Option<String>)> = LORDS_KEYS
        .iter()
        .filter_map(|k| frontmatter::get_key(old_yaml, k).map(|v| (*k, Some(v))))
        .collect();
    if keys.is_empty() {
        incoming.to_string()
    } else {
        frontmatter::rewrite(incoming, &keys)
    }
}

pub fn import(private: &Stack, only: Option<&str>, commit: bool) -> Result<Vec<Imported>, String> {
    import_stores(private, only, true, commit)
}

/// What `import` would copy, without writing anything (sync's dry run shows it).
pub fn import_preview(private: &Stack, only: Option<&str>) -> Result<Vec<Imported>, String> {
    import_stores(private, only, false, false)
}

fn import_stores(
    private: &Stack,
    only: Option<&str>,
    write: bool,
    commit: bool,
) -> Result<Vec<Imported>, String> {
    if private.visibility() != Visibility::Private {
        return Err("memory imports go into the private stack".into());
    }
    let stores: Vec<_> = private
        .manifest
        .memory_stores
        .iter()
        .filter(|s| only.is_none_or(|o| o == s.name))
        .collect();
    if stores.is_empty() {
        return Err(match only {
            Some(o) => format!("no memory store `{o}` in the private stack.toml"),
            None => "the private stack.toml declares no memory_stores".into(),
        });
    }
    let mut reports = Vec::new();
    for s in stores {
        let scope = Scope::parse(&s.scope)
            .ok_or_else(|| format!("store `{}`: bad scope `{}`", s.name, s.scope))?;
        let src = expand_home(&s.path);
        if !src.is_dir() {
            return Err(format!("store `{}`: {} is not a dir", s.name, src.display()));
        }
        let dest = private.layer_dir(&scope).join("memory").join(&s.name);
        if write {
            fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
        }
        let defaults = dest.join(DEFAULTS);
        if write && !defaults.exists() {
            let audience =
                if s.audience.is_empty() { "all".to_string() } else { s.audience.join(", ") };
            let text = format!(
                "# Metadata every memory file in this store shares (lords import).\nkind: memory\nscope: {}\naudience: [{audience}]\nstatus: active\nvisibility: private\norigin:\n  type: imported\n  store: {}\n",
                s.scope, s.name
            );
            fs::write(&defaults, text).map_err(|e| e.to_string())?;
        }
        let mut rep = Imported { store: s.name.clone(), ..Imported::default() };
        let mut seen = Vec::new();
        for f in stack::sorted_children(&src).into_iter().filter(|f| f.is_file()) {
            let name = stack::file_name(&f);
            if !name.ends_with(".md") {
                continue;
            }
            seen.push(name.clone());
            let incoming = fs::read_to_string(&f).map_err(|e| format!("{}: {e}", f.display()))?;
            let target = dest.join(&name);
            match fs::read_to_string(&target) {
                Err(_) => {
                    if write {
                        fs::write(&target, &incoming).map_err(|e| e.to_string())?;
                    }
                    rep.added.push(name);
                }
                Ok(existing) => {
                    let merged = carry_keys(&incoming, &existing);
                    if merged == existing {
                        rep.unchanged += 1;
                    } else {
                        if write {
                            fs::write(&target, merged).map_err(|e| e.to_string())?;
                        }
                        rep.changed.push(name);
                    }
                }
            }
        }
        for f in stack::sorted_children(&dest) {
            let name = stack::file_name(&f);
            if name.ends_with(".md") && !seen.contains(&name) {
                rep.absent_in_source.push(name);
            }
        }
        if commit && is_repo(&private.root) && (!rep.added.is_empty() || !rep.changed.is_empty()) {
            let msg = format!(
                "Import memory from {}: {} new, {} changed",
                s.name,
                rep.added.len(),
                rep.changed.len()
            );
            rep.commit = git::commit(&private.root, &[rel(&private.root, &dest)], &msg)?;
        }
        reports.push(rep);
    }
    Ok(reports)
}

// ---------------------------------------------------------------- promote

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Promote {
    /// Move only if nothing private is found.
    Clean,
    /// Replace each finding with `<private:class>` for the owner to reword.
    Strip,
    /// Move the lines with findings into a private entry that extends the public one.
    Split,
}

#[derive(Debug)]
pub struct Promoted {
    pub to: PathBuf,
    pub stripped: usize,
    pub private_part: Option<PathBuf>,
    pub commits: Vec<String>,
}

fn findings_text(hits: &[(PathBuf, Hit)]) -> String {
    hits.iter()
        .map(|(p, h)| format!("\n  {}:{} {}: `{}`", p.display(), h.line, h.class.name(), h.text))
        .collect()
}

pub fn promote(
    stacks: &Stacks,
    id: &str,
    mode: Promote,
    rules: &Rules,
    commit: bool,
) -> Result<Promoted, String> {
    let e = stacks.find(id).ok_or_else(|| format!("no entry `{id}`"))?;
    let private = stacks.private.as_ref().ok_or("no private stack loaded")?;
    let public = &stacks.public;
    if e.stack != Visibility::Private {
        return Err(format!("`{id}` is already public"));
    }
    if e.meta.kind == Kind::Memory {
        return Err("a memory isn't promoted as is: make it an intent, rule or fact entry first, then retire the memory".into());
    }
    let refs = e
        .meta
        .overrides
        .iter()
        .chain(&e.meta.extends)
        .chain(&e.meta.superseded_by)
        .chain(&e.meta.links);
    let private_refs: Vec<&String> =
        refs.filter(|r| stacks.find(r).is_some_and(|t| t.stack == Visibility::Private)).collect();
    if !private_refs.is_empty() {
        return Err(format!(
            "`{id}` names private entries ({}); a public entry can't",
            private_refs.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
        ));
    }
    let undeclared = match &e.scope {
        Scope::Project(p) if !public.manifest.projects.contains_key(p) => {
            Some(format!("project `{p}`"))
        }
        Scope::Repo(r) | Scope::Path { repo: r, .. } if !public.manifest.repos.contains_key(r) => {
            Some(format!("repo `{r}`"))
        }
        _ => None,
    };
    if let Some(what) = undeclared {
        return Err(format!(
            "declare {what} in the public stack.toml first (its name becomes public)"
        ));
    }

    let is_skill = e.meta.kind == Kind::Skill;
    let src_path = if is_skill {
        e.meta_file.parent().expect("a sidecar has a dir").to_path_buf()
    } else {
        e.meta_file.clone()
    };
    let dest_path = public.root.join(rel(&private.root, &src_path));
    if dest_path.exists() {
        return Err(format!("{} already exists", dest_path.display()));
    }
    let mut hits: Vec<(PathBuf, Hit)> = Vec::new();
    for f in &e.files {
        let text = fs::read_to_string(f).unwrap_or_default();
        hits.extend(privacy::scan(&text, rules).into_iter().map(|h| (f.clone(), h)));
    }
    if !hits.is_empty() {
        match mode {
            Promote::Clean => {
                return Err(format!(
                    "private details found; use --strip or --split:{}",
                    findings_text(&hits)
                ))
            }
            Promote::Split if is_skill => {
                return Err(format!("a skill can't be split; use --strip:{}", findings_text(&hits)))
            }
            _ if matches!(e.meta.origin, Origin::Inherited(_)) => {
                return Err(format!(
                    "an inherited entry stays verbatim, so it can't be stripped:{}",
                    findings_text(&hits)
                ))
            }
            _ => {}
        }
    }

    // Build the public copy in memory first.
    let mut files: Vec<(PathBuf, String)> = Vec::new();
    let mut private_part = None;
    for f in &e.files {
        let text = fs::read_to_string(f).unwrap_or_default();
        let mine: Vec<Hit> = hits.iter().filter(|(p, _)| p == f).map(|(_, h)| h.clone()).collect();
        let text = match mode {
            Promote::Split if !mine.is_empty() => {
                if mine.iter().any(|h| h.line < e.body_line) {
                    return Err(format!(
                        "the frontmatter itself has private details; use --strip:{}",
                        findings_text(&hits)
                    ));
                }
                let lines: Vec<usize> = mine.iter().map(|h| h.line).collect();
                let (mut kept, mut moved) = (String::new(), String::new());
                for (i, l) in text.split_inclusive('\n').enumerate() {
                    if lines.contains(&(i + 1)) { &mut moved } else { &mut kept }.push_str(l);
                }
                private_part = Some(split_part(e, &moved));
                kept
            }
            _ => privacy::strip(&text, &mine),
        };
        files.push((f.clone(), text));
    }
    for (f, text) in files.iter_mut() {
        if *f == e.meta_file {
            let keys = [("visibility", Some("public".to_string()))];
            *text = if is_skill {
                frontmatter::set_keys(text, &keys)
            } else {
                frontmatter::rewrite(text, &keys)
            };
        }
        let left = privacy::scan(text, rules);
        if !left.is_empty() {
            return Err(format!("still private after {mode:?} in {}: {:?}", f.display(), left));
        }
    }

    // Write: public copy, then the private part, then remove the private original.
    for (f, text) in &files {
        let to = public.root.join(rel(&private.root, f));
        fs::create_dir_all(to.parent().expect("a file has a parent")).map_err(|e| e.to_string())?;
        fs::write(&to, text).map_err(|e| e.to_string())?;
    }
    let mut part_path = None;
    if let Some((name, text)) = private_part {
        let p = e.meta_file.with_file_name(name);
        fs::write(&p, text).map_err(|e| e.to_string())?;
        part_path = Some(p);
    }
    if is_skill {
        fs::remove_dir_all(&src_path).map_err(|e| e.to_string())?;
    } else {
        fs::remove_file(&src_path).map_err(|e| e.to_string())?;
    }
    let mut commits = Vec::new();
    if commit {
        let mut paths = vec![rel(&private.root, &src_path).to_path_buf()];
        paths.extend(part_path.iter().map(|p| rel(&private.root, p).to_path_buf()));
        if is_repo(&private.root) {
            let refs: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
            commits.extend(git::commit(
                &private.root,
                &refs,
                &format!("Promote {id} to the public stack"),
            )?);
        }
        if is_repo(&public.root) {
            let r = rel(&public.root, &dest_path);
            commits.extend(git::commit(
                &public.root,
                &[r],
                &format!("Add {id}, promoted from the private stack"),
            )?);
        }
    }
    let stripped = if mode == Promote::Strip { hits.len() } else { 0 };
    Ok(Promoted { to: dest_path, stripped, private_part: part_path, commits })
}

/// The private entry that keeps a split's lines: same place, extends the public entry.
fn split_part(e: &Entry, lines: &str) -> (String, String) {
    let stem =
        e.meta_file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let audience = e
        .meta
        .audience
        .iter()
        .map(|a| {
            serde_json::to_value(a)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(", ");
    let text = format!(
        "---\nid: {id}.private\nkind: {kind}\ntitle: {title}\naudience: [{audience}]\nscope: {scope}\nstatus: active\nvisibility: private\norigin:\n  type: own\nextends: [{id}]\n---\n{lines}",
        id = e.id(),
        kind = e.meta.kind.name(),
        title = quote(&format!("{} (private detail)", e.meta.title)),
        scope = e.meta.scope,
    );
    (format!("{stem}.private.md"), text)
}

// ---------------------------------------------------------------- edit

/// What an edit changes; `None` leaves a field as it is.
#[derive(Clone, Debug, Default)]
pub struct Edit {
    pub title: Option<String>,
    pub audience: Option<Vec<String>>,
    pub topics: Option<Vec<String>>,
    pub roles: Option<Vec<String>>,
    pub body: Option<String>,
}

fn yaml_list(items: &[String]) -> String {
    format!("[{}]", items.join(", "))
}

/// Changes an entry's title, audience, topics or text, then checks that it still loads and commits. Refused: an
/// inherited entry's text (it stays verbatim), a memory's text (the agent's write path owns it), and topics the
/// entry's stack doesn't declare.
pub fn edit(
    stacks: &Stacks,
    id: &str,
    change: &Edit,
    commit: bool,
) -> Result<Option<String>, String> {
    let e = stacks.find(id).ok_or_else(|| format!("no entry `{id}`"))?;
    if change.body.is_some() {
        if matches!(e.meta.origin, Origin::Inherited(_)) {
            return Err(
                "an inherited entry stays verbatim; adapt it into a new entry to change its text"
                    .into(),
            );
        }
        if e.meta.kind == Kind::Memory {
            return Err(
                "a memory's text belongs to the agent that wrote it; promote it to change it"
                    .into(),
            );
        }
    }
    let stack = stacks.stack(e.stack).expect("the entry's stack is loaded");
    if let Some(roles) = &change.roles {
        let declared = |r: &String| {
            stacks.public.manifest.roles.contains(r)
                || (e.stack == Visibility::Private && stack.manifest.roles.contains(r))
        };
        if let Some(r) = roles.iter().find(|r| !declared(r)) {
            return Err(format!("role `{r}` isn't in the {} stack's roles list", e.stack));
        }
    }
    if let Some(topics) = &change.topics {
        let declared = |t: &String| {
            stacks.public.manifest.topics.contains(t)
                || (e.stack == Visibility::Private && stack.manifest.topics.contains(t))
        };
        if let Some(t) = topics.iter().find(|t| !declared(t)) {
            return Err(format!("topic `{t}` isn't in the {} stack's topics list", e.stack));
        }
    }
    let mut keys: Vec<(&str, Option<String>)> = Vec::new();
    if let Some(t) = &change.title {
        keys.push(("title", Some(quote(t))));
    }
    if let Some(a) = &change.audience {
        keys.push(("audience", Some(yaml_list(a))));
    }
    if let Some(t) = &change.topics {
        keys.push(("topics", (!t.is_empty()).then(|| yaml_list(t))));
    }
    if let Some(r) = &change.roles {
        keys.push(("roles", (!r.is_empty()).then(|| yaml_list(r))));
    }
    let files: Vec<PathBuf> = if e.meta_file == e.body_file {
        vec![e.meta_file.clone()]
    } else {
        vec![e.meta_file.clone(), e.body_file.clone()]
    };
    let before: Vec<Vec<u8>> = files.iter().map(|f| fs::read(f).unwrap_or_default()).collect();
    let restore = || {
        for (f, b) in files.iter().zip(&before) {
            let _ = fs::write(f, b);
        }
    };
    if !keys.is_empty() {
        set_meta_keys(&e.meta_file, &keys)?;
    }
    if let Some(body) = &change.body {
        let text = fs::read_to_string(&e.body_file).map_err(|err| err.to_string())?;
        let new = if e.body_file == e.meta_file {
            let parts = frontmatter::split(&text);
            let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
            format!("---{nl}{}---{nl}{body}", parts.yaml.unwrap_or(""))
        } else {
            body.clone()
        };
        fs::write(&e.body_file, new).map_err(|err| err.to_string())?;
    }
    // The entry must still load, with no new problem in its files; otherwise put them back.
    let reloaded = Stack::load(&stack.root);
    let broken = match &reloaded {
        Err(err) => Some(err.clone()),
        Ok(s) if s.find(id).is_none() => Some("the entry no longer loads".into()),
        Ok(s) => s
            .problems
            .iter()
            .find(|p| p.file.as_ref().is_some_and(|f| files.contains(f)))
            .map(|p| p.message.clone()),
    };
    if let Some(why) = broken {
        restore();
        return Err(format!("that edit would break `{id}` ({why}); nothing changed"));
    }
    if commit && is_repo(&stack.root) {
        let rels: Vec<&Path> = files.iter().map(|f| rel(&stack.root, f)).collect();
        return git::commit(&stack.root, &rels, &format!("Edit {id}"));
    }
    Ok(None)
}
