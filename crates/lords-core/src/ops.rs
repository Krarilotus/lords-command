//! The commands that change a stack: `retire`, `import`, `promote`. Each commits what it changed, so the stack's
//! git log is the history; nothing is deleted silently.

use crate::config::expand_home;
use crate::frontmatter::{self, quote};
use crate::fsguard;
use crate::git;
use crate::model::{Entry, Kind, Origin, Scope, Visibility};
use crate::privacy::{self, Hit, Rules};
use crate::stack::{self, Stack, Stacks, DEFAULTS, SIDECAR};
use std::path::{Path, PathBuf};

fn unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Today's date (UTC) as YYYY-MM-DD.
pub fn today() -> String {
    date_of((unix_secs() / 86_400) as i64)
}

/// Now (UTC) as YYYY-MM-DDTHH:MM:SSZ: a journal's time stamps.
pub fn now() -> String {
    let secs = unix_secs();
    let s = secs % 86_400;
    format!("{}T{:02}:{:02}:{:02}Z", date_of((secs / 86_400) as i64), s / 3600, s / 60 % 60, s % 60)
}

/// The civil date `days` after 1970-01-01.
fn date_of(days: i64) -> String {
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

/// Whether git keeps every byte that changing `path` replaces (design: Ownership boundary; Sol review r2, R2-05: a
/// clean status alone proves nothing for an ignored file). Every operation checks each file it will change once,
/// before its first write, whether it commits or not (Sol review r3, R3-10): git is the stack's backup, so a stack
/// edit replaces only bytes git holds. Outside git, only a new file can be written.
fn git_keeps(path: &Path) -> Result<(), String> {
    let dir = path.parent().ok_or_else(|| format!("{}: no parent dir", path.display()))?;
    let Some(top) = dir.exists().then(|| git::toplevel(dir)).flatten() else {
        return match fsguard::kind(path)? {
            fsguard::Kind::Missing => Ok(()),
            _ => Err(format!(
                "{}: not in a git repo, so nothing could give back what this replaces; a stack lives in git",
                path.display()
            )),
        };
    };
    let name = rel(&top, path).to_string_lossy().replace('\\', "/");
    match fsguard::kind(path)? {
        fsguard::Kind::File => {
            if !git::status(&top, &["ls-files", "--error-unmatch", "--", &name])? {
                return Err(format!(
                    "{}: git doesn't track it, so it couldn't give back what this replaces; commit it first",
                    path.display()
                ));
            }
            if !git::status(&top, &["diff", "--quiet", "HEAD", "--", &name])? {
                return Err(format!(
                    "{}: has uncommitted changes; commit them first",
                    path.display()
                ));
            }
        }
        fsguard::Kind::Missing => {
            if git::status(&top, &["check-ignore", "-q", "--", &name])? {
                return Err(format!(
                    "{}: git ignores it, so it couldn't be committed",
                    path.display()
                ));
            }
        }
        fsguard::Kind::Dir => return Err(format!("{} is a dir", path.display())),
    }
    Ok(())
}

/// A stack file as a place below its stack's verified root (design-simplification-r6, rule A): a path outside
/// the stack, or with a part that is no plain name, is refused (Sol review r6, R6-01).
fn stack_place(root: &Path, path: &Path) -> Result<fsguard::Contained, String> {
    fsguard::Root::stack(root)?
        .below(path)
        .ok_or_else(|| format!("{} lies outside the stack {}", path.display(), root.display()))
}

/// Writes a stack file through `fsguard`, below the stack's root (the operation has checked it with `git_keeps`
/// first).
fn stack_write(root: &Path, path: &Path, bytes: &[u8]) -> Result<(), String> {
    fsguard::replace_own_file(&stack_place(root, path)?, bytes)
}

/// The parts of a scope's layer dir below a stack's root, each a plain name, the scope's own a slug (R6-01: a
/// scope `project:../../escaped` is no layer).
fn layer_parts(scope: &Scope) -> Result<Vec<fsguard::Part>, String> {
    let named = |kind: &str, name: &str| -> Result<Vec<fsguard::Part>, String> {
        Ok(vec![fsguard::Part::new(kind)?, fsguard::Part::skill(name)?])
    };
    match scope {
        Scope::Global => Ok(vec![fsguard::Part::new("global")?]),
        Scope::Project(p) => named("project", p),
        Scope::Repo(r) | Scope::Path { repo: r, .. } => named("repo", r),
        Scope::Workspace(w) => named("workspace", w),
    }
}

/// Writes keys into an entry's metadata file: frontmatter for Markdown, the whole file for a sidecar.
fn set_meta_keys(
    root: &Path,
    meta_file: &Path,
    keys: &[(&str, Option<String>)],
) -> Result<(), String> {
    let text =
        fsguard::read_string(meta_file).map_err(|e| format!("{}: {e}", meta_file.display()))?;
    let new = if stack::file_name(meta_file) == SIDECAR {
        frontmatter::set_keys(&text, keys)
    } else {
        frontmatter::rewrite(&text, keys)
    };
    stack_write(root, meta_file, new.as_bytes())
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
    let before = fsguard::read_bytes(&e.meta_file).map_err(|e| e.to_string())?;
    let keys = [
        ("status", Some("retired".to_string())),
        ("superseded_by", r.superseded_by.map(str::to_string)),
        ("retired", Some(format!("date: {}\nreason: {}", r.date, quote(r.reason)))),
    ];
    git_keeps(&e.meta_file)?;
    let root = stacks.stack(e.stack).expect("the entry's stack is loaded").root.clone();
    set_meta_keys(&root, &e.meta_file, &keys)?;
    // The stack must still load the entry, now retired; otherwise put the file back.
    let ok =
        Stack::load(&root).ok().and_then(|s| s.find(r.id).map(|x| !x.active())).unwrap_or(false);
    if !ok {
        fsguard::replace_own_file(&stack_place(&root, &e.meta_file)?, &before)?;
        return Err(format!("retiring `{}` would leave invalid metadata; nothing changed", r.id));
    }
    let commit = if r.commit && is_repo(&root) {
        git::commit(&root, &[rel(&root, &e.meta_file)], &format!("Retire {}: {}", r.id, r.reason))?
    } else {
        None
    };
    Ok(Retired { file: e.meta_file.clone(), commit })
}

/// Retires every temporary entry that has ended by `today`, reason "expired: <why>", one commit each: it stays in
/// history, never in an agent's context. Returns the ids with their commits.
pub fn retire_expired(
    stacks: &Stacks,
    today: &str,
    commit: bool,
) -> Result<Vec<(String, Option<String>)>, String> {
    let ended: Vec<(String, String)> = crate::temporary::expired(stacks, today)
        .into_iter()
        .map(|(e, why)| (e.id().to_string(), why))
        .collect();
    let mut done = Vec::new();
    for (id, why) in ended {
        // Reload: each retirement changes a file the next one's stack load reads.
        let s =
            Stacks::load(&stacks.public.root, stacks.private.as_ref().map(|p| p.root.as_path()))?;
        let reason = format!("expired: {why}");
        let r = retire(
            &s,
            &Retire {
                id: &id,
                reason: &reason,
                superseded_by: None,
                date: today.to_string(),
                commit,
            },
        )?;
        done.push((id, r.commit));
    }
    Ok(done)
}

// ---------------------------------------------------------------- import

/// The keys Lords Command adds to a memory file, which an import keeps when the agent's copy changes: every entry
/// property in the published schema except the agent's own (`name`, `description`, `metadata`). Read from the
/// schema, so a new field is never dropped by a forgotten list.
fn lords_keys() -> &'static [String] {
    static KEYS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    KEYS.get_or_init(|| {
        let schema: serde_json::Value =
            serde_json::from_str(crate::schema::ENTRY_SCHEMA).expect("the bundled schema is JSON");
        schema["properties"]
            .as_object()
            .expect("the schema lists properties")
            .keys()
            .filter(|k| !matches!(k.as_str(), "name" | "description" | "metadata"))
            .cloned()
            .collect()
    })
}

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

/// Whether the stack's copy still holds exactly what the agent wrote: the same body, and the same frontmatter once
/// the keys Lords Command added are taken out. Those keys (and where they sit) are not the agent's change.
fn same_source(incoming: &str, existing: &str) -> bool {
    let (a, b) = (frontmatter::split(incoming), frontmatter::split(existing));
    let lines =
        |y: &str| y.lines().map(|l| l.trim_end_matches('\r').to_string()).collect::<Vec<_>>();
    let agent_yaml = |yaml: &str| {
        let none: Vec<(&str, Option<String>)> =
            lords_keys().iter().map(|k| (k.as_str(), None)).collect();
        lines(&frontmatter::set_keys(yaml, &none))
            .into_iter()
            .filter(|l| !l.trim().is_empty())
            .collect::<Vec<_>>()
    };
    let theirs: Vec<String> =
        lines(a.yaml.unwrap_or("")).into_iter().filter(|l| !l.trim().is_empty()).collect();
    a.body == b.body && theirs == agent_yaml(b.yaml.unwrap_or(""))
}

/// The agent's new text, with the Lords Command keys of the stack's copy carried over.
fn carry_keys(incoming: &str, existing: &str) -> String {
    let Some(old_yaml) = frontmatter::split(existing).yaml else { return incoming.to_string() };
    let keys: Vec<(&str, Option<String>)> = lords_keys()
        .iter()
        .filter_map(|k| frontmatter::get_key(old_yaml, k).map(|v| (k.as_str(), Some(v))))
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
        // Below the private stack's root, by parts: a scope or store name that is no plain name imports nothing.
        let mut parts = layer_parts(&scope).map_err(|e| format!("store `{}`: {e}", s.name))?;
        parts.push(fsguard::Part::new("memory")?);
        parts.push(fsguard::Part::skill(&s.name).map_err(|e| format!("store `{}`: {e}", s.name))?);
        let dest_place = fsguard::Root::stack(&private.root)?.join(&parts);
        let dest = dest_place.path().to_path_buf();
        if write {
            fsguard::create_dir_all(&dest_place)?;
        }
        let defaults = dest.join(DEFAULTS);
        if write && !defaults.exists() {
            let audience =
                if s.audience.is_empty() { "all".to_string() } else { s.audience.join(", ") };
            let text = format!(
                "# Metadata every memory file in this store shares (lords import).\nkind: memory\nscope: {}\naudience: [{audience}]\nstatus: active\nvisibility: private\norigin:\n  type: imported\n  store: {}\n",
                s.scope, s.name
            );
            git_keeps(&defaults)?;
            stack_write(&private.root, &defaults, text.as_bytes())?;
        }
        let mut rep = Imported { store: s.name.clone(), ..Imported::default() };
        let mut seen = Vec::new();
        for f in stack::sorted_children(&src)? {
            // A source entry whose kind can't be told stops the import, never is skipped (r5, R5-04's class).
            if fsguard::kind_here(&f)? != fsguard::Kind::File {
                continue;
            }
            let name = stack::file_name(&f);
            if !name.ends_with(".md") {
                continue;
            }
            seen.push(name.clone());
            let incoming = fsguard::read_string(&f).map_err(|e| format!("{}: {e}", f.display()))?;
            let target = dest.join(&name);
            // Only a file that isn't there is new; one that can't be read stops the import (r5, R5-04's class).
            let existing = match fsguard::read_string(&target) {
                Ok(t) => Ok(t),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(()),
                Err(e) => return Err(format!("{}: {e}", target.display())),
            };
            match existing {
                Err(()) => {
                    if write {
                        git_keeps(&target)?;
                        stack_write(&private.root, &target, incoming.as_bytes())?;
                    }
                    rep.added.push(name);
                }
                Ok(existing) => {
                    let merged = carry_keys(&incoming, &existing);
                    if same_source(&incoming, &existing) {
                        rep.unchanged += 1;
                    } else {
                        if write {
                            git_keeps(&target)?;
                            stack_write(&private.root, &target, merged.as_bytes())?;
                        }
                        rep.changed.push(name);
                    }
                }
            }
        }
        for f in stack::sorted_children(&dest)? {
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
        Scope::Workspace(w) => {
            return Err(format!(
                "`{id}` belongs to workspace `{w}`; workspaces are private, so it stays private"
            ))
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
    // Strict: a destination whose state can't be told is never taken for none (r5, R5-04's class).
    if fsguard::kind(&dest_path)? != fsguard::Kind::Missing {
        return Err(format!("{} already exists", dest_path.display()));
    }
    let mut hits: Vec<(PathBuf, Hit)> = Vec::new();
    // Every file is read whole or the promotion stops: an unreadable or non-UTF-8 file is never promoted as an
    // empty one (Sol review r5, R5-04's class).
    let read = |f: &Path| {
        fsguard::read_string(f)
            .map_err(|err| format!("{}: {err}; promotion carries text files only", f.display()))
    };
    // Nothing git can't give back (rule C's allowlist, R5-05, R6-06): a file with any attribute but ARCHIVE or
    // NORMAL, or a skill dir with any but DIRECTORY, refuses.
    if is_skill {
        let mut dirs = vec![src_path.clone()];
        dirs.extend(
            fsguard::read_tree(&src_path)?
                .keys()
                .filter(|r| fsguard::is_dir_entry(r))
                .map(|r| src_path.join(r.trim_end_matches('/'))),
        );
        for d in dirs {
            let a = fsguard::attrs(&d)?;
            if !a.git_restores(true) {
                return Err(format!("{}: marked {}; git can't give that back once promotion deletes it. Clear it and promote again", d.display(), a.describe()));
            }
        }
    }
    for f in &e.files {
        let a = fsguard::attrs(f)?;
        if !a.git_restores(false) {
            return Err(format!("{}: marked {}; git can't give that back once promotion deletes it. Clear it and promote again", f.display(), a.describe()));
        }
    }
    for f in &e.files {
        let text = read(f)?;
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
        let text = read(f)?;
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

    // Before any write of a promotion, committing or not, git must keep every byte it changes in either stack: a
    // promotion cut short is then undone from git (Sol review r3, R3-10).
    for (f, _) in &files {
        git_keeps(f)?;
        git_keeps(&public.root.join(rel(&private.root, f)))?;
    }
    if let Some((name, _)) = &private_part {
        git_keeps(&e.meta_file.with_file_name(name))?;
    }
    // A skill's dir goes whole, so it may hold only what git gives back and the promotion carries over: files it
    // copies, and no dir without a file below it, which git doesn't record (Sol review r4, R4-07).
    if is_skill {
        let tree = fsguard::read_tree(&src_path)?;
        let carried: Vec<String> = files
            .iter()
            .map(|(f, _)| rel(&src_path, f).to_string_lossy().replace('\\', "/"))
            .collect();
        for r in tree.keys() {
            let refused = if fsguard::is_dir_entry(r) {
                !tree.keys().any(|k| !fsguard::is_dir_entry(k) && k.starts_with(r.as_str()))
            } else {
                !carried.contains(r)
            };
            if refused {
                return Err(format!(
                    "{}: {} is {}; git couldn't give it back once the promotion deletes the skill's private dir. Remove it or add a file, and promote again",
                    e.id(),
                    src_path.join(r).display(),
                    if fsguard::is_dir_entry(r) { "an empty dir" } else { "a file the promotion doesn't carry" }
                ));
            }
        }
    }
    // Write: public copy, then the private part, then remove the private original.
    for (f, text) in &files {
        let to = public.root.join(rel(&private.root, f));
        stack_write(&public.root, &to, text.as_bytes())?;
    }
    let mut part_path = None;
    if let Some((name, text)) = private_part {
        let p = e.meta_file.with_file_name(name);
        stack_write(&private.root, &p, text.as_bytes())?;
        part_path = Some(p);
    }
    let src = stack_place(&private.root, &src_path)?;
    if is_skill {
        fsguard::delete_tree(&src)?;
    } else {
        fsguard::delete_file(&src)?;
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
    for f in &files {
        git_keeps(f)?;
    }
    let before: Vec<Vec<u8>> = files
        .iter()
        .map(|f| fsguard::read_bytes(f).map_err(|e| format!("{}: {e}", f.display())))
        .collect::<Result<_, _>>()?;
    // Puts the files back as they were; what can't go back is said (git still holds it: R3-10).
    let restore = || -> String {
        let failed: Vec<String> = files
            .iter()
            .zip(&before)
            .filter_map(|(f, b)| {
                stack_place(&stack.root, f).and_then(|at| fsguard::replace_own_file(&at, b)).err()
            })
            .collect();
        if failed.is_empty() {
            String::new()
        } else {
            format!("; and putting the files back failed: {} (git holds them)", failed.join("; "))
        }
    };
    if !keys.is_empty() {
        set_meta_keys(&stack.root, &e.meta_file, &keys)?;
    }
    if let Some(body) = &change.body {
        let text = fsguard::read_string(&e.body_file).map_err(|err| err.to_string())?;
        let new = if e.body_file == e.meta_file {
            let parts = frontmatter::split(&text);
            let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
            format!("---{nl}{}---{nl}{body}", parts.yaml.unwrap_or(""))
        } else {
            body.clone()
        };
        stack_write(&stack.root, &e.body_file, new.as_bytes())?;
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
        let undo = restore();
        let changed =
            if undo.is_empty() { "nothing changed" } else { "it was undone where it could be" };
        return Err(format!("that edit would break `{id}` ({why}); {changed}{undo}"));
    }
    if commit && is_repo(&stack.root) {
        let rels: Vec<&Path> = files.iter().map(|f| rel(&stack.root, f)).collect();
        return git::commit(&stack.root, &rels, &format!("Edit {id}"));
    }
    Ok(None)
}
