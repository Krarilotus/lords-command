//! An entry composes the same whichever line endings its file holds (the orchestrator, 8 Nov 16:43: "an entry's text
//! composes the same whichever line endings it was checked out with"; gap 11 design check r5, R5-F1). Git's own line
//! ending conversion (`text`, `eol`, `core.autocrlf`) gives a checkout CRLF or LF from one blob, so the starts and place
//! files Lords composes from it must not depend on which.

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;
use common::*;
use lords_core::compose::{compose, Context, Layers};
use lords_core::model::Audience;

fn global() -> Context {
    Context {
        audience: vec![Audience::Opus],
        layers: Layers::Global,
        public_only: false,
        skip_stores: vec![],
        roles: None,
        by_reference: false,
        role_dir: None,
        relative_to: None,
        default_role: None,
        default_role_inline: false,
        opener: None,
        child: None,
        default_role_lines: true,
        memory_file: None,
    }
}

/// The composed text of a fixture holding `files` (path, text), each written with CRLF line endings if `crlf`.
fn composed(files: &[(&str, String)], crlf: bool) -> String {
    let f = Fixture::new();
    // Roles declared, so an entry's core lines have readers (with none, a `start: core` entry composes nothing).
    f.write(
        "pub/stack.toml",
        "name = \"pub\"\nvisibility = \"public\"\nroles = [\"coder\", \"reviewer\"]\n",
    );
    for (rel, text) in files {
        let text = if crlf { text.replace('\n', "\r\n") } else { text.clone() };
        f.write(rel, &text);
    }
    compose(&f.stacks(), &global()).text
}

// Spec (R5-F1's witness, executed by the r5 reviewer with the installed binary: 317 characters from LF, 318 from a
// CRLF checkout of the same blob): a whole-body rule whose body starts with a blank line composes to the same text
// from an LF file and from a CRLF one. The blank line between the frontmatter and the text is the case: the composer
// trims leading and trailing newlines from a body before it splits it into lines.
#[test]
fn a_whole_body_with_a_leading_blank_line_composes_the_same_from_crlf() {
    let files = [(
        "pub/global/rules/a.md",
        entry("rule.a", "global", "public", "", "\nAlpha.\n\nBeta.\n\n"),
    )];
    let (lf, crlf) = (composed(&files, false), composed(&files, true));
    assert!(lf.contains("Alpha.") && lf.contains("Beta."), "{lf}");
    assert_eq!(crlf, lf);
}

// Spec (control, same source): an entry composed by its core line, which comes from the frontmatter, composes the
// same from LF and CRLF files. It held before the fix, so it is a guard, not a fails-without witness.
#[test]
fn a_core_line_composes_the_same_from_crlf() {
    let files = [(
        "pub/global/rules/b.md",
        entry(
            "rule.b",
            "global",
            "public",
            "start: core\ncore: Keep one writer per file.\n",
            "\nThe whole text.\n",
        ),
    )];
    let (lf, crlf) = (composed(&files, false), composed(&files, true));
    assert!(lf.contains("Keep one writer per file."), "{lf}");
    assert_eq!(crlf, lf);
}
