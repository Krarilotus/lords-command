//! The memory pointer (the Lords owner, 2 Nov: "Codex place files carry the memory index as a single pointer line, not
//! the full list"; design note Harness/notes/2026-11-02-lords-memory-pointer.md). With `[place_files.<adapter>] memory =
//! "pointer"`, that adapter's place file shows its Memory section as one line naming `.lords/memory-<adapter>.md`, which
//! holds the section as the place file had it. Without the setting nothing changes.

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;
use common::*;
use lords_core::compose::END;
use lords_core::ledger::{self, Ledger};
use lords_core::sync::{apply, plan, Action, Options, Plan};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const CODEX: &str = "[place_files.codex]\nmemory = \"pointer\"\n";

/// A note for every role, one for the default role (the orchestrator) and one for the coder.
const NOTES: [(&str, &str); 3] =
    [("all", ""), ("lead", "roles: [orchestrator]\n"), ("dev", "roles: [coder]\n")];

/// A repo `beta-core` whose default role is the orchestrator, with both adapters, one rule and the notes `notes`
/// (slug, more YAML).
fn place(notes: &[(&str, &str)]) -> (Fixture, PathBuf) {
    let f = Fixture::new();
    let checkout = f.path("checkout");
    f.write("checkout/README.md", "a repo\n");
    f.git_init("checkout");
    f.write(
        "priv/stack.toml",
        &format!(
            "name = \"priv\"\nvisibility = \"private\"\nroles = [\"orchestrator\", \"coder\", \"helper\", \"product\"]\n[projects.beta]\n[repos.beta-core]\nproject = \"beta\"\ncheckouts = ['{}']\nworktrees = false\ndefault_role = \"orchestrator\"\n",
            checkout.display()
        ),
    );
    f.write(
        "priv/project/beta/rules/r.md",
        &entry("rule.r", "project:beta", "private", "start: core\ncore: A rule's line.\n", "R\n"),
    );
    for (slug, yaml) in notes {
        f.write(
            &format!("priv/project/beta/memory/{slug}.md"),
            &entry(&format!("memory.{slug}"), "project:beta", "private", yaml, "A note.\n"),
        );
    }
    (f, checkout)
}

fn proposed(p: &Plan, path: &Path) -> String {
    p.items.iter().find(|i| i.path == path).and_then(|i| i.proposed.clone()).unwrap_or_default()
}

/// The `.lords` tree a plan writes in `checkout`.
fn tree(p: &Plan, checkout: &Path) -> BTreeMap<String, Vec<u8>> {
    let dir = checkout.join(".lords");
    p.items
        .iter()
        .find(|i| i.path == dir)
        .and_then(|i| i.tree.as_deref().cloned())
        .unwrap_or_default()
}

/// A composed file's Memory section, the last one: from its heading to the end marker; empty without one.
fn memory_section(text: &str) -> &str {
    let end = text.rfind(END).unwrap_or(text.len());
    let start = text.find("\n# Memory\n").map_or(end, |i| i + 1);
    &text[start.min(end)..end]
}

/// What a composed file holds before its Memory section.
fn before_memory(text: &str) -> &str {
    &text[..text.find("\n# Memory\n").map_or(text.len(), |i| i + 1)]
}

/// The managed block whose body is `section`, with the header lines of the composed file `like`.
fn block_of(section: &str, like: &str) -> String {
    let head: Vec<&str> = like.lines().take(2).collect();
    format!("{}\n{}\n{section}{END}\n", head[0], head[1])
}

// Spec (the brief: "With it, that adapter's place file shows the Memory section as one line naming a file that holds the
// full list"; "sync writes it as a new place file … with the same content the section had"; "no change to Claude place
// files unless the setting names the claude adapter, and no change to role files"; design note §2 and A6): with the
// setting on Codex, Codex's place file keeps everything before its Memory section and shows the section as its heading
// and one line naming `.lords/memory-codex.md`; that file holds the section the place file had, in a managed block;
// Claude's file and every other `.lords` file stay as they were, and `"list"` is the default spelled out. With both
// adapters on, each gets its own file holding its own section (Claude's carries the default role's note inline).
#[test]
fn a_pointer_names_a_file_holding_the_section_it_stands_for() {
    let (f, checkout) = place(&NOTES);
    let manifest = f.read("priv/stack.toml");
    let before = plan(&f.stacks(), &f.opts()).unwrap();
    let claude0 = proposed(&before, &checkout.join("CLAUDE.local.md"));
    let codex0 = proposed(&before, &checkout.join("AGENTS.override.md"));
    // A place's Codex file leaves every note for only some roles to the role files (counted there).
    let section0 = memory_section(&codex0);
    assert!(section0.contains("- all") && !section0.contains("- lead"), "{codex0}");

    f.write("priv/stack.toml", &format!("{manifest}{CODEX}"));
    let after = plan(&f.stacks(), &f.opts()).unwrap();
    let codex = proposed(&after, &checkout.join("AGENTS.override.md"));
    let file = checkout.join(".lords").join("memory-codex.md");
    let section: Vec<&str> = memory_section(&codex).lines().collect();
    assert!(
        section.len() == 3
            && section[..2] == ["# Memory", ""]
            && section[2].contains(&format!("`{}`", file.display())),
        "one line naming the file: {codex}"
    );
    assert_eq!(before_memory(&codex), before_memory(&codex0), "the rest is as before");
    let held = tree(&after, &checkout);
    assert_eq!(
        String::from_utf8_lossy(&held["memory-codex.md"]),
        block_of(section0, &codex0),
        "the file holds the section the place file had"
    );
    assert_eq!(proposed(&after, &checkout.join("CLAUDE.local.md")), claude0, "Claude's file");
    let mut rest = held.clone();
    rest.remove("memory-codex.md");
    assert_eq!(rest, tree(&before, &checkout), "every other .lords file is as before");

    f.write("priv/stack.toml", &format!("{manifest}[place_files.codex]\nmemory = \"list\"\n"));
    let listed = plan(&f.stacks(), &f.opts()).unwrap();
    assert_eq!(
        proposed(&listed, &checkout.join("AGENTS.override.md")),
        codex0,
        "list: the default"
    );
    assert_eq!(tree(&listed, &checkout), tree(&before, &checkout));

    f.write(
        "priv/stack.toml",
        &format!("{manifest}{CODEX}[place_files.claude]\nmemory = \"pointer\"\n"),
    );
    let both = plan(&f.stacks(), &f.opts()).unwrap();
    let held = tree(&both, &checkout);
    let claude_file = String::from_utf8_lossy(&held["memory-claude.md"]).into_owned();
    assert_eq!(claude_file, block_of(memory_section(&claude0), &claude0), "Claude's own section");
    assert_eq!(String::from_utf8_lossy(&held["memory-codex.md"]), block_of(section0, &codex0));
    assert!(claude_file.contains("- lead") && !section0.contains("- lead"), "{claude_file}");
}

// Spec (the brief's attack list: "an empty memory list; a place with no memory"; design note I3, A1, A2): a place
// whose file would carry no Memory section (no note, or only notes for some roles, which the role files carry) gets no
// pointer and no file, its place file as without the setting; one whose section is only a live store's line gets the
// pointer and the file, holding that line.
#[test]
fn no_memory_gives_no_pointer_and_no_file() {
    for notes in [&[][..], &[("dev", "roles: [coder]\n")][..]] {
        let (f, checkout) = place(notes);
        let manifest = f.read("priv/stack.toml");
        let codex0 =
            proposed(&plan(&f.stacks(), &f.opts()).unwrap(), &checkout.join("AGENTS.override.md"));
        assert!(!codex0.is_empty() && !codex0.contains("# Memory"), "{codex0}");
        f.write("priv/stack.toml", &format!("{manifest}{CODEX}"));
        let p = plan(&f.stacks(), &f.opts()).unwrap();
        assert_eq!(proposed(&p, &checkout.join("AGENTS.override.md")), codex0);
        assert!(!tree(&p, &checkout).contains_key("memory-codex.md"), "no file");
    }

    let (f, checkout) = place(&[]);
    f.write("store/MEMORY.md", "- a note\n");
    let manifest = format!(
        "{}[[memory_stores]]\nname = \"live\"\npath = '{}'\nscope = \"project:beta\"\nlive = true\n",
        f.read("priv/stack.toml"),
        f.path("store").display()
    );
    f.write("priv/stack.toml", &manifest);
    let codex0 =
        proposed(&plan(&f.stacks(), &f.opts()).unwrap(), &checkout.join("AGENTS.override.md"));
    let section0 = memory_section(&codex0);
    assert!(section0.contains("- live: its index is") && !section0.contains("In `"), "{codex0}");
    f.write("priv/stack.toml", &format!("{manifest}{CODEX}"));
    let p = plan(&f.stacks(), &f.opts()).unwrap();
    let file = checkout.join(".lords").join("memory-codex.md");
    assert!(proposed(&p, &checkout.join("AGENTS.override.md"))
        .contains(&format!("`{}`", file.display())));
    assert_eq!(
        String::from_utf8_lossy(&tree(&p, &checkout)["memory-codex.md"]),
        block_of(section0, &codex0)
    );
}

// Spec (the brief: "It's recorded in the ledger like the other `.lords` files and covered by `uninstall --place`"; its
// attack list: "uninstall; a re-sync after the setting is turned off"; design note I4, A3, A4): an applied sync writes
// the file and the pointer to it; a sync after the setting is turned off removes the file and puts the list back in the
// place file; `uninstall --place` removes the file with `.lords`, and no record of it stays.
#[test]
fn the_file_comes_and_goes_with_the_setting_and_the_place() {
    let (f, checkout) = place(&NOTES);
    let manifest = f.read("priv/stack.toml");
    let file = checkout.join(".lords").join("memory-codex.md");
    let codex = checkout.join("AGENTS.override.md");
    let sync = |manifest: &str| {
        f.write("priv/stack.toml", manifest);
        apply(&plan(&f.stacks(), &f.opts()).unwrap()).unwrap();
    };
    sync(&format!("{manifest}{CODEX}"));
    let held = fs::read_to_string(&file).unwrap();
    assert!(memory_section(&held).contains("- all"), "{held}");
    assert!(fs::read_to_string(&codex).unwrap().contains(&format!("`{}`", file.display())));

    sync(&manifest);
    assert!(!file.exists(), "turned off, the file goes");
    let listed = fs::read_to_string(&codex).unwrap();
    assert_eq!(memory_section(&listed), memory_section(&held), "and the list is back: {listed}");

    sync(&format!("{manifest}{CODEX}"));
    assert!(file.exists());
    let place = Options {
        repo: Some("beta-core".into()),
        place: Some(checkout.clone()),
        uninstall: true,
        ..f.opts()
    };
    apply(&plan(&f.stacks(), &place).unwrap()).unwrap();
    assert!(!file.exists() && !checkout.join(".lords").exists(), "gone with .lords");
    let records = Ledger::load(&f.home()).unwrap().owned;
    let under = ledger::key(&checkout.join(".lords"));
    assert!(!records.keys().any(|k| k.starts_with(&under)), "{:?}", records.keys());
}

// Spec (the Lords owner, 2 Nov, accepting review r1 R1-01: "`uninstall --place` never removes a Lords file that a file
// it keeps still names. If an instruction file is kept (changed inside the Lords block, so exit 1 and named in
// `remaining`), then the `.lords` files that kept file points to stay too, and are listed in `remaining`. That covers
// `memory-<adapter>.md` and the role-files line's `role-*.md`"; design note §6b, RA1, RA2, RA4, RA8): a place file
// edited inside its block is kept, whether or not the repo now tracks it, and `.lords` stays with it, with its record,
// so what the kept file names is still there; the unchanged place file goes; exactly those two remain. Once the kept
// file is gone, `.lords` goes too, and nothing of the place remains.
#[test]
fn a_kept_place_file_keeps_the_lords_files_it_names() {
    // RA1: Codex's pointer; RA2: Claude's role-files line, without the setting; RA8 (sol-loop repair r1, U1): the
    // pointer in a file the repo now tracks.
    for (setting, name, other, named, tracked) in [
        (CODEX, "AGENTS.override.md", "CLAUDE.local.md", "memory-codex.md", false),
        ("", "CLAUDE.local.md", "AGENTS.override.md", "role-coder.md", false),
        (CODEX, "AGENTS.override.md", "CLAUDE.local.md", "memory-codex.md", true),
    ] {
        let (f, checkout) = place(&NOTES);
        let manifest = f.read("priv/stack.toml");
        f.write("priv/stack.toml", &format!("{manifest}{setting}"));
        apply(&plan(&f.stacks(), &f.opts()).unwrap()).unwrap();
        let (lords, kept, other) =
            (checkout.join(".lords"), checkout.join(name), checkout.join(other));
        let text = fs::read_to_string(&kept).unwrap();
        assert!(lords.join(named).exists() && text.contains(&format!("{named}`")), "{text}");
        // A line after the begin marker: inside the block, so neither a whole-file nor a block-only removal.
        fs::write(&kept, text.replacen('\n', "\nmine, inside the block\n", 1)).unwrap();
        if tracked {
            git(&checkout, &["add", "-f", name]);
        }
        let place = Options {
            repo: Some("beta-core".into()),
            place: Some(checkout.clone()),
            uninstall: true,
            ..f.opts()
        };
        let action = |p: &Plan, path: &Path| {
            p.items
                .iter()
                .find(|i| ledger::key(&i.path) == ledger::key(path))
                .map(|i| i.action.clone())
        };
        let p = plan(&f.stacks(), &place).unwrap();
        assert_eq!(action(&p, &kept), Some(Action::Report), "{named}: {:?}", p.items);
        assert_eq!(action(&p, &lords), Some(Action::Report), "{named}: {:?}", p.items);
        assert_eq!(action(&p, &other), Some(Action::Remove), "{named}: {:?}", p.items);
        apply(&p).unwrap();
        assert!(lords.join(named).exists() && !other.exists(), "{named}");
        let left = plan(&f.stacks(), &place).unwrap();
        let remaining: BTreeSet<String> = left.items.iter().map(|i| ledger::key(&i.path)).collect();
        assert_eq!(remaining, BTreeSet::from([ledger::key(&kept), ledger::key(&lords)]), "{named}");
        assert!(left.items.iter().all(|i| i.action == Action::Report), "{:?}", left.items);
        // RA4: the hold lets go with the kept file.
        if tracked {
            git(&checkout, &["rm", "-q", "-f", name]);
        } else {
            fs::remove_file(&kept).unwrap();
        }
        apply(&plan(&f.stacks(), &place).unwrap()).unwrap();
        assert!(!lords.exists(), "{named}");
        assert!(plan(&f.stacks(), &place).unwrap().items.is_empty(), "{named}: nothing remains");
    }
}
