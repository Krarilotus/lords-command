//! The privacy check (design: Privacy). Finds spans that must not reach a public repo.

use crate::config::expand_home;
use crate::fsguard;
use crate::git;
use crate::model::Visibility;
use crate::stack::Stack;
use regex::Regex;
use serde::Serialize;
use std::collections::BTreeSet;
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
    /// The matched text. A deny phrase wrapped across lines runs on past `end`, into the next lines.
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
        // Lossy on purpose: an allow-list that can't be read allows nothing, so the check only finds more.
        if let Some(text) = fsguard::read_string_lossy(stack_root.join("privacy").join("allow.txt"))
        {
            self.allow.extend(list_lines(&text).map(str::to_string));
        }
        self
    }

    /// Adds the private stack's deny-list and fleet hosts, and never its allow-list: the only way a private stack enters
    /// the rules, so they only find more (design note d28-alias, fix check r5, D28-R5-01). No function adds a private
    /// stack's allow-list to any scan (design note privacy-own-allow, sol-loop r1, S2: `with_private` is gone). Without a
    /// private stack, or a deny-list, that is noted; a deny-list or fleet config that is there but can't be read is an
    /// error, so the check fails closed.
    pub fn with_private_terms(mut self, private: Option<&Stack>) -> Result<Rules, String> {
        let Some(private) = private else {
            self.notes.push("no private stack: the deny and fleet classes were not checked".into());
            return Ok(self);
        };
        let deny = private.root.join("privacy").join("deny.txt");
        match fsguard::read_string(&deny) {
            Ok(text) => self.deny.extend(list_lines(&text).map(fold_term)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.notes.push("the private stack has no privacy/deny.txt".into())
            }
            Err(e) => return Err(format!("{}: {e}", deny.display())),
        }
        if let Some(cfg) = &private.manifest.privacy.fleet_config {
            let hosts = fleet_hosts(&expand_home(cfg))
                .map_err(|e| format!("fleet config not read: {e}"))?;
            self.hosts.extend(hosts);
        }
        Ok(self)
    }
}

fn list_lines(text: &str) -> impl Iterator<Item = &str> {
    text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#'))
}

/// Host names from a fleet config: the `[host.*]` and `[machine.*]` keys and each link's `target`.
pub fn fleet_hosts(path: &Path) -> Result<Vec<String>, String> {
    let text = fsguard::read_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let value: toml::Table =
        toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut out = BTreeSet::new();
    for table in ["host", "machine"] {
        if let Some(toml::Value::Table(hosts)) = value.get(table) {
            for (name, h) in hosts {
                out.insert(fold_term(name));
                if let Some(target) =
                    h.get("link").and_then(|l| l.get("target")).and_then(|t| t.as_str())
                {
                    out.insert(fold_term(target));
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
            // The named-key rule: after a secret-named key, any value of 16 or more token characters is a secret,
            // whatever its alphabet. The patterns above match a secret's own shape or place and need no key.
            p(Class::Secret, r#"(?i)\b(?:password|passwd|secret|token|api[_-]?key)\s*[:=]\s*["']?([A-Za-z0-9/+_\-]{16,})"#, 1),
        ]
    })
}

/// Text as deny and fleet terms are compared: compatibility-decomposed (NFKD), Unicode default full case folding
/// (ß and SS, ς and σ), decomposed again, each run of whitespace one space. Each folded byte keeps the byte range
/// of the char it came from.
fn fold(text: &str) -> (String, Vec<(usize, usize)>) {
    let nfkd = icu_normalizer::DecomposingNormalizerBorrowed::new_nfkd();
    let case = icu_casemap::CaseMapper::new();
    let mut out = String::with_capacity(text.len());
    let mut from = Vec::with_capacity(text.len());
    let mut push = |out: &mut String, c: char, origin: (usize, usize)| {
        out.push(c);
        from.extend(std::iter::repeat_n(origin, c.len_utf8()));
    };
    for (i, c) in text.char_indices() {
        let origin = (i, c.len_utf8());
        if c.is_whitespace() {
            if !out.ends_with(' ') {
                push(&mut out, ' ', origin);
            }
        } else if c.is_ascii() {
            push(&mut out, c.to_ascii_lowercase(), origin);
        } else {
            // Folded text need not be normalised, so it is decomposed again (Unicode's compatibility caseless match).
            let decomposed = nfkd.normalize(c.encode_utf8(&mut [0; 4])).into_owned();
            for d in nfkd.normalize(&case.fold_string(&decomposed)).chars() {
                push(&mut out, d, origin);
            }
        }
    }
    (out, from)
}

/// A deny or fleet term folded as `fold` folds text.
pub fn fold_term(term: &str) -> String {
    fold(term.trim()).0
}

pub fn scan(text: &str, rules: &Rules) -> Vec<Hit> {
    let mut hits = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let n = i + 1;
        for p in patterns().iter().filter(|p| rules.classes.contains(&p.class)) {
            for caps in p.re.captures_iter(line) {
                let Some(m) = caps.get(p.group) else { continue };
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
    }
    // Terms match on the whole folded text, so a phrase wrapped across lines is found; the hit is placed on the
    // line it starts on, its span ending at that line's end at most and its text the whole phrase.
    let (folded, from) = fold(text);
    let starts = line_starts(text);
    for (class, terms) in [(Class::Fleet, &rules.hosts), (Class::Deny, &rules.deny)] {
        if !rules.classes.contains(&class) {
            continue;
        }
        for term in terms.iter().map(|t| fold_term(t)).filter(|t| !t.is_empty()) {
            for (start, _) in folded.match_indices(term.as_str()) {
                let end = start + term.len();
                let before = folded[..start].chars().next_back();
                let after = folded[end..].chars().next();
                let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
                if word(before) || word(after) {
                    continue;
                }
                let (s, e) = (from[start].0, from[end - 1].0 + from[end - 1].1);
                let i = starts.partition_point(|&l| l <= s) - 1;
                let line = text[starts[i]..].lines().next().unwrap_or("");
                hits.push(Hit {
                    class,
                    line: i + 1,
                    start: s - starts[i],
                    end: (e - starts[i]).min(line.len()),
                    text: text[s..e].to_string(),
                });
            }
        }
    }
    hits.retain(|h| !rules.allow.contains(&h.text));
    hits.sort_by_key(|h| (h.line, h.start));
    hits.dedup_by(|a, b| a.line == b.line && a.start < b.end && b.start < a.end);
    hits
}

/// The byte offset each line starts at.
fn line_starts(text: &str) -> Vec<usize> {
    std::iter::once(0).chain(text.match_indices('\n').map(|(i, _)| i + 1)).collect()
}

/// Replaces each hit's span with `<private:class>`, the placeholder the owner then rewords. A phrase wrapped across
/// lines is replaced whole, up to the end of its `text`.
pub fn strip(text: &str, hits: &[Hit]) -> String {
    let starts = line_starts(text);
    let mut spans: Vec<(usize, usize, Class)> = hits
        .iter()
        .filter_map(|h| {
            let line = *starts.get(h.line.checked_sub(1)?)?;
            let s = line + h.start;
            let e = if text.get(s..)?.starts_with(h.text.as_str()) {
                s + h.text.len()
            } else {
                line + h.end
            };
            Some((s, e, h.class))
        })
        .collect();
    spans.sort();
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    for (s, e, class) in spans {
        if s < pos {
            continue;
        }
        out.push_str(&text[pos..s]);
        out.push_str(&format!("<private:{}>", class.name()));
        pos = e;
    }
    out.push_str(&text[pos..]);
    out
}

/// The result of scanning a set of files.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Scan {
    pub files_scanned: usize,
    /// Files that are not text: scanned only as far as their bytes read as text; listed so nothing passes silently.
    pub binary: Vec<PathBuf>,
    /// Staged binary files the repo doesn't mark `lords-binary` in .gitattributes: the staged check refuses them.
    pub unmarked: Vec<PathBuf>,
    /// A hit on line 0 is in the file's name.
    pub hits: Vec<(PathBuf, Hit)>,
}

/// The attribute that allows a staged binary file (`*.png lords-binary` in .gitattributes).
pub const BINARY_ATTR: &str = "lords-binary";

/// A file's bytes as the text it is, and whether they are text: UTF-8 without NUL bytes, or UTF-16 with a byte
/// order mark. Anything else is not text, and reads as its lossy UTF-8 reading and printable ASCII runs. This
/// reading is the one a hit's line refers to; `Scan::add` scans every other reading too (`readings`).
pub fn decode(bytes: &[u8]) -> (String, bool) {
    match bytes {
        [0xff, 0xfe, rest @ ..] => return (utf16(rest, false), true),
        [0xfe, 0xff, rest @ ..] => return (utf16(rest, true), true),
        _ => {}
    }
    if let Some(text) = utf8(bytes) {
        return (text.to_string(), true);
    }
    (byte_text(bytes), false)
}

/// Every reading of a file's bytes that the privacy check scans, whatever they look like (Sol review r3, R3-07;
/// r4, R4-02): UTF-8 (lossy, with its printable ASCII runs) and UTF-16 little- and big-endian, from the first byte
/// and from the second. No reading is chosen over another: bytes that are valid UTF-8 can be UTF-16 text too
/// (`中中中中` in UTF-16LE is `-N-N-N-N`), and text in a script without Latin letters has no NUL bytes. What this
/// doesn't read is listed in the design (Privacy, "Not covered").
pub fn readings(bytes: &[u8]) -> Vec<String> {
    let mut all = vec![byte_text(bytes)];
    for from in [bytes, bytes.get(1..).unwrap_or_default()] {
        for be in [false, true] {
            all.push(utf16(from, be));
        }
    }
    all
}

fn utf16(bytes: &[u8], be: bool) -> String {
    let units: Vec<u16> =
        bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| {
                if be {
                    u16::from_be_bytes([c[0], c[1]])
                } else {
                    u16::from_le_bytes([c[0], c[1]])
                }
            })
            .collect();
    String::from_utf16_lossy(&units)
}

fn utf8(bytes: &[u8]) -> Option<&str> {
    if bytes.contains(&0) {
        return None;
    }
    std::str::from_utf8(bytes).ok()
}

/// What can be read in bytes that aren't text: the lossy UTF-8 reading and the printable ASCII runs.
fn byte_text(bytes: &[u8]) -> String {
    let mut text = String::from_utf8_lossy(bytes).into_owned();
    let mut run = String::new();
    for &b in bytes.iter().filter(|&&b| b != 0).chain([&b'\n']) {
        if b == b'\t' || (0x20..0x7f).contains(&b) {
            run.push(char::from(b));
            continue;
        }
        if run.len() >= 4 {
            text.push('\n');
            text.push_str(&run);
        }
        run.clear();
    }
    text
}

impl Scan {
    /// Scans one file's content (not its name: see `add_name`).
    pub fn add(&mut self, path: PathBuf, bytes: &[u8], rules: &Rules) {
        let (text, is_text) = decode(bytes);
        let mut hits = scan(&text, rules);
        for reading in readings(bytes) {
            hits.extend(scan(&reading, rules));
        }
        // The readings overlap: one hit per text, the one in the file's own reading first.
        let mut seen = BTreeSet::new();
        hits.retain(|h| seen.insert((h.class, h.text.clone())));
        if is_text {
            self.files_scanned += 1;
        } else {
            self.binary.push(path.clone());
        }
        self.hits.extend(hits.into_iter().map(|h| (path.clone(), h)));
    }

    /// Scans a file's repo-relative path, as text: a name can leak as much as content.
    pub fn add_name(&mut self, path: &Path, rules: &Rules) {
        let name = path.to_string_lossy().replace('\\', "/");
        for h in scan(&name, rules) {
            self.hits.push((path.to_path_buf(), Hit { line: 0, ..h }));
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
        // Only a listed path that isn't there (deleted in the worktree) or is a dir (a submodule) goes; one whose
        // kind can't be told stays, so reading it fails loudly (Sol review r5, R5-04's class).
        .filter(|p| {
            !matches!(
                crate::fsguard::kind_here(&repo.join(p)),
                Ok(crate::fsguard::Kind::Missing | crate::fsguard::Kind::Dir)
            )
        })
        .collect();
    files.sort();
    files.dedup();
    Ok(files)
}

pub fn scan_tree(repo: &Path, rules: &Rules) -> Result<Scan, String> {
    let mut scan = Scan::default();
    for rel in tree_files(repo)? {
        let bytes =
            fsguard::read_bytes(repo.join(&rel)).map_err(|e| format!("{}: {e}", rel.display()))?;
        scan.add_name(&rel, rules);
        scan.add(rel, &bytes, rules);
    }
    Ok(scan)
}

/// The staged version of every staged entry that isn't a deletion (added, copied, modified, renamed, a type or mode
/// change, Sol review r7, R7-01), and its path: what the commit would contain. Each is read as the blob the index
/// names, by its mode and object id (`git cat-file blob`, never a rendering of another object; r7 fix check,
/// FC-03): a gitlink (mode 160000) or any entry whose blob can't be read is an error, never left out. Binary files
/// the staged .gitattributes doesn't mark `lords-binary` are listed in `unmarked`.
pub fn scan_staged(repo: &Path, rules: &Rules) -> Result<Scan, String> {
    // Raw records, NUL-separated: `:<old mode> <new mode> <old id> <new id> <status>`, then the path.
    // The index a hook names (a `git commit <paths>` commits from a temporary one), not the repo's own.
    let raw = git::run_index(
        repo,
        &["diff", "--cached", "--raw", "--no-abbrev", "--no-renames", "--diff-filter=d", "-z"],
    )?;
    let mut fields = raw.split('\0');
    let mut scan = Scan::default();
    while let Some(head) = fields.next().filter(|s| !s.is_empty()) {
        let rel = fields.next().filter(|s| !s.is_empty()).ok_or_else(|| {
            format!("git diff --raw: a record without its path ({head:?}); nothing was scanned")
        })?;
        let parts: Vec<&str> = head.trim_start_matches(':').split(' ').collect();
        let [_, mode, _, id, _] = parts.as_slice() else {
            return Err(format!("git diff --raw: a record that doesn't parse ({head:?})"));
        };
        if *mode == "160000" {
            return Err(format!(
                "{rel}: a staged gitlink (submodule, mode 160000); the privacy check reads only file contents, so it refuses the commit. Commit the submodule's own repo, or unstage it"
            ));
        }
        if !matches!(*mode, "100644" | "100755" | "120000") {
            return Err(format!("{rel}: staged with mode {mode}, not a file; refused"));
        }
        let bytes = git::bytes(repo, &["cat-file", "blob", id])
            .map_err(|e| format!("{rel}: its staged blob can't be read ({e}); refused"))?;
        scan.add_name(Path::new(rel), rules);
        scan.add(PathBuf::from(rel), &bytes, rules);
    }
    if !scan.binary.is_empty() {
        let mut args = vec!["check-attr", "--cached", "-z", BINARY_ATTR, "--"];
        let paths: Vec<String> =
            scan.binary.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        args.extend(paths.iter().map(String::as_str));
        let out = git::run_index(repo, &args)?;
        // `-z` output: path, attribute, value, each NUL-terminated.
        let fields: Vec<&str> = out.split('\0').collect();
        let marked: BTreeSet<&str> =
            fields.chunks(3).filter(|f| f.len() == 3 && f[2] == "set").map(|f| f[0]).collect();
        scan.unmarked = scan
            .binary
            .iter()
            .filter(|p| !marked.contains(p.to_string_lossy().as_ref()))
            .cloned()
            .collect();
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
/// Binary files are shown as text (`--text`) and NUL bytes dropped, so their readable parts and UTF-16 text are
/// scanned too; what a binary format compresses can't be.
pub fn scan_history(repo: &Path, rules: &Rules) -> Result<HistoryScan, String> {
    let out = git::run(
        repo,
        &["log", "--all", "-p", "--text", "--no-color", "--no-ext-diff", "--format=%x1e%H%n%B%x1f"],
    )?
    .replace('\0', "");
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

// ---------------------------------------------------------------- the checkout's own mode (D28 r2)

/// Where a stack's manifest is at a checkout's top: the private stack's, and the public one's (`stack/`).
pub const MANIFESTS: [&str; 2] = ["stack.toml", "stack/stack.toml"];

/// What a checkout's own stack says about the mode of a privacy check that runs in it (D28 r2, design note
/// 2026-11-08-lords-d28-r2, C1 and C4): the mode belongs to the checkout that runs the check, never to the one that
/// installed its hooks, since every worktree of a repo runs the same hooks (review r1, R1-01).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckoutStack {
    /// No copy of a manifest anywhere in the checkout, or no git checkout.
    None,
    /// Every copy names this visibility.
    Is(Visibility),
    /// A copy is there, but the mode can't be told from the copies: why, in one line.
    Unknown(Untold),
}

/// Why a checkout's stack can't be told (design note privacy-own-allow, sol-loop r1, S1). `full` is all that was found
/// out, git's own words included: every scan and hook says it (`Display`). `brief` is the place and a category only:
/// `privacy --explain` says it, since git's words can quote a config value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Untold {
    pub brief: String,
    pub full: String,
}

impl Untold {
    /// One of the tool's own lines, which name places and categories only: the same both ways.
    pub fn said(why: String) -> Untold {
        Untold { brief: why.clone(), full: why }
    }

    /// git's own words, `words`, which `brief` keeps behind `category`.
    fn git(category: &str, words: String) -> Untold {
        Untold { brief: category.to_string(), full: words }
    }

    /// The same, at the place `at`.
    fn at(self, at: &str) -> Untold {
        Untold { brief: format!("{at}: {}", self.brief), full: format!("{at}: {}", self.full) }
    }
}

impl std::fmt::Display for Untold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.full)
    }
}

impl CheckoutStack {
    /// Whether the checkout has a stack's manifest, readable or not: install's choice of a stack repo's hooks.
    pub fn found(&self) -> bool {
        !matches!(self, CheckoutStack::None)
    }
}

/// Every copy of the manifests (`MANIFESTS`) of the checkout `dir` is in: in HEAD, in the index a hook checks (the
/// commit's own, as `scan_staged` reads it), and on disk, tracked or not (review r1, R1-02). All naming one
/// visibility is that visibility. Anything else is `Unknown`, never a guess: a copy that can't be read or names no
/// visibility, and copies that disagree, two manifests or a change that is only partly staged. Only a dir git says is
/// in no repository has no stack; a checkout git can't tell is `Unknown`, with git's answer (R2-02).
///
/// A copy counts by structure, never by matching the names that alias it (fix check r3, R2-01; design note
/// 2026-11-08-lords-d28-alias): on disk only through entries listed under their exact canonical names
/// (`exactly_listed`), and in git only at a canonical path as a regular file. Any other git entry a filesystem may take
/// for a manifest (`git_entry`) is `Unknown`, named, so no alias of a public copy is left out.
pub fn checkout_stack(dir: &Path) -> CheckoutStack {
    let top = match git::top_level(dir) {
        Ok(Some(top)) => top,
        Ok(None) => return CheckoutStack::None,
        Err(e) => {
            let why = "git can't tell which checkout this is";
            let brief =
                "git can't tell which checkout this is (its config or the repo can't be read)";
            return CheckoutStack::Unknown(Untold::git(brief, format!("{why} ({e})")));
        }
    };
    // Each copy's failure: the tool's own lines as they are (`said`); git's words behind a category (`git`).
    let said = Untold::said;
    let git_read = |e: String| Untold::git("git couldn't read it", e);
    let mut copies: Vec<(String, Result<Visibility, Untold>)> = Vec::new();
    for rel in MANIFESTS {
        let path = top.join(rel);
        // A disk failure's text names the path and the OS's words: a category, before `Untold` is built (sol-loop r5,
        // S13). `exactly_listed`'s alias line names only canonical names, and is the tool's own.
        let unread = |e: String| Untold::git("can't be read", e);
        let text = match fsguard::kind(&path) {
            Ok(fsguard::Kind::Missing) => continue,
            Ok(_) => exactly_listed(&top, rel).and_then(|()| {
                fsguard::read_text(&path)
                    .map_err(unread)
                    .and_then(|t| t.ok_or_else(|| said("gone".to_string())))
            }),
            Err(e) => Err(unread(e)),
        };
        let visibility = text.and_then(|t| visibility_of(&t).map_err(said));
        copies.push((format!("{rel} on disk"), visibility));
    }
    // Each blob read once: HEAD's copy and the staged one are mostly the same object.
    let mut blobs = std::collections::BTreeMap::new();
    let mut blob = |id: &str| -> Result<Visibility, Untold> {
        let found = blobs.entry(id.to_string()).or_insert_with(|| -> Result<Visibility, Untold> {
            let bytes = git::bytes(&top, &["cat-file", "blob", id]).map_err(git_read)?;
            let text = String::from_utf8(bytes).map_err(|_| said("isn't UTF-8 text".into()))?;
            visibility_of(&text).map_err(said)
        });
        found.clone()
    };
    // Every git entry, at any depth, is put to `git_entry`: `<mode> <id> <stage>\t<path>` from the index (each stage of
    // a conflict a copy), `<mode> <type> <id>\t<path>` from HEAD's whole tree. Two listings, not a pathspec: git's
    // case folding is ASCII only and knows no other filesystem rule.
    let mut entry = |path: &str, mode: &str, id: &str, at: String| -> Option<_> {
        match git_entry(path, mode) {
            GitEntry::Other => None,
            GitEntry::Manifest => Some((at, blob(id))),
            GitEntry::Alias(why) => Some((at, Err(said(why)))),
        }
    };
    // A listing's record git couldn't be parsed as: its path is named, its words are git's.
    let odd = |listing: &str, rec: &str| git_read(format!("`{listing}` said {rec:?}"));
    let staged = ["ls-files", "-s", "-z", "--full-name"];
    match git::run_index(&top, &staged) {
        Ok(out) => {
            for rec in out.split('\0').filter(|r| !r.is_empty()) {
                let (head, path) = rec.split_once('\t').unwrap_or((rec, ""));
                copies.extend(match head.split(' ').collect::<Vec<_>>().as_slice() {
                    [mode, id, _] => entry(path, mode, id, format!("{path} staged")),
                    _ => Some((format!("{path} staged"), Err(odd("git ls-files -s", rec)))),
                });
            }
        }
        Err(e) => copies.push(("the index".to_string(), Err(git_read(e)))),
    }
    // No HEAD copy only where git proves HEAD unborn; a HEAD naming a commit that can't be read is `Unknown` (fix check
    // r6, R6-01).
    match git::status(&top, &["rev-parse", "-q", "--verify", "HEAD^{commit}"]) {
        Ok(false) => match git::head_unborn(&top) {
            Ok(true) => {}
            Ok(false) => copies.push((
                "HEAD".to_string(),
                Err(said("names an object that isn't a commit git can read".into())),
            )),
            Err(e) => copies.push(("HEAD".to_string(), Err(git_read(e)))),
        },
        Ok(true) => match git::run(&top, &["ls-tree", "-r", "-z", "--full-tree", "HEAD"]) {
            Ok(out) => {
                for rec in out.split('\0').filter(|r| !r.is_empty()) {
                    let (head, path) = rec.split_once('\t').unwrap_or((rec, ""));
                    copies.extend(match head.split(' ').collect::<Vec<_>>().as_slice() {
                        [mode, _, id] => entry(path, mode, id, format!("{path} in HEAD")),
                        _ => Some((format!("{path} in HEAD"), Err(odd("git ls-tree", rec)))),
                    });
                }
            }
            Err(e) => copies.push(("HEAD".to_string(), Err(git_read(e)))),
        },
        Err(e) => copies.push(("HEAD".to_string(), Err(git_read(e)))),
    }
    agreed(copies)
}

/// Whether each step of the manifest path `rel` below `top` is an entry its dir lists under exactly that name, so the
/// file read at `top/rel` is the canonical one, not one a short name, another case, a trailing dot or space or an
/// ignorable code point reaches (design note d28-alias, the disk rule). No filesystem's aliasing is modelled: a name
/// that opens but isn't listed is an error, so discovery is `Unknown`.
fn exactly_listed(top: &Path, rel: &str) -> Result<(), Untold> {
    let mut dir = top.to_path_buf();
    let mut shown = "the checkout's top".to_string();
    for name in rel.split('/') {
        let names =
            fsguard::list_dir(&dir).map_err(|e| Untold::git("its dir can't be listed", e))?;
        if !names.iter().any(|p| p.file_name() == Some(std::ffi::OsStr::new(name))) {
            return Err(Untold::said(format!(
                "opens, but {shown} lists no entry named exactly {name:?}, so another name reaches it (a short name, \
                 another case, a trailing dot or space): name it exactly {rel}"
            )));
        }
        dir.push(name);
        shown = format!("{name}/");
    }
    Ok(())
}

/// What a git entry is to discovery (design note d28-alias, the git rule).
enum GitEntry {
    /// A regular file at a manifest's own path: it counts by what it names.
    Manifest,
    /// Something a filesystem may take for a manifest, or a link or submodule where one goes: why, in a line.
    Alias(String),
    Other,
}

/// The git rule: an entry whose basename (after the last `/` or `\`, which Windows takes for a separator) a filesystem
/// may take for `stack.toml` (`folds_to`) is a manifest only at a canonical path and as a regular file (`100644`,
/// `100755`). Elsewhere, at any depth, it is an alias, a fixture's included: renaming it settles that. So is a symlink
/// or submodule at the top whose name folds to `stack`: what it reaches isn't this checkout's content.
fn git_entry(path: &str, mode: &str) -> GitEntry {
    let parts: Vec<&str> = path.split(['/', '\\']).collect();
    let base = parts.last().copied().unwrap_or(path);
    let what = |mode: &str| match mode {
        "120000" => "a symlink".to_string(),
        "160000" => "a submodule".to_string(),
        m => format!("mode {m}"),
    };
    if folds_to(base, MANIFESTS[0]) {
        if !MANIFESTS.contains(&path) {
            return GitEntry::Alias(format!(
                "is named like a stack manifest but isn't {} or {}, so a filesystem may read it as one: rename or \
                 remove it (a copy in HEAD counts until a commit removes it)",
                MANIFESTS[0], MANIFESTS[1]
            ));
        }
        return match mode {
            "100644" | "100755" => GitEntry::Manifest,
            m => GitEntry::Alias(format!("is {} where the manifest goes", what(m))),
        };
    }
    if parts.len() == 1 && folds_to(base, "stack") && matches!(mode, "120000" | "160000") {
        return GitEntry::Alias(format!("is {} where the stack's dir goes", what(mode)));
    }
    GitEntry::Other
}

/// Whether some filesystem may take the name `name` for `canonical` (an ASCII name). The rules, all applied, so the
/// answer is a superset of every one: HFS+ ignores some code points (git's own `is_hfs_dotgit` list); NTFS reads a name
/// up to a `:` as the file (`stack.toml::$DATA` is its data) and drops trailing dots and spaces; case-insensitive
/// filesystems fold case, Unicode's folds included.
fn folds_to(name: &str, canonical: &str) -> bool {
    let kept: String = name
        .chars()
        .filter(|c| {
            !matches!(c, '\u{200C}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{206A}'..='\u{206F}' | '\u{FEFF}')
        })
        .collect();
    let file = kept.split(':').next().unwrap_or_default();
    same_but_case(file.trim_end_matches(['.', ' ']), canonical)
}

fn same_but_case(a: &str, b: &str) -> bool {
    a.chars().count() == b.chars().count()
        && a.chars().zip(b.chars()).all(|(x, y)| {
            x == y || x.to_lowercase().eq(y.to_lowercase()) || x.to_uppercase().eq(y.to_uppercase())
        })
}

/// The visibility a manifest's text names, as the manifest's schema reads it. Why it can't is said by line only: the
/// parser's message can hold the text itself, a value in an unknown variant (package review privownallow r1, R1-02).
fn visibility_of(text: &str) -> Result<Visibility, String> {
    #[derive(serde::Deserialize)]
    struct Head {
        visibility: Option<Visibility>,
    }
    let head: Head = toml::from_str(text).map_err(|e| {
        let line = e.span().and_then(|s| text.get(..s.start)).map(|t| t.matches('\n').count() + 1);
        match line {
            Some(n) => format!("can't be read as a manifest (line {n})"),
            None => "can't be read as a manifest".to_string(),
        }
    })?;
    head.visibility.ok_or_else(|| "names no visibility".to_string())
}

/// The one visibility every copy names, or why there isn't one.
fn agreed(copies: Vec<(String, Result<Visibility, Untold>)>) -> CheckoutStack {
    let mut named = Vec::new();
    for (at, v) in copies {
        match v {
            Ok(v) => named.push((at, v)),
            Err(e) => return CheckoutStack::Unknown(e.at(&at)),
        }
    }
    match named.first() {
        None => CheckoutStack::None,
        Some((_, v)) if named.iter().all(|(_, w)| w == v) => CheckoutStack::Is(*v),
        Some(_) => CheckoutStack::Unknown(Untold::said(format!(
            "this checkout's stack manifests disagree ({})",
            named.iter().map(|(at, v)| format!("{at}: {v}")).collect::<Vec<_>>().join("; ")
        ))),
    }
}

/// Whether a privacy check in a checkout whose stack is `stack` runs secrets only, and the line to say when its mode
/// isn't the one asked for (D28 r2, C1 and C2). `by_stack` (a stack repo's hooks): the stack's mode, the whole policy
/// where it can't be told. `secrets_only`: only where the checkout has no stack or a private one, so no caller gives
/// a public checkout secrets-only mode. Neither: the whole policy.
pub fn checkout_mode(
    stack: &CheckoutStack,
    by_stack: bool,
    secrets_only: bool,
) -> (bool, Option<String>) {
    let whole = "so the whole privacy policy is checked";
    match stack {
        CheckoutStack::Is(Visibility::Private) => (by_stack || secrets_only, None),
        CheckoutStack::None if by_stack => (
            false,
            Some(format!(
                "this checkout has no stack manifest ({} or {}), {whole}",
                MANIFESTS[0], MANIFESTS[1]
            )),
        ),
        CheckoutStack::None => (secrets_only, None),
        CheckoutStack::Is(Visibility::Public) if secrets_only => {
            (false, Some(format!("this checkout's stack is public, {whole}, not --secrets-only")))
        }
        CheckoutStack::Is(Visibility::Public) => (false, None),
        // The place and a category only: `privacy` prints this line in every mode (sol-loop r4).
        CheckoutStack::Unknown(why) if by_stack || secrets_only => {
            (false, Some(format!("{}, {whole}", why.brief)))
        }
        CheckoutStack::Unknown(_) => (false, None),
    }
}
