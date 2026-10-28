//! Which project a working directory belongs to, and the registry of project stores (v2 design §2.1, identity
//! algorithm). One owner, shared by the hook, the CLI and the MCP server.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The registry's file in the registry home.
pub const REGISTRY: &str = "stores.toml";

/// `<home>/stores.toml`: where the stores are and the bindings their owners approved, never their content.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub core: Option<Core>,
    #[serde(default)]
    pub stores: Vec<StoreReg>,
    #[serde(default)]
    pub repos: Vec<RepoBinding>,
    #[serde(default)]
    pub workspaces: Vec<WorkspaceBinding>,
}

/// The core: the public stack and the private overlay, of which only `global/` is read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Core {
    pub public: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub private: Option<PathBuf>,
}

/// A project's store: its dir, canonical.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreReg {
    pub project: String,
    pub path: PathBuf,
}

/// A checkout of a repo the project's store declares: its canonical absolute git common dir and checkout, and the
/// origin remote without credentials, when it has one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoBinding {
    pub project: String,
    pub name: String,
    pub common_dir: PathBuf,
    pub checkout: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
}

/// A non-git workspace the project's store declares: its canonical path and the non-git dirs below it (relative,
/// `/`-separated) that inherit it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceBinding {
    pub project: String,
    pub name: String,
    pub path: PathBuf,
    #[serde(default)]
    pub descendants: Vec<String>,
}

/// What `register_store` recorded for the store's project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Registration {
    pub project: String,
    pub store: PathBuf,
    pub repos: Vec<RepoBinding>,
    pub workspaces: Vec<WorkspaceBinding>,
    /// Whether the registry file changed (`false`: the same registration was there already).
    pub changed: bool,
}

/// Where a working dir sits in its project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    Repo { name: String, toplevel: PathBuf },
    Workspace { name: String },
}

/// What a working dir is bound to: one project, or the core alone (with why).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Binding {
    Project { project: String, store: PathBuf, place: Place, cwd: PathBuf },
    CoreOnly { cwd: Option<PathBuf>, reason: String },
}

/// The registry home: `$LORDS_HOME`, else `~/.lords`.
pub fn default_home() -> Option<PathBuf> {
    None
}

impl Registry {
    pub fn load(_home: &Path) -> Result<Registry, String> {
        Err("not yet".into())
    }
}

pub fn set_core(_home: &Path, _public: &Path, _private: Option<&Path>) -> Result<(), String> {
    Err("not yet".into())
}

pub fn register_store(_home: &Path, _store_dir: &Path) -> Result<Registration, String> {
    Err("not yet".into())
}

pub fn resolve(_reg: &Registry, _cwd: &Path, _hint: Option<&str>) -> Binding {
    Binding::CoreOnly { cwd: None, reason: "not yet".into() }
}
