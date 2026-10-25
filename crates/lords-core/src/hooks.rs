//! `hooks install`: the privacy check as a repo's pre-commit and commit-msg hooks, in `.git/hooks` (design:
//! Privacy).
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

/// The commit-msg hook: git passes the message file as `$1`.
pub fn commit_msg_script(lords_exe: &Path, secrets_only: bool) -> String {
    let exe = lords_exe.to_string_lossy().replace('\\', "/");
    let flag = if secrets_only { " --secrets-only" } else { "" };
    format!("#!/bin/sh\n{MARKER} of the commit message (installed by `lords hooks install`).\nexec \"{exe}\" privacy --message \"$1\"{flag}\n")
}

/// Installs the pre-commit and commit-msg hooks. A hook there that isn't ours stops both, unless `force`, which
/// backs each one up first.
pub fn install(
    repo: &Path,
    lords_exe: &Path,
    secrets_only: bool,
    force: bool,
) -> Result<Vec<Installed>, String> {
    if let Ok(local) = git::run(repo, &["config", "--local", "--get", "core.hooksPath"]) {
        return Err(format!(
            "this repo sets core.hooksPath = {} locally, so .git/hooks would not run; unset it or point it at a dispatcher first",
            local.trim()
        ));
    }
    let common = git::run(repo, &["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
    let dir = PathBuf::from(common.trim()).join("hooks");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let hooks = [
        ("pre-commit", script(lords_exe, secrets_only)),
        ("commit-msg", commit_msg_script(lords_exe, secrets_only)),
    ];
    let foreign = |name: &str| {
        fs::read_to_string(dir.join(name)).is_ok_and(|existing| !existing.contains(MARKER))
    };
    if !force {
        if let Some((name, _)) = hooks.iter().find(|(name, _)| foreign(name)) {
            return Err(format!(
                "{} exists and isn't ours; --force backs it up and replaces it",
                dir.join(name).display()
            ));
        }
    }
    let global_hooks_path = git::run(repo, &["config", "--global", "--get", "core.hooksPath"])
        .ok()
        .map(|s| s.trim().to_string());
    let mut installed = Vec::new();
    for (name, text) in hooks {
        let hook = dir.join(name);
        let mut backup = None;
        if foreign(name) {
            let b = dir.join(format!("{name}.bak-{}", crate::ops::today()));
            fs::rename(&hook, &b).map_err(|e| e.to_string())?;
            backup = Some(b);
        }
        fs::write(&hook, text).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&hook, fs::Permissions::from_mode(0o755))
                .map_err(|e| e.to_string())?;
        }
        installed.push(Installed { hook, backup, global_hooks_path: global_hooks_path.clone() });
    }
    Ok(installed)
}

// ---------------------------------------------------------------- post-checkout

pub const SYNC_MARKER: &str = "# lords-command: place files for a new worktree";

/// The name a hook that was here before ours keeps; ours runs it first, with the same arguments.
pub const CHAINED: &str = "post-checkout.before-lords";

/// What `lords sync` needs besides the repo and the worktree: the stacks, when they aren't in config.txt.
pub struct SyncArgs {
    pub repo: String,
    pub public_stack: Option<PathBuf>,
    pub private_stack: Option<PathBuf>,
}

fn quoted(p: &Path) -> String {
    format!("'{}'", p.to_string_lossy().replace('\\', "/").replace('\'', "'\\''"))
}

/// A post-checkout hook that writes a new worktree's place files (orchestrator, 25 Oct). It never fails or slows
/// the checkout: it acts only when the old HEAD is all zeros (`git worktree add`), runs sync in the background
/// under a time limit, and only logs (to `<git common dir>/lords-post-checkout.log`); a hook that was here before
/// runs first, and its exit status is the hook's.
pub fn post_checkout_script(lords_exe: &Path, args: &SyncArgs) -> String {
    let mut stacks = String::new();
    if let Some(p) = &args.public_stack {
        stacks.push_str(&format!(" --public-stack {}", quoted(p)));
    }
    if let Some(p) = &args.private_stack {
        stacks.push_str(&format!(" --private-stack {}", quoted(p)));
    }
    format!(
        r#"#!/bin/sh
{SYNC_MARKER} (installed by `lords hooks install --post-checkout`).
# Never fails or slows a checkout: only a new worktree (old HEAD all zeros), in the background, time-boxed, logged.
status=0
before="$(dirname "$0")/{CHAINED}"
if [ -f "$before" ]; then
  "$before" "$@"
  status=$?
fi
case "$1" in
  "" | *[!0]*) exit $status ;;
esac
exe={exe}
log="$(git rev-parse --git-common-dir 2>/dev/null || echo .)/lords-post-checkout.log"
place="$(pwd -W 2>/dev/null || pwd)"
if [ ! -f "$exe" ]; then
  echo "$(date '+%F %T') $place: lords not found at $exe; no place files written" >>"$log" 2>/dev/null
  exit $status
fi
limit=""
command -v timeout >/dev/null 2>&1 && limit="timeout 120"
(
  echo "$(date '+%F %T') $place: lords sync" >>"$log"
  $limit "$exe" sync --repo {repo} --place "$place" --apply{stacks} >>"$log" 2>&1 \
    || echo "$(date '+%F %T') $place: lords sync failed (exit $?)" >>"$log"
) </dev/null >/dev/null 2>&1 &
exit $status
"#,
        exe = quoted(lords_exe),
        repo = quoted(Path::new(&args.repo)),
    )
}

/// Installs the post-checkout hook in the repo's own hooks dir (the common dir, so every worktree has it). A hook
/// that isn't ours is kept as `post-checkout.before-lords` and chained; ours is replaced in place.
pub fn install_post_checkout(
    repo: &Path,
    lords_exe: &Path,
    args: &SyncArgs,
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
    let hook = dir.join("post-checkout");
    let mut backup = None;
    if let Ok(existing) = fs::read_to_string(&hook) {
        if !existing.contains(SYNC_MARKER) {
            let chained = dir.join(CHAINED);
            if chained.exists() {
                return Err(format!(
                    "{} isn't ours and {} exists already; merge them by hand first",
                    hook.display(),
                    chained.display()
                ));
            }
            fs::rename(&hook, &chained).map_err(|e| e.to_string())?;
            backup = Some(chained);
        }
    }
    fs::write(&hook, post_checkout_script(lords_exe, args)).map_err(|e| e.to_string())?;
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
