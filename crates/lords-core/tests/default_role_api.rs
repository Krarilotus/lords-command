//! The size guard's message (orchestrator, 25 Oct: "the guard message names the adapter and sections"). It needs
//! a new field (`Size::adapter`) and call (`Size::describe`), so this file doesn't build on its parent (its red
//! run); default_role.rs has the others.

use lords_core::sync::Size;

// Spec (above): a file past the line is described by its place, its adapter, its file, its total against the
// limit, and each section's bytes.
#[test]
fn the_size_guard_names_the_adapter_and_the_sections() {
    let s = Size {
        place: "repo fleet-scheduler".into(),
        at: None,
        adapter: "codex".into(),
        file: "AGENTS.override.md".into(),
        bytes: 20_000,
        reads: 24_963,
        limit: Some(32_768),
        sections: vec![("Rules".into(), 13_571), ("Memory".into(), 3_511)],
        enabled: true,
    };
    let d = s.describe();
    for part in [
        "repo fleet-scheduler",
        "codex",
        "AGENTS.override.md",
        "24963",
        "32768",
        "Rules 13571",
        "Memory 3511",
    ] {
        assert!(d.contains(part), "{part} in: {d}");
    }
}
