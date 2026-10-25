//! Text that says the same thing twice, and entries on one topic that disagree. Both are candidates for the
//! owner: the tool shows the two texts, it never picks one.

use crate::check::{coapply, related};
use crate::finding::Finding;
use crate::model::{Entry, Kind};
use crate::stack::Stacks;
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

/// A sentence this long, found word for word in two entries, is a restated rule.
const SENTENCE_WORDS: usize = 8;
/// Word 5-shingles shared by this share of the smaller entry make the pair near-duplicates.
const SHINGLE_SHARE: f64 = 0.6;
const SHINGLE: usize = 5;
const MIN_WORDS: usize = 30;

fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric() && c != '%')
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect()
}

fn sentences(body: &str) -> BTreeSet<String> {
    body.split(['.', '!', '?', '\n', ';'])
        .map(words)
        .filter(|w| w.len() >= SENTENCE_WORDS)
        .map(|w| w.join(" "))
        .collect()
}

fn shingles(body: &str) -> BTreeSet<String> {
    let w = words(body);
    if w.len() < MIN_WORDS {
        return BTreeSet::new();
    }
    w.windows(SHINGLE).map(|s| s.join(" ")).collect()
}

/// Numbers with a unit, per normalised unit: "at most 2 helpers" and "3 %" become `helper → {2}`, `% → {3}`.
pub fn quantities(body: &str) -> BTreeMap<String, BTreeSet<String>> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r"(?i)\b(\d+(?:\.\d+)?)\s*(%|percent|minutes?|mins?|hours?|h|seconds?|s|gb|mb|kb|agents?|helpers?|threads?|rounds?|days?|lines?|kb)\b|(\d+(?:\.\d+)?)\s*%",
        )
        .expect("pattern compiles")
    });
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for c in re.captures_iter(body) {
        let (num, unit) = match (c.get(1), c.get(2), c.get(3)) {
            (Some(n), Some(u), _) => (n.as_str(), u.as_str().to_lowercase()),
            (_, _, Some(n)) => (n.as_str(), "%".to_string()),
            _ => continue,
        };
        let unit = match unit.as_str() {
            "percent" => "%".to_string(),
            "min" | "mins" | "minute" | "minutes" => "min".into(),
            "h" | "hour" | "hours" => "h".into(),
            "s" | "second" | "seconds" => "s".into(),
            u => u.trim_end_matches('s').to_string(),
        };
        out.entry(unit).or_default().insert(num.to_string());
    }
    out
}

/// Opposed keywords: each pair is (one side, other side). Only whole words count.
const OPPOSED: [(&[&str], &[&str]); 3] = [
    (&["always"], &["never"]),
    (&["allowed", "permitted"], &["forbidden", "prohibited"]),
    (&["required", "mandatory"], &["optional"]),
];

fn opposed(a: &str, b: &str) -> Option<(String, String)> {
    let wa: BTreeSet<String> = words(a).into_iter().collect();
    let wb: BTreeSet<String> = words(b).into_iter().collect();
    for (x, y) in OPPOSED {
        let has = |set: &BTreeSet<String>, side: &[&str]| {
            side.iter().find(|w| set.contains(**w)).map(|w| w.to_string())
        };
        if let (Some(p), Some(q)) = (has(&wa, x), has(&wb, y)) {
            return Some((p, q));
        }
        if let (Some(p), Some(q)) = (has(&wa, y), has(&wb, x)) {
            return Some((p, q));
        }
    }
    None
}

pub fn check(stacks: &Stacks, out: &mut Vec<Finding>) {
    let entries: Vec<&Entry> =
        stacks.entries().filter(|e| e.active() && e.meta.kind != Kind::Memory).collect();
    let sents: Vec<BTreeSet<String>> = entries.iter().map(|e| sentences(&e.body)).collect();
    let shings: Vec<BTreeSet<String>> = entries.iter().map(|e| shingles(&e.body)).collect();
    for i in 0..entries.len() {
        for j in i + 1..entries.len() {
            let (a, b) = (entries[i], entries[j]);
            if related(a, b) {
                continue;
            }
            let pair = || vec![b.id().to_string()];
            let shared: Vec<&String> = sents[i].intersection(&sents[j]).collect();
            if let Some(first) = shared.first() {
                out.push(
                    Finding::candidate(
                        "duplicate-text",
                        format!(
                            "{} sentence(s) appear in both, e.g. \"{first}\"; give it one home",
                            shared.len()
                        ),
                    )
                    .at(&a.meta_file, None)
                    .entry(a.id())
                    .related(pair()),
                );
            } else if !shings[i].is_empty() && !shings[j].is_empty() {
                let common = shings[i].intersection(&shings[j]).count() as f64;
                let smaller = shings[i].len().min(shings[j].len()) as f64;
                if common / smaller >= SHINGLE_SHARE {
                    out.push(
                        Finding::candidate(
                            "near-duplicate",
                            format!(
                                "{:.0}% of the shorter text is shared",
                                100.0 * common / smaller
                            ),
                        )
                        .at(&a.meta_file, None)
                        .entry(a.id())
                        .related(pair()),
                    );
                }
            }
            let topics: Vec<&String> =
                a.meta.topics.iter().filter(|t| b.meta.topics.contains(t)).collect();
            if topics.is_empty() || !coapply(stacks, &a.scope, &b.scope) {
                continue;
            }
            let topic = topics.iter().map(|t| t.as_str()).collect::<Vec<_>>().join(", ");
            let (qa, qb) = (quantities(&a.body), quantities(&b.body));
            for (unit, va) in &qa {
                if let Some(vb) = qb.get(unit) {
                    if va.is_disjoint(vb) {
                        let list =
                            |v: &BTreeSet<String>| v.iter().cloned().collect::<Vec<_>>().join("/");
                        out.push(
                            Finding::candidate(
                                "conflict",
                                format!("topic {topic}: {} {unit} here, {} {unit} in the other, and neither names the other", list(va), list(vb)),
                            )
                            .at(&a.meta_file, None)
                            .entry(a.id())
                            .related(pair()),
                        );
                    }
                }
            }
            if let Some((p, q)) = opposed(&a.body, &b.body) {
                out.push(
                    Finding::candidate("conflict", format!("topic {topic}: \"{p}\" here, \"{q}\" in the other, and neither names the other"))
                        .at(&a.meta_file, None)
                        .entry(a.id())
                        .related(pair()),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Spec (design: Layers, unnamed conflict): "different numbers for the same quantity" means the same unit,
    // whatever its spelling.
    #[test]
    fn quantities_group_by_unit() {
        let q =
            quantities("Timing delta = 3 % default; at most 2 helpers; wait 30 minutes or 80 min.");
        assert_eq!(q["%"], BTreeSet::from(["3".to_string()]));
        assert_eq!(q["helper"], BTreeSet::from(["2".to_string()]));
        assert_eq!(q["min"], BTreeSet::from(["30".to_string(), "80".to_string()]));
    }
}
