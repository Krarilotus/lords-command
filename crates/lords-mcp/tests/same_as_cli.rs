//! Spec: design §5 1a "CLI/MCP give identical ids/bodies". The server's tools return exactly what the library's
//! `serve` returns for the same delivery; crates/lords/tests/v2_cli.rs shows the same for the CLI, so the two agree.

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

use lords_core::serve::{self, Note, Start};
use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};

fn entry(id: &str, extra: &str, body: &str) -> String {
    let kind = id.split('.').next().unwrap();
    format!(
        "---\nid: {id}\nkind: {kind}\ntitle: {id}\naudience: [all]\nscope: project:alpha\nstatus: active\n\
         visibility: private\norigin:\n  type: own\n{extra}---\n{body}"
    )
}

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

/// A core and a project store bound to a plain (non-git) workspace dir.
fn world(root: &Path) -> std::path::PathBuf {
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
        &entry("rule.duty", "roles: [coder]\n", "Do the duty.\n"),
    );
    write(root, "store/project/alpha/facts/f.md", &entry("fact.f", "", "A fact.\n"));
    let home = root.join("registry");
    lords_core::identity::set_core(&home, &root.join("core"), None).unwrap();
    lords_core::identity::register_store(&home, &root.join("store")).unwrap();
    home
}

struct Server {
    child: std::process::Child,
    out: BufReader<std::process::ChildStdout>,
    next: u64,
}

impl Server {
    fn start(home: &Path) -> Server {
        let mut child = Command::new(env!("CARGO_BIN_EXE_lords-mcp"))
            .arg("--registry")
            .arg(home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let out = BufReader::new(child.stdout.take().unwrap());
        let mut s = Server { child, out, next: 1 };
        let init = s.call(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}),
        );
        assert!(init.get("result").is_some(), "{init}");
        s.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        s
    }

    fn send(&mut self, v: Value) {
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{v}").unwrap();
        stdin.flush().unwrap();
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let mut line = String::new();
            assert!(self.out.read_line(&mut line).unwrap() > 0, "the server closed its output");
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == json!(id) {
                return v;
            }
        }
    }

    /// A tool's text, and whether the server marked it an error.
    fn tool(&mut self, name: &str, args: Value) -> (String, bool) {
        let r = self.call("tools/call", json!({"name": name, "arguments": args}));
        let result = &r["result"];
        (
            result["content"][0]["text"].as_str().unwrap_or_default().to_string(),
            result["isError"] == json!(true),
        )
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn the_servers_tools_return_what_the_library_returns() {
    let dir = tempfile::TempDir::new().unwrap();
    let home = world(dir.path());
    let start = Start { cwd: dir.path().join("ws"), role: "coder".into(), ..Start::default() };
    let id = serve::brief(&home, &start).unwrap().delivery_id;
    let today = lords_core::ops::today();
    let mut s = Server::start(&home);

    let (text, err) = s.tool("context", json!({"role": "coder", "delivery_id": id}));
    assert!(!err, "{text}");
    let lib = serve::context(&home, &id, "coder", None, &today).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), serde_json::to_value(&lib).unwrap());
    assert_eq!(lib.duties[0].body.as_deref(), Some("Do the duty.\n"));

    let (text, err) = s.tool("search", json!({"delivery_id": id, "ids": ["fact.f"]}));
    assert!(!err, "{text}");
    let lib = serve::search(&home, &id, "", &["fact.f".into()], false, &today).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), serde_json::to_value(&lib).unwrap());

    let (text, err) =
        s.tool("remember", json!({"delivery_id": id, "text": "learnt", "title": "over mcp"}));
    assert!(!err, "{text}");
    let r: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(r["id"], "memory.over-mcp");
    let again = serve::remember(
        &home,
        &id,
        &Note { text: "x".into(), title: Some("over mcp".into()), ..Note::default() },
        &today,
    );
    assert_eq!(again.unwrap().id, "memory.over-mcp-2", "the library sees the server's note");

    // A refusal reaches the model as a tool error, not a crash or a success.
    let (text, err) = s.tool("context", json!({"role": "orchestrator", "delivery_id": id}));
    assert!(err && text.contains("issued for role coder"), "{text}");
}
