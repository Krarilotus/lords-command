//! `check` (design: Check): every test over both stacks. Candidates are for the owner, never resolved here.

use crate::finding::{Finding, Severity};
use crate::model::{Entry, Origin, Scope, Visibility};
use crate::overlap;
use crate::privacy::{self, Rules, Scan};
use crate::stack::{Stacks, SIDECAR};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// Whether two scopes can ever apply in the same composition.
pub fn coapply(stacks: &Stacks, a: &Scope, b: &Scope) -> bool {
    let project_of = |r: &str| stacks.repo_project(r);
    match (a, b) {
        (Scope::Global, _) | (_, Scope::Global) => true,
        (Scope::Project(p), Scope::Project(q)) => p == q,
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
            dir_hash(dir)
        }
        _ => hex(Sha256::digest(normalise_eol(entry.body.as_bytes()))),
    }
}

pub fn dir_hash(dir: &Path) -> String {
    let mut h = Sha256::new();
    for f in crate::stack::all_files(dir) {
        if f.file_name().is_some_and(|n| n == SIDECAR) {
            continue;
        }
        let rel = f.strip_prefix(dir).unwrap_or(&f).to_string_lossy().replace('\\', "/");
        let bytes = fs::read(&f).unwrap_or_default();
        h.update(rel.as_bytes());
        h.update([0]);
        h.update(normalise_eol(&bytes));
        h.update([0]);
    }
    hex(h.finalize())
}

/// The hash for `lords hash`: a dir as a skill, a file with frontmatter by its body, any other file whole.
pub fn path_hash(path: &Path) -> Result<String, String> {
    if path.is_dir() {
        return Ok(dir_hash(path));
    }
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
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
    let mut out: Vec<Finding> = stacks.all().flat_map(|s| s.problems.iter().cloned()).collect();
    ids(stacks, &mut out);
    declared(stacks, &mut out);
    relations(stacks, &mut out);
    links(stacks, &mut out);
    provenance(stacks, &mut out);
    overlap::check(stacks, &mut out);
    privacy_of_entries(stacks, public_rules, &mut out);
    for n in &public_rules.notes {
        out.push(Finding::info("privacy-scope", n.clone()));
    }
    out.sort_by(|a, b| {
        a.severity.cmp(&b.severity).then_with(|| a.file.cmp(&b.file)).then(a.line.cmp(&b.line))
    });
    out
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
    for e in stacks.entries() {
        let missing = match &e.scope {
            Scope::Project(p) if !stacks.declares_project(p) => Some(format!("project `{p}`")),
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
        for l in &e.meta.links {
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
            } else if !dir.join(l).exists() {
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

fn privacy_of_entries(stacks: &Stacks, public_rules: &Rules, out: &mut Vec<Finding>) {
    let secrets = Rules::secrets_only();
    for stack in stacks.all() {
        let rules = if stack.visibility() == Visibility::Public { public_rules } else { &secrets };
        for e in &stack.entries {
            for f in &e.files {
                let Ok(text) = fs::read_to_string(f) else { continue };
                for h in privacy::scan(&text, rules) {
                    out.push(privacy_finding(f, &h).entry(e.id()));
                }
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

pub fn privacy_finding(file: &Path, h: &privacy::Hit) -> Finding {
    Finding::error("privacy", format!("{}: `{}`", h.class.name(), h.text)).at(file, Some(h.line))
}

/// `check --public`: the whole public repo tree, every file git would commit, scanned with all classes.
pub fn public_tree(repo: &Path, rules: &Rules) -> Result<(Scan, Vec<Finding>), String> {
    let scan = privacy::scan_tree(repo, rules)?;
    let findings = scan.hits.iter().map(|(p, h)| privacy_finding(&repo.join(p), h)).collect();
    Ok((scan, findings))
}

pub fn has_errors(findings: &[Finding], strict: bool) -> bool {
    findings
        .iter()
        .any(|f| f.severity == Severity::Error || (strict && f.severity == Severity::Candidate))
}
