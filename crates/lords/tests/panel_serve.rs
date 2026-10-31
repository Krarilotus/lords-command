//! `lords panel serve`, the real binary end to end (design note 2026-10-31-lords-panel-remote, P1). The child gets an
//! empty home and global git config, so no real home dir, config.txt or user hook is read.

// Tests are outside the filesystem boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

use lords_core::http::Client;
use lords_core::panel::Source;
use std::fs;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

/// The child, stopped by its own handle however the test ends.
struct Serving(std::process::Child);

impl Drop for Serving {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

// Spec (P1: "`lords panel serve` on 127.0.0.1:7879 only. It serves `lords_core::view`'s serialisable views"): the
// binary serves the stacks it is given on 127.0.0.1 and nowhere else, says where, and the window's remote source reads
// their entries from it; a second server on a port that is taken says so and exits 2.
#[test]
fn panel_serve_serves_the_stacks_on_loopback() {
    let dir = tempfile::TempDir::new().unwrap();
    let write = |rel: &str, text: &str| {
        let p = dir.path().join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    };
    write("home/.keep", "");
    write("pub/stack.toml", "name = \"pub\"\nvisibility = \"public\"\n");
    write("priv/stack.toml", "name = \"priv\"\nvisibility = \"private\"\n");
    write(
        "priv/global/rules/v.md",
        "---\nid: rule.v\nkind: rule\ntitle: v\naudience: [all]\nscope: global\nstatus: active\nvisibility: private\norigin:\n  type: own\n---\nServed.\n",
    );
    let lords = |port: &str| {
        let mut c = Command::new(env!("CARGO_BIN_EXE_lords"));
        c.args(["panel", "serve", "--port", port, "--public-stack"])
            .arg(dir.path().join("pub"))
            .arg("--private-stack")
            .arg(dir.path().join("priv"))
            .env("GIT_CONFIG_GLOBAL", dir.path().join("gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", dir.path().join("home"))
            .env("USERPROFILE", dir.path().join("home"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        c
    };
    let mut serving = Serving(lords("0").spawn().unwrap());
    let mut first = String::new();
    BufReader::new(serving.0.stdout.take().unwrap()).read_line(&mut first).unwrap();
    let at = first.trim().rsplit(' ').next().unwrap().to_string();
    let addr: std::net::SocketAddr = at.parse().unwrap_or_else(|e| panic!("{first:?}: {e}"));
    assert!(addr.ip().is_loopback(), "{first}");
    // The token `serve` made in its home, as `lords panel add` copies it to another PC.
    let saved = fs::read_to_string(dir.path().join("home/.lords-command/panel-token")).unwrap();
    let token = lords_core::http::Token::new(&saved).unwrap();
    let stacks = Source::Remote(Client::new(addr.port(), token)).stacks().unwrap();
    assert_eq!(stacks.entries.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["rule.v"]);
    assert!(stacks.entries[0].body.ends_with("Served.\n"));

    let taken = lords(&addr.port().to_string()).output().unwrap();
    assert_eq!(taken.status.code(), Some(2), "{}", String::from_utf8_lossy(&taken.stderr));
    assert!(String::from_utf8_lossy(&taken.stderr).contains("does a panel serve there already"));
}
