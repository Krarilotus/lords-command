//! YAML frontmatter: split it from the body, and change top-level keys without touching the rest.

/// A file split at its frontmatter. `yaml` is `None` when the file has none.
pub struct Split<'a> {
    pub yaml: Option<&'a str>,
    pub body: &'a str,
    /// 1-based line where `body` starts.
    pub body_line: usize,
}

pub fn split(text: &str) -> Split<'_> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let none = Split { yaml: None, body: text, body_line: 1 };
    let Some(after_open) = text.strip_prefix("---\n").or_else(|| text.strip_prefix("---\r\n"))
    else {
        return none;
    };
    let open_len = text.len() - after_open.len();
    let mut pos = 0;
    let mut line = 2;
    for l in after_open.split_inclusive('\n') {
        if l.trim_end_matches(['\r', '\n']) == "---" {
            let yaml = &after_open[..pos];
            let body = &text[open_len + pos + l.len()..];
            return Split { yaml: Some(yaml), body, body_line: line + 1 };
        }
        pos += l.len();
        line += 1;
    }
    none
}

/// Replaces (or adds) top-level keys in a YAML block. Each value is YAML text for that key, written as
/// `key: value` when it is one line, else `key:` followed by its lines. Other keys, comments and order stay.
pub fn set_keys(yaml: &str, keys: &[(&str, Option<String>)]) -> String {
    let nl = if yaml.contains("\r\n") { "\r\n" } else { "\n" };
    let mut out: Vec<&str> = Vec::new();
    let mut skipping = false;
    for line in yaml.lines() {
        if line.trim().is_empty() || line.starts_with('#') {
            // A blank line or a column-0 comment ends a value block.
            skipping = false;
        } else if !line.starts_with([' ', '\t', '-']) {
            let key = line.split(':').next().unwrap_or("").trim();
            skipping = keys.iter().any(|(k, _)| *k == key);
        }
        // Indented lines and `- ` items continue the value of the key above them.
        if !skipping {
            out.push(line);
        }
    }
    let mut text = out.join(nl);
    for (key, value) in keys {
        let Some(value) = value else { continue };
        if !text.is_empty() && !text.ends_with(nl) {
            text.push_str(nl);
        }
        if value.contains('\n') {
            text.push_str(&format!("{key}:{nl}"));
            for l in value.lines() {
                text.push_str(&format!("  {l}{nl}"));
            }
        } else {
            text.push_str(&format!("{key}: {value}{nl}"));
        }
    }
    if !text.ends_with(nl) {
        text.push_str(nl);
    }
    text
}

/// The value text of a top-level key, in the form `set_keys` takes: the inline value, or the block's lines
/// with their two-space indent removed.
pub fn get_key(yaml: &str, key: &str) -> Option<String> {
    let mut lines = yaml.lines();
    let first = lines
        .by_ref()
        .find(|l| !l.starts_with([' ', '\t', '-', '#']) && l.split(':').next() == Some(key))?;
    let inline = first.split_once(':').map(|(_, v)| v.trim()).unwrap_or("");
    if !inline.is_empty() {
        return Some(inline.to_string());
    }
    let block: Vec<&str> = lines
        .take_while(|l| l.starts_with([' ', '\t', '-']))
        .map(|l| l.strip_prefix("  ").unwrap_or(l))
        .collect();
    Some(block.join("\n"))
}

/// Rewrites a whole file with changed frontmatter keys; adds frontmatter if it had none.
pub fn rewrite(text: &str, keys: &[(&str, Option<String>)]) -> String {
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let parts = split(text);
    let yaml = set_keys(parts.yaml.unwrap_or(""), keys);
    format!("---{nl}{yaml}---{nl}{}", parts.body)
}

/// Quotes a string as a YAML scalar.
pub fn quote(s: &str) -> String {
    serde_json::to_string(s).expect("a string serialises")
}

#[cfg(test)]
mod tests {
    use super::*;

    // Spec (design: Entry metadata): retire changes only the keys it sets; every other line of the
    // frontmatter stays as written, comments included.
    #[test]
    fn set_keys_replaces_named_keys_and_keeps_the_rest() {
        let yaml = "id: rule.a\n# why\nstatus: active\nretired:\n  date: x\n  reason: y\ntopics:\n- a\ntitle: T\n";
        let got = set_keys(
            yaml,
            &[
                ("status", Some("retired".into())),
                ("retired", Some("date: 2026-10-25\nreason: \"r\"".into())),
            ],
        );
        assert_eq!(
            got,
            "id: rule.a\n# why\ntopics:\n- a\ntitle: T\nstatus: retired\nretired:\n  date: 2026-10-25\n  reason: \"r\"\n"
        );
    }

    // Spec: the body is everything after the closing `---`, and its first line number is reported.
    #[test]
    fn split_finds_body_and_its_line() {
        let s = split("---\r\nid: a\r\n---\r\nhello\r\n");
        assert_eq!(s.yaml, Some("id: a\r\n"));
        assert_eq!(s.body, "hello\r\n");
        assert_eq!(s.body_line, 4);
        assert!(split("no frontmatter\n---\n").yaml.is_none());
    }
}
