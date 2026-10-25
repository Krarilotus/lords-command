//! `config.txt`: `key = value` lines, `#` comments (overseer's format). Read from beside the exe, else from
//! `~/.lords-command/config.txt`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default)]
pub struct Config {
    /// The public stack root.
    pub public: Option<PathBuf>,
    /// The private stack root.
    pub private: Option<PathBuf>,
    /// Where it was read from.
    pub source: Option<PathBuf>,
}

pub fn home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from)
}

/// Expands a leading `~` to the home dir.
pub fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix('~'), home()) {
        (Some(rest), Some(h)) => h.join(rest.trim_start_matches(['/', '\\'])),
        _ => PathBuf::from(path),
    }
}

pub fn parse(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

impl Config {
    pub fn read(path: &Path) -> Option<Config> {
        let text = fs::read_to_string(path).ok()?;
        let keys = parse(&text);
        let dir = path.parent().unwrap_or(Path::new("."));
        let resolve = |k: &str| {
            keys.get(k).map(|v| {
                let p = expand_home(v);
                if p.is_absolute() {
                    p
                } else {
                    dir.join(p)
                }
            })
        };
        Some(Config {
            public: resolve("public"),
            private: resolve("private"),
            source: Some(path.to_path_buf()),
        })
    }

    /// The config beside the running exe, else the one in the home dir, else empty.
    pub fn discover() -> Config {
        let beside =
            std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("config.txt")));
        let in_home = home().map(|h| h.join(".lords-command").join("config.txt"));
        beside.into_iter().chain(in_home).find_map(|p| Config::read(&p)).unwrap_or_default()
    }
}
