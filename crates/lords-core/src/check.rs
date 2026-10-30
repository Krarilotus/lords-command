//! `check` (design: Check): every test over both stacks. Candidates are for the owner, never resolved here.

use crate::finding::{Finding, Severity};
use crate::fsguard;
use crate::model::{Entry, Origin, Scope, Visibility};
use crate::overlap;
use crate::privacy::{self, Rules, Scan};
use crate::stack::{Stacks, SIDECAR};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

/// Whether two scopes can ever apply in the same composition.
pub fn coapply(stacks: &Stacks, a: &Scope, b: &Scope) -> bool {
    let project_of = |r: &str| stacks.repo_project(r);
    match (a, b) {
        (Scope::Global, _) | (_, Scope::Global) => true,
        (Scope::Project(p), Scope::Project(q)) => p == q,
        (Scope::Project(p), Scope::Workspace(w)) | (Scope::Workspace(w), Scope::Project(p)) => {
            stacks.workspace_project(w).as_deref() == Some(p)
        }
        (Scope::Workspace(a), Scope::Workspace(b)) => a == b,
        (Scope::Workspace(_), _) | (_, Scope::Workspace(_)) => false,
        (Scope::Project(p), s) | (s, Scope::Project(p)) => {
            s.repo().and_then(project_of).as_deref() == Some(p)
        }
        (Scope::Repo(r), s) | (s, Scope::Repo(r)) => s.repo() == Some(r),
        (Scope::Path { repo: r1, sub: s1 }, Scope::Path { repo: r2, sub: s2 }) => {
            r1 == r2 && (s1.starts_with(s2.as_str()) || s2.starts_with(s1.as_str()))
        }
    }
}

/// Whether one entry names the other (override, extension, succession).
pub fn related(a: &Entry, b: &Entry) -> bool {
    let names = |x: &Entry, y: &Entry| {
        x.meta
            .overrides
            .iter()
            .chain(&x.meta.extends)
            .chain(&x.meta.superseded_by)
            .any(|id| id == y.id())
    };
    names(a, b) || names(b, a)
}

/// The hash an inherited entry records: a skill's files (all but `lords.yaml`, paths with `/`, sorted), or a
/// plain entry's body. Line endings count as LF, so a checkout's autocrlf doesn't change it.
pub fn source_hash(entry: &Entry) -> String {
    match entry.meta.kind {
        crate::model::Kind::Skill => {
            let dir = entry.meta_file.parent().expect("a sidecar has a dir");
            dir_hash_or_unreadable(dir)
        }
        _ => hex(Sha256::digest(normalise_eol(entry.body.as_bytes()))),
    }
}

/// A skill dir's hash, or for one that can't be read a text that is no SHA-256: it never equals a recorded hash,
/// so the entry shows as changed, with the reason (Sol review r5, R5-04's class: never a hash of what could be read).
pub fn dir_hash_or_unreadable(dir: &Path) -> String {
    dir_hash(dir).unwrap_or_else(|e| format!("unreadable: {e}"))
}

pub fn dir_hash(dir: &Path) -> Result<String, String> {
    let mut h = Sha256::new();
    for f in crate::stack::all_files(dir)? {
        if f.file_name().is_some_and(|n| n == SIDECAR) {
            continue;
        }
        let rel = f.strip_prefix(dir).unwrap_or(&f).to_string_lossy().replace('\\', "/");
        let bytes = fsguard::read_bytes(&f).map_err(|e| format!("{}: {e}", f.display()))?;
        h.update(rel.as_bytes());
        h.update([0]);
        h.update(normalise_eol(&bytes));
        h.update([0]);
    }
    Ok(hex(h.finalize()))
}

/// The hash for `lords hash`: a dir as a skill, a file with frontmatter by its body, any other file whole.
pub fn path_hash(path: &Path) -> Result<String, String> {
    if path.is_dir() {
        return dir_hash(path);
    }
    let text = fsguard::read_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let body = crate::frontmatter::split(&text).body;
    Ok(hex(Sha256::digest(normalise_eol(body.as_bytes()))))
}

fn normalise_eol(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
            i += 1;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

fn hex(bytes: impl AsRef<[u8]>) -> String {
    bytes.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

pub fn is_id(s: &str) -> bool {
    crate::model::Kind::ALL
        .iter()
        .any(|k| s.strip_prefix(k.name()).is_some_and(|r| r.starts_with('.')))
        && !s.contains('/')
        && !s.ends_with(".md")
}

/// Runs every check over the stacks. `public_rules` is what the public stack is scanned with; the private
/// stack is always scanned for secrets only.
pub fn check(stacks: &Stacks, public_rules: &Rules) -> Vec<Finding> {
    check_at(stacks, public_rules, &crate::sync::default_home().unwrap_or_default())
}

/// `check` with the tool's home given (where the hook probe's nonce goes; tests use their own).
pub fn check_at(stacks: &Stacks, public_rules: &Rules, home: &Path) -> Vec<Finding> {
    findings(stacks, public_rules, Some(home))
}

/// `check` without the hook probe: what a stack repo's own pre-commit runs, where probing the hooks would run them
/// again (orchestrator, 27 Oct).
pub fn check_stack(stacks: &Stacks, public_rules: &Rules) -> Vec<Finding> {
    findings(stacks, public_rules, None)
}

fn findings(stacks: &Stacks, public_rules: &Rules, probe_home: Option<&Path>) -> Vec<Finding> {
    let mut out: Vec<Finding> = stacks.all().flat_map(|s| s.problems.iter().cloned()).collect();
    ids(stacks, &mut out);
    declared(stacks, &mut out);
    relations(stacks, &mut out);
    links(stacks, &mut out);
    provenance(stacks, &mut out);
    overlap::check(stacks, &mut out);
    skill_clashes(stacks, &mut out);
    temporaries(stacks, &crate::ops::today(), &mut out);
    privacy_of_entries(stacks, public_rules, &mut out);
    if let Some(home) = probe_home {
        privacy_hooks(stacks, home, &mut out);
    }
    for n in &public_rules.notes {
        out.push(Finding::info("privacy-scope", n.clone()));
    }
    out.sort_by(|a, b| {
        a.severity.cmp(&b.severity).then_with(|| a.file.cmp(&b.file)).then(a.line.cmp(&b.line))
    });
    out
}

/// Guarantee 7 holds as long as commits run through the effective privacy hooks (orchestrator, 25 Oct, decision 8):
/// the public stack's repo is checked for that, and each hook a commit wouldn't reach is "not verified".
fn privacy_hooks(stacks: &Stacks, home: &Path, out: &mut Vec<Finding>) {
    let root = &stacks.public.root;
    // No repo only where git says so; any other git error verifies nothing (Sol review r6, R6-08).
    let top = match crate::git::run(root, &["rev-parse", "--show-toplevel"]) {
        Ok(t) => std::path::PathBuf::from(t.trim()),
        Err(e) if e.contains("not a git repository") => {
            out.push(Finding::info(
                "privacy-hooks",
                format!("{} is in no git repo: no commit to check", root.display()),
            ));
            return;
        }
        Err(e) => {
            out.push(Finding::warning(
                "privacy-hooks",
                format!(
                    "{}: {}: not verified ({e})",
                    root.display(),
                    crate::hooks::PRIVACY.join(", ")
                ),
            ));
            return;
        }
    };
    if home.as_os_str().is_empty() {
        out.push(Finding::warning(
            "privacy-hooks",
            format!(
                "{}: {}: not verified (no tool home for the probe)",
                top.display(),
                crate::hooks::PRIVACY.join(", ")
            ),
        ));
        return;
    }
    for p in crate::hooks::privacy_dispatch(&top, home) {
        out.push(Finding::warning("privacy-hooks", format!("{}: {p}", top.display())));
    }
}

fn ids(stacks: &Stacks, out: &mut Vec<Finding>) {
    let mut by_id: BTreeMap<&str, Vec<&Entry>> = BTreeMap::new();
    for e in stacks.entries() {
        by_id.entry(e.id()).or_default().push(e);
    }
    for (id, es) in by_id.into_iter().filter(|(_, es)| es.len() > 1) {
        for e in &es {
            out.push(
                Finding::error("id-duplicate", format!("`{id}` is defined {} times", es.len()))
                    .at(&e.meta_file, None)
                    .entry(id)
                    .related(
                        es.iter()
                            .filter(|o| o.meta_file != e.meta_file)
                            .map(|o| o.meta_file.display().to_string()),
                    ),
            );
        }
    }
}

fn declared(stacks: &Stacks, out: &mut Vec<Finding>) {
    // A place's default role is a declared development role (product runs never read a place's files).
    let roles = crate::sync::declared_roles(stacks);
    for s in stacks.all() {
        let m = &s.manifest;
        let repos = m.repos.iter().map(|(n, r)| (format!("repo {n}"), &r.default_role));
        let workspaces =
            m.workspaces.iter().map(|(n, w)| (format!("workspace {n}"), &w.default_role));
        for (place, role) in repos.chain(workspaces) {
            let Some(role) = role else { continue };
            if !roles.contains(role) || role == crate::compose::PRODUCT_ROLE {
                out.push(Finding::error(
                    "role-undeclared",
                    format!("{place}: default_role `{role}` is no declared development role"),
                ));
            }
        }
        // A target's roles likewise (Codex switch-on review r1, R3: a typo selected nothing).
        for t in &m.targets {
            for r in t.roles.iter().filter(|r| !roles.contains(r)) {
                out.push(Finding::error(
                    "role-undeclared",
                    format!("target {}: role `{r}` is in no stack's roles list", t.name),
                ));
            }
        }
    }
    for e in stacks.entries() {
        // Topics are a controlled list: the entry's own stack declares them (a public entry, the public stack).
        let known = |t: &String| {
            stacks.public.manifest.topics.contains(t)
                || (e.stack == Visibility::Private
                    && stacks.private.as_ref().is_some_and(|p| p.manifest.topics.contains(t)))
        };
        // Roles likewise.
        let role_known = |r: &String| {
            stacks.public.manifest.roles.contains(r)
                || (e.stack == Visibility::Private
                    && stacks.private.as_ref().is_some_and(|p| p.manifest.roles.contains(r)))
        };
        for r in e.meta.roles.iter().filter(|r| !role_known(r)) {
            out.push(
                Finding::error(
                    "role-undeclared",
                    format!("role `{r}` is not in the {} stack's roles list", e.stack),
                )
                .at(&e.meta_file, None)
                .entry(e.id()),
            );
        }
        for t in e.meta.topics.iter().filter(|t| !known(t)) {
            out.push(
                Finding::error(
                    "topic-undeclared",
                    format!("topic `{t}` is not in the {} stack's topics list", e.stack),
                )
                .at(&e.meta_file, None)
                .entry(e.id()),
            );
        }
        let missing = match &e.scope {
            Scope::Project(p) if !stacks.declares_project(p) => Some(format!("project `{p}`")),
            Scope::Workspace(w) if !stacks.declares_workspace(w) => {
                Some(format!("workspace `{w}`"))
            }
            Scope::Repo(r) | Scope::Path { repo: r, .. } if !stacks.declares_repo(r) => {
                Some(format!("repo `{r}`"))
            }
            _ => None,
        };
        if let Some(what) = missing {
            out.push(
                Finding::warning("undeclared", format!("{what} is in no stack.toml"))
                    .at(&e.meta_file, None)
                    .entry(e.id()),
            );
        }
    }
}

fn relations(stacks: &Stacks, out: &mut Vec<Finding>) {
    for e in stacks.entries() {
        let at = |f: Finding| f.at(&e.meta_file, None).entry(e.id());
        let named = e
            .meta
            .overrides
            .iter()
            .map(|t| ("overrides", t))
            .chain(e.meta.extends.iter().map(|t| ("extends", t)));
        let named = named.chain(e.meta.superseded_by.iter().map(|t| ("superseded_by", t)));
        for (field, target) in named {
            let Some(t) = stacks.find(target) else {
                out.push(at(Finding::error(
                    "dangling",
                    format!("{field} `{target}`: no such entry"),
                )));
                continue;
            };
            if e.stack == Visibility::Public && t.stack == Visibility::Private {
                out.push(at(Finding::error(
                    "public-to-private",
                    format!("{field} `{target}`, a private entry"),
                )));
            }
            if field == "superseded_by" {
                if !t.active() {
                    out.push(at(Finding::warning(
                        "superseded-by-retired",
                        format!("successor `{target}` is retired"),
                    )));
                }
                continue;
            }
            if t.rank() > e.rank() {
                out.push(at(Finding::error(
                    "override-narrower",
                    format!(
                        "{field} `{target}` ({} {}), a narrower layer than its own ({} {})",
                        t.stack, t.scope, e.stack, e.scope
                    ),
                )));
            } else if !coapply(stacks, &e.scope, &t.scope) {
                out.push(at(Finding::error(
                    "override-unrelated",
                    format!("{field} `{target}` ({}), which never applies with it", t.scope),
                )));
            }
            if e.active() && !t.active() {
                out.push(at(Finding::warning(
                    "links-retired",
                    format!("{field} `{target}`, which is retired"),
                )));
            }
            if field == "overrides" && e.active() {
                out.push(at(Finding::info(
                    "override",
                    format!("replaces `{target}` ({} {})", t.stack, t.scope),
                )));
            }
        }
    }
}

fn links(stacks: &Stacks, out: &mut Vec<Finding>) {
    let md_link = regex::Regex::new(r"\]\(([^)\s]+)\)").expect("pattern compiles");
    for e in stacks.entries() {
        let dir = e.body_file.parent().unwrap_or(Path::new("."));
        for l in e.meta.links.iter().chain(e.meta.evidence.iter().flat_map(|x| x.links.iter())) {
            if is_id(l) {
                match stacks.find(l) {
                    None => out.push(
                        Finding::error("dangling", format!("link `{l}`: no such entry"))
                            .at(&e.meta_file, None)
                            .entry(e.id()),
                    ),
                    Some(t) if e.stack == Visibility::Public && t.stack == Visibility::Private => {
                        out.push(
                            Finding::error(
                                "public-to-private",
                                format!("links `{l}`, a private entry"),
                            )
                            .at(&e.meta_file, None)
                            .entry(e.id()),
                        )
                    }
                    Some(t) if e.active() && !t.active() => out.push(
                        Finding::warning("links-retired", format!("links `{l}`, which is retired"))
                            .at(&e.meta_file, None)
                            .entry(e.id()),
                    ),
                    Some(_) => {}
                }
            } else if !l.contains("://") && !dir.join(l).exists() {
                out.push(
                    Finding::error("dangling", format!("link `{l}`: no such file"))
                        .at(&e.meta_file, None)
                        .entry(e.id()),
                );
            }
        }
        for (i, line) in e.body.lines().enumerate() {
            for cap in md_link.captures_iter(line) {
                let target = &cap[1];
                if target.contains("://")
                    || target.starts_with('#')
                    || target.starts_with("mailto:")
                {
                    continue;
                }
                let path = target.split('#').next().unwrap_or(target);
                if !path.is_empty() && !dir.join(path).exists() {
                    out.push(
                        Finding::error("dangling", format!("link `{target}`: no such file"))
                            .at(&e.body_file, Some(e.body_line + i))
                            .entry(e.id()),
                    );
                }
            }
        }
    }
}

fn provenance(stacks: &Stacks, out: &mut Vec<Finding>) {
    for e in stacks.entries() {
        let Some(src) = e.meta.origin.source() else { continue };
        let dir = e.meta_file.parent().unwrap_or(Path::new("."));
        let looks_like_file =
            src.licence.contains('/') || src.licence.contains('.') || src.licence.contains('\\');
        if looks_like_file && !dir.join(&src.licence).is_file() {
            out.push(
                Finding::error("provenance", format!("licence file `{}` is missing", src.licence))
                    .at(&e.meta_file, None)
                    .entry(e.id()),
            );
        }
        if let Origin::Inherited(src) = &e.meta.origin {
            let recorded = src.sha256.as_deref().unwrap_or("");
            let now = source_hash(e);
            if recorded != now {
                out.push(
                    Finding::error("inherited-changed", format!("an inherited entry must stay verbatim: sha256 is {now}, recorded {recorded}"))
                        .at(&e.meta_file, None)
                        .entry(e.id()),
                );
            }
        }
    }
}

/// Temporary entries: each `when` watches a real status entry, and one that has ended is reported (sync retires
/// it). The warning fails under `--strict` like any finding the owner must act on.
pub fn temporaries(stacks: &Stacks, today: &str, out: &mut Vec<Finding>) {
    for e in stacks.entries().filter(|e| e.active()) {
        let Some(t) = &e.meta.temporary else { continue };
        if let Some(w) = &t.when {
            match stacks.find(&w.status) {
                Some(s) if s.meta.kind == crate::model::Kind::Status => {}
                _ => out.push(
                    Finding::error(
                        "dangling",
                        format!("temporary.when watches `{}`, which is no status entry", w.status),
                    )
                    .at(&e.meta_file, None)
                    .entry(e.id()),
                ),
            }
        }
        if let Some(why) = crate::temporary::ended(e, stacks, today) {
            out.push(
                Finding::warning(
                    "temporary-expired",
                    format!("ended ({why}); it is no longer composed, and sync --apply retires it"),
                )
                .at(&e.meta_file, None)
                .entry(e.id()),
            );
        }
    }
}

/// A development skill whose name a repo also tracks in a skills dir it indexes in place. The repo's tracked copy wins
/// there and sync doesn't install the stack's over it (orchestrator, 27 Oct); a copy with the same files is no finding,
/// one that differs is a warning, so the two get looked at.
fn skill_clashes(stacks: &Stacks, out: &mut Vec<Finding>) {
    for t in stacks.entries().filter(|e| matches!(e.meta.origin, Origin::InRepo { .. })) {
        let Origin::InRepo { sha256: tracked_sha, .. } = &t.meta.origin else { continue };
        let tracked = t.id().rsplit('.').next().unwrap_or_default();
        for s in stacks.entries().filter(|e| {
            e.active()
                && e.meta.kind == crate::model::Kind::Skill
                && !matches!(e.meta.origin, Origin::InRepo { .. })
                && !crate::compose::user_level(e)
                && coapply(stacks, &e.scope, &t.scope)
        }) {
            if s.id().trim_start_matches("skill.") != tracked {
                continue;
            }
            let stack_sha = s.meta_file.parent().map(dir_hash_or_unreadable).unwrap_or_default();
            if stack_sha == *tracked_sha {
                continue;
            }
            out.push(
                Finding::warning(
                    "skill-clash",
                    format!(
                        "the repo tracks its own `{tracked}` ({}), which differs from the stack's; the repo's copy wins there",
                        t.id()
                    ),
                )
                .at(&s.meta_file, None)
                .entry(s.id()),
            );
        }
    }
}

fn privacy_of_entries(stacks: &Stacks, public_rules: &Rules, out: &mut Vec<Finding>) {
    let secrets = Rules::secrets_only();
    for stack in stacks.all() {
        let rules = if stack.visibility() == Visibility::Public { public_rules } else { &secrets };
        for e in &stack.entries {
            for f in &e.files {
                // Every file is scanned, whatever it is: one that can't be read is an error, never skipped, and
                // one that isn't UTF-8 is scanned in all its readings, as the staged check does (r5, R5-04's class).
                let bytes = match fsguard::read_bytes(f) {
                    Ok(b) => b,
                    Err(err) => {
                        out.push(
                            Finding::error("unreadable", format!("{}: {err}", f.display()))
                                .at(f, None)
                                .entry(e.id()),
                        );
                        continue;
                    }
                };
                let mut scan = privacy::Scan::default();
                scan.add(f.clone(), &bytes, rules);
                for (_, h) in &scan.hits {
                    out.push(privacy_finding(f, h).entry(e.id()));
                }
                let text = privacy::decode(&bytes).0;
                if stack.visibility() == Visibility::Public && text.contains("<private:") {
                    out.push(
                        Finding::warning("placeholder", "a stripped detail still needs rewording")
                            .at(f, None)
                            .entry(e.id()),
                    );
                }
            }
        }
    }
}

/// A hit on line 0 is in the file's name.
pub fn privacy_finding(file: &Path, h: &privacy::Hit) -> Finding {
    let place = if h.line == 0 { " in the file name" } else { "" };
    Finding::error("privacy", format!("{}{place}: `{}`", h.class.name(), h.text))
        .at(file, Some(h.line).filter(|&l| l > 0))
}

/// `check --public`: the whole public repo tree, every file git would commit, scanned with all classes.
pub fn public_tree(repo: &Path, rules: &Rules) -> Result<(Scan, Vec<Finding>), String> {
    let scan = privacy::scan_tree(repo, rules)?;
    let findings = scan.hits.iter().map(|(p, h)| privacy_finding(&repo.join(p), h)).collect();
    Ok((scan, findings))
}

pub fn has_errors(findings: &[Finding], strict: bool) -> bool {
    findings.iter().any(|f| {
        f.severity == Severity::Error
            || (strict && (f.severity == Severity::Candidate || f.code == "temporary-expired"))
    })
}
