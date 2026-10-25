//! `hooks install`: the privacy check as a repo's pre-commit and commit-msg hooks, in `.git/hooks` (design:
//! Privacy).
//! Never through `core.hooksPath`: a global hooks path whose dispatcher runs the repo's own hooks keeps working.
//! A hook is the tool's only by its ledger record (design: Ownership boundary): any other hook there, readable or
//! not, is the owner's (Sol review r2, R2-16). Every write goes through `fsguard`, and nothing is overwritten.
//! Installing takes the apply lock, so the ledger has one writer at a time (Sol review r3, R3-09).

use crate::fsguard;
use crate::git;
use crate::ledger::Ledger;
use crate::sync::ApplyLock;
use std::path::{Path, PathBuf};

pub const MARKER: &str = "# lords-command: privacy check";

/// The hooks the tool installs: the only names a `hooks:` ledger record may have.
pub const NAMES: [&str; 3] = ["pre-commit", "commit-msg", "post-checkout"];

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

/// Whether the hook at `hook` is the tool's: its ledger record, and still exactly what sync wrote there.
fn ours(ledger: &Ledger, hook: &Path) -> Result<bool, String> {
    Ok(match (ledger.get(hook), fsguard::read(hook)?) {
        (Some(o), Some(bytes)) => o.sha == fsguard::sha(&bytes),
        _ => false,
    })
}

/// Puts `text` in as the hook at `hook`, replacing the tool's own hook there, and records it.
fn put_hook(ledger: &mut Ledger, hook: &Path, text: &str, scope: &str) -> Result<(), String> {
    let old = match fsguard::kind(hook)? {
        fsguard::Kind::Missing => None,
        _ => Some(fsguard::move_aside(hook)?),
    };
    if let Err(e) = fsguard::put_file(hook, text.as_bytes()) {
        if let Some(old) = &old {
            fsguard::move_back(old, hook)?;
        }
        return Err(e);
    }
    if let Some(old) = old {
        fsguard::delete_file(&old)?;
    }
    fsguard::make_executable(hook)?;
    ledger.record(hook, "file", &fsguard::sha(text.as_bytes()), scope);
    Ok(())
}

/// A name beside `hook` that nothing has yet: where an owner's hook goes when `--force` replaces it.
fn backup_name(hook: &Path) -> Result<PathBuf, String> {
    let name = hook.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    for n in 0.. {
        let suffix = if n == 0 { String::new() } else { format!("-{n}") };
        let b = hook.with_file_name(format!("{name}.bak-{}{suffix}", crate::ops::today()));
        if fsguard::kind(&b)? == fsguard::Kind::Missing {
            return Ok(b);
        }
    }
    unreachable!()
}

fn hooks_dir(repo: &Path) -> Result<PathBuf, String> {
    if let Ok(local) = git::run(repo, &["config", "--local", "--get", "core.hooksPath"]) {
        return Err(format!(
            "this repo sets core.hooksPath = {} locally, so .git/hooks would not run; unset it or point it at a dispatcher first",
            local.trim()
        ));
    }
    let common = git::run(repo, &["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
    let dir = PathBuf::from(common.trim()).join("hooks");
    fsguard::create_dir_all(&dir)?;
    Ok(dir)
}

/// Installs the pre-commit and commit-msg hooks; `home` holds the ledger that records them. A hook there that isn't
/// the tool's stops both, unless `force`, which moves each one to a new `<name>.bak-<date>` first.
pub fn install(
    repo: &Path,
    lords_exe: &Path,
    secrets_only: bool,
    force: bool,
    home: &Path,
) -> Result<Vec<Installed>, String> {
    let dir = hooks_dir(repo)?;
    let _lock = ApplyLock::take(home, 60)?;
    let mut ledger = Ledger::load(home)?;
    let hooks = [
        ("pre-commit", script(lords_exe, secrets_only)),
        ("commit-msg", commit_msg_script(lords_exe, secrets_only)),
    ];
    let mut foreign = Vec::new();
    for (name, _) in &hooks {
        let hook = dir.join(name);
        if fsguard::kind(&hook)? != fsguard::Kind::Missing && !ours(&ledger, &hook)? {
            foreign.push(hook);
        }
    }
    if !force {
        if let Some(hook) = foreign.first() {
            return Err(format!(
                "{} exists and isn't the tool's (no ledger record); --force moves it aside and replaces it",
                hook.display()
            ));
        }
    }
    let global_hooks_path = git::run(repo, &["config", "--global", "--get", "core.hooksPath"])
        .ok()
        .map(|s| s.trim().to_string());
    let scope = format!("hooks:{}", crate::ledger::key(&dir));
    let mut installed = Vec::new();
    for (name, text) in hooks {
        let hook = dir.join(name);
        let mut backup = None;
        if foreign.contains(&hook) {
            let b = backup_name(&hook)?;
            fsguard::move_to(&hook, &b)?;
            backup = Some(b);
        }
        put_hook(&mut ledger, &hook, &text, &scope)?;
        installed.push(Installed { hook, backup, global_hooks_path: global_hooks_path.clone() });
    }
    ledger.save(home)?;
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

/// Installs the post-checkout hook in the repo's own hooks dir (the common dir, so every worktree has it); `home`
/// holds the ledger. A hook there that isn't the tool's moves to `post-checkout.before-lords` and is chained; the
/// tool's own is replaced.
pub fn install_post_checkout(
    repo: &Path,
    lords_exe: &Path,
    args: &SyncArgs,
    home: &Path,
) -> Result<Installed, String> {
    let dir = hooks_dir(repo)?;
    let _lock = ApplyLock::take(home, 60)?;
    let mut ledger = Ledger::load(home)?;
    let hook = dir.join("post-checkout");
    let mut backup = None;
    if fsguard::kind(&hook)? != fsguard::Kind::Missing && !ours(&ledger, &hook)? {
        let chained = dir.join(CHAINED);
        if fsguard::kind(&chained)? != fsguard::Kind::Missing {
            return Err(format!(
                "{} isn't the tool's and {} exists already; merge them by hand first",
                hook.display(),
                chained.display()
            ));
        }
        fsguard::move_to(&hook, &chained)?;
        backup = Some(chained);
    }
    let scope = format!("hooks:{}", crate::ledger::key(&dir));
    put_hook(&mut ledger, &hook, &post_checkout_script(lords_exe, args), &scope)?;
    ledger.save(home)?;
    let global_hooks_path = git::run(repo, &["config", "--global", "--get", "core.hooksPath"])
        .ok()
        .map(|s| s.trim().to_string());
    Ok(Installed { hook, backup, global_hooks_path })
}
