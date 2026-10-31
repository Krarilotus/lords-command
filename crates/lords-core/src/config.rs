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
    /// The panel's server, `http://127.0.0.1:<port>` (overseer's format): the window reads the stacks from it, on
    /// another PC, instead of from `public` and `private`.
    pub api: Option<String>,
    /// An SSH alias whose `LocalForward` reaches the server's port: `ssh -N <alias>` runs while the window is open.
    pub tunnel: Option<String>,
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

/// `api` takes only a loopback address: the panel's server has no authentication of its own, so it is reached from
/// its own PC or through an SSH forward (overseer's rule).
pub fn check_api(api: &str) -> Result<(), String> {
    crate::http::Client::from_api(api).map(|_| ())
}

/// `tunnel` is an SSH alias, never an option: ssh would read `-oProxyCommand=…` as one.
pub fn check_alias(alias: &str) -> Result<(), String> {
    let plain = |c: char| c.is_ascii_alphanumeric() || "._-".contains(c);
    if !alias.is_empty() && !alias.starts_with('-') && alias.chars().all(plain) {
        Ok(())
    } else {
        Err(format!("tunnel `{alias}` must be an SSH alias (letters, digits, `.`, `_`, `-`)"))
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
        let at = |e: String| format!("{}: {e}", path.display());
        let api =
            keys.get("api").map(|a| check_api(a).map(|()| a.clone())).transpose().map_err(at)?;
        let tunnel = keys
            .get("tunnel")
            .map(|t| check_alias(t).map(|()| t.clone()))
            .transpose()
            .map_err(at)?;
        Ok(Some(Config {
            public: resolve("public"),
            private: resolve("private"),
            api,
            tunnel,
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
