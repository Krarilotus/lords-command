//! `config.txt`: `key = value` lines, `#` comments (overseer's format). Read from beside the exe, else from
//! `~/.lords-command/config.txt`.

use crate::fsguard;
use std::collections::BTreeMap;
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
    /// The config in `path`, None if there is none. One that is there but can't be read or decoded is an error:
    /// read as none, it would silently drop the private stack and with it the deny and fleet checks.
    pub fn read(path: &Path) -> Result<Option<Config>, String> {
        let text = match fsguard::read_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("{} can't be read: {e}", path.display())),
        };
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
        Ok(Some(Config {
            public: resolve("public"),
            private: resolve("private"),
            source: Some(path.to_path_buf()),
        }))
    }

    /// The config beside the running exe, else the one in the home dir, else empty. The first one that is there
    /// but can't be read is an error.
    pub fn load() -> Result<Config, String> {
        let beside =
            std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("config.txt")));
        let in_home = home().map(|h| h.join(".lords-command").join("config.txt"));
        for path in beside.into_iter().chain(in_home) {
            if let Some(cfg) = Config::read(&path)? {
                return Ok(cfg);
            }
        }
        Ok(Config::default())
    }

    /// As `load`, with a config that can't be read taken as none: only for views that show stacks. Checks use
    /// `load`.
    pub fn discover() -> Config {
        Config::load().unwrap_or_default()
    }
}
