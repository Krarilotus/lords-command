//! The privacy check (design: Privacy). Finds spans that must not reach a public repo.

use crate::config::expand_home;
use crate::git;
use crate::stack::Stack;
use regex::Regex;
use serde::Serialize;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Class {
    Secret,
    Path,
    Contact,
    Fleet,
    Deny,
}

impl Class {
    pub fn name(self) -> &'static str {
        match self {
            Class::Secret => "secret",
            Class::Path => "path",
            Class::Contact => "contact",
            Class::Fleet => "fleet",
            Class::Deny => "deny",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Hit {
    pub class: Class,
    /// 1-based line.
    pub line: usize,
    /// Byte range within the line.
    pub start: usize,
    pub end: usize,
    pub text: String,
}

/// What to look for. `public()` with the private stack's lists is the full check; `secrets_only()` is what
/// runs on the private stack.
#[derive(Clone, Debug, Default)]
pub struct Rules {
    pub classes: BTreeSet<Class>,
    /// Lower-cased terms from the private deny-list.
    pub deny: Vec<String>,
    /// Lower-cased host names from the fleet config.
    pub hosts: Vec<String>,
    /// Exact matched texts accepted as false positives.
    pub allow: BTreeSet<String>,
    /// What could not be loaded (e.g. no private stack): reported, never silent.
    pub notes: Vec<String>,
}

impl Rules {
    pub fn public() -> Rules {
        Rules {
            classes: [Class::Secret, Class::Path, Class::Contact, Class::Fleet, Class::Deny].into(),
            ..Rules::default()
        }
    }

    pub fn secrets_only() -> Rules {
        Rules { classes: [Class::Secret].into(), ..Rules::default() }
    }

    /// Adds the allow-list of a stack (`privacy/allow.txt`), if it has one.
    pub fn with_allow_from(mut self, stack_root: &Path) -> Rules {
        if let Ok(text) = fs::read_to_string(stack_root.join("privacy").join("allow.txt")) {
            self.allow.extend(list_lines(&text).map(str::to_string));
        }
        self
    }

    /// Adds the private stack's deny-list and fleet hosts, or notes that they are missing.
    pub fn with_private(mut self, private: Option<&Stack>) -> Rules {
        let Some(private) = private else {
            self.notes.push("no private stack: the deny and fleet classes were not checked".into());
            return self;
        };
        self = self.with_allow_from(&private.root);
        match fs::read_to_string(private.root.join("privacy").join("deny.txt")) {
            Ok(text) => self.deny.extend(list_lines(&text).map(|t| t.to_ascii_lowercase())),
            Err(_) => self.notes.push("the private stack has no privacy/deny.txt".into()),
        }
        if let Some(cfg) = &private.manifest.privacy.fleet_config {
            match fleet_hosts(&expand_home(cfg)) {
                Ok(hosts) => self.hosts.extend(hosts),
                Err(e) => self.notes.push(format!("fleet config not read: {e}")),
            }
        }
        self
    }
}

fn list_lines(text: &str) -> impl Iterator<Item = &str> {
    text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#'))
}

/// Host names from a fleet config: the `[host.*]` and `[machine.*]` keys and each link's `target`.
pub fn fleet_hosts(path: &Path) -> Result<Vec<String>, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let value: toml::Table =
        toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut out = BTreeSet::new();
    for table in ["host", "machine"] {
        if let Some(toml::Value::Table(hosts)) = value.get(table) {
            for (name, h) in hosts {
                out.insert(name.to_ascii_lowercase());
                if let Some(target) =
                    h.get("link").and_then(|l| l.get("target")).and_then(|t| t.as_str())
                {
                    out.insert(target.to_ascii_lowercase());
                }
            }
        }
    }
    Ok(out.into_iter().collect())
}

struct Pattern {
    class: Class,
    re: Regex,
    /// Which capture group is the private part (0 = whole match).
    group: usize,
}

fn patterns() -> &'static [Pattern] {
    static P: OnceLock<Vec<Pattern>> = OnceLock::new();
    P.get_or_init(|| {
        let p = |class, re: &str, group| Pattern { class, re: Regex::new(re).expect("pattern compiles"), group };
        vec![
            // A user name in a Windows or WSL home path. Placeholders such as <name> or %USERNAME% don't match.
            p(Class::Path, r#"(?i)\b[a-z]:(?:\\{1,2}|/)users(?:\\{1,2}|/)([^\\/\s"'`<>%*|?:{}$]+)"#, 1),
            p(Class::Path, r#"(?i)(?:/mnt/[a-z]|^/[a-z]|[\s"'`(=]/[a-z])/users/([^/\s"'`<>%{}$]+)"#, 1),
            p(Class::Path, r#"(?:^|[\s"'`(=:])/home/([^/\s"'`<>%{}$]+)"#, 1),
            p(Class::Contact, r"(?i)\b[a-z0-9._%+-]+@[a-z0-9-]+(?:\.[a-z0-9-]+)*\.[a-z]{2,}\b", 0),
            p(Class::Secret, r"\b(?:gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{30,}|glpat-[A-Za-z0-9_-]{20,})", 0),
            p(Class::Secret, r"\b(?:sk-[A-Za-z0-9_-]{20,}|xox[abprs]-[A-Za-z0-9-]{10,}|AKIA[0-9A-Z]{16})\b", 0),
            p(Class::Secret, r"-----BEGIN [A-Z ]*PRIVATE KEY-----", 0),
            p(
                Class::Secret,
                r#"(?i)https?://[^\s"'<>]*[?&](?:token|access_token|key|api_key|apikey|sig|signature|secret|code)=[^&\s"'<>]{8,}"#,
                0,
            ),
            p(Class::Secret, r"(?i)https?://[^/\s:@]+:[^/\s@]+@", 0),
            p(Class::Secret, r"(?i)hooks\.slack\.com/services/[A-Za-z0-9/]+", 0),
            p(Class::Secret, r"(?i)discord(?:app)?\.com/api/webhooks/\d+/[A-Za-z0-9_-]+", 0),
            p(Class::Secret, r#"(?i)https?://[^\s"'<>]*/webhooks?/[A-Za-z0-9_-]{16,}"#, 0),
            p(Class::Secret, r#"(?i)\b(?:password|passwd|secret|token|api[_-]?key)\s*[:=]\s*["']?([A-Za-z0-9/+_\-]{16,})"#, 1),
        ]
    })
}

/// A long assigned value counts as a secret only if it mixes letters and digits (not a word or a path part).
fn looks_random(s: &str) -> bool {
    s.chars().any(|c| c.is_ascii_digit()) && s.chars().any(|c| c.is_ascii_alphabetic())
}

pub fn scan(text: &str, rules: &Rules) -> Vec<Hit> {
    let mut hits = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let n = i + 1;
        for p in patterns().iter().filter(|p| rules.classes.contains(&p.class)) {
            for caps in p.re.captures_iter(line) {
                let Some(m) = caps.get(p.group) else { continue };
                if p.group == 1 && p.class == Class::Secret && !looks_random(m.as_str()) {
                    continue;
                }
                // A backslash escape before `@` (an `\n@AGENTS.md` include line in source) is not an address.
                if p.class == Class::Contact && line[..m.start()].ends_with(char::from(92)) {
                    continue;
                }
                hits.push(Hit {
                    class: p.class,
                    line: n,
                    start: m.start(),
                    end: m.end(),
                    text: m.as_str().to_string(),
                });
            }
        }
        let lower = line.to_ascii_lowercase();
        for (class, terms) in [(Class::Fleet, &rules.hosts), (Class::Deny, &rules.deny)] {
            if rules.classes.contains(&class) {
                for term in terms.iter().filter(|t| !t.is_empty()) {
                    for (start, _) in lower.match_indices(term.as_str()) {
                        let end = start + term.len();
                        let before = lower[..start].chars().next_back();
                        let after = lower[end..].chars().next();
                        let word =
                            |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
                        if !word(before) && !word(after) {
                            hits.push(Hit {
                                class,
                                line: n,
                                start,
                                end,
                                text: line[start..end].to_string(),
                            });
                        }
                    }
                }
            }
        }
    }
    hits.retain(|h| !rules.allow.contains(&h.text));
    hits.sort_by_key(|h| (h.line, h.start));
    hits.dedup_by(|a, b| a.line == b.line && a.start < b.end && b.start < a.end);
    hits
}

/// Replaces each hit's span with `<private:class>`, the placeholder the owner then rewords.
pub fn strip(text: &str, hits: &[Hit]) -> String {
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.split_inclusive('\n').enumerate() {
        let mut spans: Vec<&Hit> = hits.iter().filter(|h| h.line == i + 1).collect();
        spans.sort_by_key(|h| h.start);
        let mut pos = 0;
        for h in spans {
            if h.start < pos {
                continue;
            }
            out.push_str(&line[pos..h.start]);
            out.push_str(&format!("<private:{}>", h.class.name()));
            pos = h.end;
        }
        out.push_str(&line[pos..]);
    }
    out
}

/// The result of scanning a set of files.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Scan {
    pub files_scanned: usize,
    /// Files not scanned because they are not text; listed so nothing is skipped silently.
    pub binary: Vec<PathBuf>,
    pub hits: Vec<(PathBuf, Hit)>,
}

fn is_text(bytes: &[u8]) -> Option<&str> {
    if bytes.iter().take(8000).any(|&b| b == 0) {
        return None;
    }
    std::str::from_utf8(bytes).ok()
}

impl Scan {
    fn add(&mut self, path: PathBuf, bytes: &[u8], rules: &Rules) {
        match is_text(bytes) {
            Some(text) => {
                self.files_scanned += 1;
                self.hits.extend(scan(text, rules).into_iter().map(|h| (path.clone(), h)));
            }
            None => self.binary.push(path),
        }
    }
}

/// Every file git would commit in this repo: tracked plus untracked-not-ignored, as in the worktree.
pub fn tree_files(repo: &Path) -> Result<Vec<PathBuf>, String> {
    let out = git::run(repo, &["ls-files", "--cached", "--others", "--exclude-standard", "-z"])?;
    let mut files: Vec<PathBuf> = out
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .filter(|p| repo.join(p).is_file())
        .collect();
    files.sort();
    files.dedup();
    Ok(files)
}

pub fn scan_tree(repo: &Path, rules: &Rules) -> Result<Scan, String> {
    let mut scan = Scan::default();
    for rel in tree_files(repo)? {
        let bytes = fs::read(repo.join(&rel)).map_err(|e| format!("{}: {e}", rel.display()))?;
        scan.add(rel, &bytes, rules);
    }
    Ok(scan)
}

/// The staged version of every added, copied, modified or renamed file: what the commit would contain.
pub fn scan_staged(repo: &Path, rules: &Rules) -> Result<Scan, String> {
    let names = git::run(repo, &["diff", "--cached", "--name-only", "--diff-filter=ACMR", "-z"])?;
    let mut scan = Scan::default();
    for rel in names.split('\0').filter(|s| !s.is_empty()) {
        let bytes = git::bytes(repo, &["show", &format!(":{rel}")])?;
        scan.add(PathBuf::from(rel), &bytes, rules);
    }
    Ok(scan)
}

/// One finding in a repo's history: the commit, where in it (`message`, or the file the patch line belongs to),
/// and the span.
#[derive(Clone, Debug, Serialize)]
pub struct HistoryHit {
    pub commit: String,
    pub place: String,
    pub hit: Hit,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct HistoryScan {
    pub commits: usize,
    pub hits: Vec<HistoryHit>,
}

/// Every commit reachable from any ref, as `git log -p` shows it: its message and its whole patch (file names
/// included). What a later commit removed is still in the history, so a clean tree proves nothing about it.
pub fn scan_history(repo: &Path, rules: &Rules) -> Result<HistoryScan, String> {
    let out = git::run(
        repo,
        &["log", "--all", "-p", "--no-color", "--no-ext-diff", "--format=%x1e%H%n%B%x1f"],
    )?;
    let mut scan = HistoryScan::default();
    for record in out.split('\u{1e}').filter(|r| !r.trim().is_empty()) {
        let (head, patch) = record.split_once('\u{1f}').unwrap_or((record, ""));
        let (commit, message) = head.split_once('\n').unwrap_or((head, ""));
        let commit: String = commit.trim().chars().take(12).collect();
        scan.commits += 1;
        for h in self::scan(message, rules) {
            scan.hits.push(HistoryHit { commit: commit.clone(), place: "message".into(), hit: h });
        }
        let lines: Vec<&str> = patch.lines().collect();
        for h in self::scan(patch, rules) {
            let file = lines[..h.line.min(lines.len())]
                .iter()
                .rev()
                .find_map(|l| l.strip_prefix("diff --git a/").and_then(|r| r.split(" b/").next()))
                .unwrap_or("patch")
                .to_string();
            scan.hits.push(HistoryHit { commit: commit.clone(), place: file, hit: h });
        }
    }
    Ok(scan)
}
