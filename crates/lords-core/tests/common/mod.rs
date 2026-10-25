//! Fixtures: a public and a private stack in a temp dir. Never the real home dir: git runs with an empty
//! global config, so no user hooks or settings apply.
#![allow(dead_code)]

use lords_core::privacy::Rules;
use lords_core::{Finding, Severity, Stacks};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Once;
use tempfile::TempDir;

pub fn isolate_git() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let cfg = std::env::temp_dir().join(format!("lords-test-gitconfig-{}", std::process::id()));
        // Built at run time, so the repo's own privacy check doesn't see an address in this source.
        let email = ["test", "example.invalid"].join("@");
        fs::write(
            &cfg,
            format!("[user]\n\tname = Test\n\temail = {email}\n[init]\n\tdefaultBranch = main\n"),
        )
        .unwrap();
        std::env::set_var("GIT_CONFIG_GLOBAL", &cfg);
        std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
    });
}

pub struct Fixture {
    pub dir: TempDir,
}

impl Fixture {
    pub fn new() -> Fixture {
        isolate_git();
        let f = Fixture { dir: TempDir::new().unwrap() };
        f.write("pub/stack.toml", "name = \"pub\"\nvisibility = \"public\"\ntopics = [\"delegation\", \"timing\", \"t\"]\n[projects.alpha]\n[repos.alpha-core]\nproject = \"alpha\"\n");
        f.write("priv/stack.toml", "name = \"priv\"\nvisibility = \"private\"\ntopics = [\"x\"]\n[projects.beta]\n[repos.beta-core]\nproject = \"beta\"\n");
        f
    }

    pub fn pub_root(&self) -> PathBuf {
        self.dir.path().join("pub")
    }

    pub fn priv_root(&self) -> PathBuf {
        self.dir.path().join("priv")
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    pub fn write(&self, rel: &str, text: &str) -> PathBuf {
        let p = self.path(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, text).unwrap();
        p
    }

    pub fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.path(rel)).unwrap()
    }

    /// The tool's own dir for this fixture (its ledger, backups and lock): never the real one.
    pub fn home(&self) -> PathBuf {
        self.path("lords-home")
    }

    /// Sync options for this fixture: every target, its own tool home.
    pub fn opts(&self) -> lords_core::sync::Options {
        lords_core::sync::Options { home: self.home(), ..lords_core::sync::Options::default() }
    }

    pub fn stacks(&self) -> Stacks {
        Stacks::load(&self.pub_root(), Some(&self.priv_root())).unwrap()
    }

    pub fn check(&self) -> Vec<Finding> {
        lords_core::check::check(&self.stacks(), &Rules::public())
    }

    pub fn git_init(&self, rel: &str) {
        git(&self.path(rel), &["init", "-q"]);
        git(&self.path(rel), &["add", "-A"]);
        git(&self.path(rel), &["commit", "-q", "-m", "fixture"]);
    }
}

pub fn git(dir: &Path, args: &[&str]) -> String {
    isolate_git();
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A Markdown entry. `extra` is more YAML (one key per line).
pub fn entry(id: &str, scope: &str, visibility: &str, extra: &str, body: &str) -> String {
    let kind = id.split('.').next().unwrap();
    format!(
        "---\nid: {id}\nkind: {kind}\ntitle: {id}\naudience: [all]\nscope: {scope}\nstatus: active\nvisibility: {visibility}\norigin:\n  type: own\n{extra}---\n{body}"
    )
}

/// Private-looking samples, assembled at run time for the same reason.
pub fn user_path(name: &str) -> String {
    format!("C:\\{}\\{name}\\notes", "Users")
}

pub fn email(user: &str) -> String {
    [user, "example.org"].join("@")
}

pub fn codes(findings: &[Finding], severity: Severity) -> Vec<&'static str> {
    let mut v: Vec<_> =
        findings.iter().filter(|f| f.severity == severity).map(|f| f.code).collect();
    v.sort();
    v
}

/// A file's planned text inside a tool-owned dir's item (a place's `.lords/` role files).
pub fn planned_in(p: &lords_core::sync::Plan, dir: &Path, file: &str) -> String {
    p.items
        .iter()
        .find(|i| i.path == dir)
        .and_then(|i| i.tree.as_ref())
        .and_then(|t| t.get(file))
        .map(|b| String::from_utf8_lossy(b).into_owned())
        .unwrap_or_default()
}

pub fn errors(findings: &[Finding]) -> Vec<&'static str> {
    codes(findings, Severity::Error)
}

pub fn has(findings: &[Finding], code: &str, entry: &str) -> bool {
    findings.iter().any(|f| f.code == code && f.entry.as_deref() == Some(entry))
}
