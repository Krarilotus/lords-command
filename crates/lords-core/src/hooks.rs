//! `hooks install`: the privacy check as a repo's pre-commit hook, in `.git/hooks` (design: Privacy).
//! Never through `core.hooksPath`: a global hooks path whose dispatcher runs the repo's own hooks keeps working.

use crate::git;
use std::fs;
use std::path::{Path, PathBuf};

pub const MARKER: &str = "# lords-command: privacy check";

pub struct Installed {
    pub hook: PathBuf,
    pub backup: Option<PathBuf>,
    /// The global `core.hooksPath`, if any: its dispatcher must run `.git/hooks/pre-commit`.
    pub global_hooks_path: Option<String>,
}

pub fn script(lords_exe: &Path, secrets_only: bool) -> String {
    let exe = lords_exe.to_string_lossy().replace('\\', "/");
    let flag = if secrets_only { " --secrets-only" } else { "" };
    format!("#!/bin/sh\n{MARKER} of the staged files (installed by `lords hooks install`).\nexec \"{exe}\" privacy --staged{flag}\n")
}

pub fn install(
    repo: &Path,
    lords_exe: &Path,
    secrets_only: bool,
    force: bool,
) -> Result<Installed, String> {
    if let Ok(local) = git::run(repo, &["config", "--local", "--get", "core.hooksPath"]) {
        return Err(format!(
            "this repo sets core.hooksPath = {} locally, so .git/hooks would not run; unset it or point it at a dispatcher first",
            local.trim()
        ));
    }
    let common = git::run(repo, &["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
    let dir = PathBuf::from(common.trim()).join("hooks");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let hook = dir.join("pre-commit");
    let mut backup = None;
    if let Ok(existing) = fs::read_to_string(&hook) {
        if !existing.contains(MARKER) {
            if !force {
                return Err(format!(
                    "{} exists and isn't ours; --force backs it up and replaces it",
                    hook.display()
                ));
            }
            let b = dir.join(format!("pre-commit.bak-{}", crate::ops::today()));
            fs::rename(&hook, &b).map_err(|e| e.to_string())?;
            backup = Some(b);
        }
    }
    fs::write(&hook, script(lords_exe, secrets_only)).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    let global_hooks_path = git::run(repo, &["config", "--global", "--get", "core.hooksPath"])
        .ok()
        .map(|s| s.trim().to_string());
    Ok(Installed { hook, backup, global_hooks_path })
}
