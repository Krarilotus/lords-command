//! A unified line diff. Instruction files are small, so plain LCS is enough.

pub fn unified(old: &str, new: &str, old_name: &str, new_name: &str, context: usize) -> String {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    if a == b {
        return String::new();
    }
    // lcs[i][j] = LCS length of a[i..] and b[j..].
    let mut lcs = vec![vec![0u32; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] =
                if a[i] == b[j] { lcs[i + 1][j + 1] + 1 } else { lcs[i + 1][j].max(lcs[i][j + 1]) };
        }
    }
    // Each op: (' ' | '-' | '+', old index, new index).
    let mut ops: Vec<(char, usize, usize)> = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            ops.push((' ', i, j));
            i += 1;
            j += 1;
        } else if i < a.len() && (j == b.len() || lcs[i + 1][j] >= lcs[i][j + 1]) {
            // Deletions before insertions, as git prints them.
            ops.push(('-', i, j));
            i += 1;
        } else {
            ops.push(('+', i, j));
            j += 1;
        }
    }
    let mut out = format!("--- {old_name}\n+++ {new_name}\n");
    let changed: Vec<usize> =
        ops.iter().enumerate().filter(|(_, o)| o.0 != ' ').map(|(k, _)| k).collect();
    let mut k = 0;
    while k < changed.len() {
        let start = changed[k].saturating_sub(context);
        let mut end = changed[k];
        while k < changed.len() && changed[k] <= end + 2 * context + 1 {
            end = changed[k];
            k += 1;
        }
        let end = (end + context + 1).min(ops.len());
        let hunk = &ops[start..end];
        let old_start = hunk.iter().find(|o| o.0 != '+').map(|o| o.1 + 1).unwrap_or(hunk[0].1);
        let new_start = hunk.iter().find(|o| o.0 != '-').map(|o| o.2 + 1).unwrap_or(hunk[0].2);
        let old_len = hunk.iter().filter(|o| o.0 != '+').count();
        let new_len = hunk.iter().filter(|o| o.0 != '-').count();
        out.push_str(&format!("@@ -{old_start},{old_len} +{new_start},{new_len} @@\n"));
        for (c, oi, nj) in hunk {
            let line = if *c == '+' { b[*nj] } else { a[*oi] };
            out.push(*c);
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::unified;

    // Spec (design: Sync, "shows the diff per target"): the diff is a standard unified diff, so it reads like
    // `git diff`; equal texts give none.
    #[test]
    fn unified_diff_marks_changes_with_context() {
        let old = "a\nb\nc\nd\n";
        let new = "a\nB\nc\nd\ne\n";
        assert_eq!(
            unified(old, new, "old", "new", 1),
            "--- old\n+++ new\n@@ -1,4 +1,5 @@\n a\n-b\n+B\n c\n d\n+e\n"
        );
        assert_eq!(unified(old, old, "old", "new", 3), "");
        assert_eq!(unified("", "x\n", "old", "new", 3), "--- old\n+++ new\n@@ -0,0 +1,1 @@\n+x\n");
    }
}
