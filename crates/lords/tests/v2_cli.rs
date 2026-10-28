//! Spec: design §5 1a "CLI/MCP give identical ids/bodies"; §5 "CLI fallback retained": `lords brief`, `context`,
//! `search` and `remember` print exactly what the library's `serve` returns (crates/lords-mcp/tests/same_as_cli.rs
//! shows the same for the server).

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

use lords_core::serve;
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::process::Command;

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

fn lords(home: &Path, args: &[&str]) -> (String, String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_lords"))
        .args(args)
        .arg("--registry")
        .arg(home)
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

#[test]
fn the_cli_prints_what_the_library_returns() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    write(root, "core/stack.toml", "name = \"pub\"\nvisibility = \"public\"\n");
    write(
        root,
        "core/global/rules/core.md",
        "---\nid: rule.core\nkind: rule\ntitle: core\naudience: [all]\nscope: global\nstatus: active\n\
         visibility: public\norigin:\n  type: own\n---\nCore.\n",
    );
    fs::create_dir_all(root.join("ws")).unwrap();
    let ws = root.join("ws").display().to_string().replace('\\', "/");
    write(
        root,
        "store/stack.toml",
        &format!(
            "name = \"alpha-lords\"\nvisibility = \"private\"\nproject = \"alpha\"\nroles = [\"coder\"]\n\
             [projects.alpha]\n[workspaces.ws]\nproject = \"alpha\"\npath = '{ws}'\n"
        ),
    );
    write(
        root,
        "store/project/alpha/rules/duty.md",
        "---\nid: rule.duty\nkind: rule\ntitle: duty\naudience: [all]\nroles: [coder]\nscope: project:alpha\n\
         status: active\nvisibility: private\norigin:\n  type: own\n---\nDo the duty.\n",
    );
    let home = root.join("registry");
    let core = root.join("core").display().to_string();
    let store = root.join("store").display().to_string();
    assert!(lords(&home, &["register", "--core", "--public", &core]).2);
    let (out, err, ok) = lords(&home, &["register", "--store", &store]);
    assert!(ok, "{out}{err}");

    let (out, err, ok) = lords(&home, &["brief", "--json", "--role", "coder", "--cwd", &ws]);
    assert!(ok, "{err}");
    let brief: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(brief["project"], "alpha");
    let id = brief["delivery_id"].as_str().unwrap().to_string();

    let (out, err, ok) = lords(&home, &["context", "--json", "--role", "coder", "--delivery", &id]);
    assert!(ok, "{err}");
    let lib = serve::context(&home, &id, "coder", None, &lords_core::ops::today()).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&out).unwrap(), serde_json::to_value(&lib).unwrap());

    let (out, err, ok) = lords(&home, &["search", "duty", "--json", "--delivery", &id]);
    assert!(ok, "{err}");
    let lib = serve::search(&home, &id, "duty", &[], false, &lords_core::ops::today()).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&out).unwrap(), serde_json::to_value(&lib).unwrap());
    assert_eq!(lib[0].id, "rule.duty");

    let (out, err, ok) = lords(&home, &["check", "--store", &store]);
    assert!(ok, "{out}{err}");

    // The fallback a session uses when MCP is down refuses another role's context as the server does.
    let (_, err, ok) = lords(&home, &["context", "--role", "orchestrator", "--delivery", &id]);
    assert!(!ok && err.contains("issued for role coder"), "{err}");
}
