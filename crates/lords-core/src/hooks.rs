//! `hooks install`: the privacy check as a repo's pre-commit, pre-merge-commit and commit-msg hooks, in
//! `.git/hooks` (design: Privacy).
//! Never through `core.hooksPath`: a global hooks path whose dispatcher runs the repo's own hooks keeps working.
//! A hook is the tool's only by its ledger record (design: Ownership boundary): any other hook there, readable or
//! not, is the owner's (Sol review r2, R2-16). Every write goes through `fsguard`, below the checkout's verified
//! hooks root (design-simplification-r6, rule A), and nothing is overwritten. Installing takes the apply lock, so
//! the ledger has one writer at a time (Sol review r3, R3-09).

use crate::fsguard::{self, Contained, Part, Root};
use crate::git;
use crate::ledger::Ledger;
use crate::sync::ApplyLock;
use std::path::{Path, PathBuf};

pub const MARKER: &str = "# lords-command: privacy check";

/// The hooks the tool installs: the only names a `hooks:` ledger record may have.
pub const NAMES: [&str; 5] =
    ["pre-commit", "pre-merge-commit", "commit-msg", "pre-push", "post-checkout"];

/// The privacy hooks, which a commit or an automatic merge commit runs; `lords check` verifies each by its probe.
pub const PRIVACY: [&str; 3] = ["pre-commit", "pre-merge-commit", "commit-msg"];

pub struct Installed {
    pub hook: PathBuf,
    pub backup: Option<PathBuf>,
    /// The global `core.hooksPath`, if any: its dispatcher must run `.git/hooks/pre-commit`.
    pub global_hooks_path: Option<String>,
}

/// The repo's own commit check, which the pre-commit hook runs first where the repo tracks one (orchestrator, 25 Oct:
/// clippy `-D warnings` a required part of Lords Command's commit check).
pub const COMMIT_CHECK: &str = "tools/commit-check.sh";

/// The environment variable `lords check` sets to a nonce when it probes the hooks (design-simplification-r6, rule
/// D and the review's point 1).
pub const PROBE_VAR: &str = "LORDS_HOOK_PROBE";

/// What a hook answers a probe with.
pub fn probe_answer(nonce: &str, hook: &str) -> String {
    format!("lords-hook-probe:{nonce}:{hook}")
}

/// The first lines of a privacy hook: it answers a probe, and does nothing else then, only when the variable is a
/// nonce (32 hex digits) whose file `lords check` made in the tool's home just before. Any other value, set or
/// exported anywhere, runs the check below unchanged, so the probe is no way around it.
fn probe_lines(home: &Path, hook: &str) -> String {
    let state = quoted(&home.join("state"));
    let answer = probe_answer("$probe", hook);
    format!(
        "probe=\"${{{PROBE_VAR}:-}}\"\n\
         case \"$probe\" in\n\
         \x20 *[!0-9a-f]* | '') ;;\n\
         \x20 *) if [ ${{#probe}} -eq 32 ] && [ -f {state}/probe-\"$probe\" ]; then echo \"{answer}\"; exit 0; fi ;;\n\
         esac\n"
    )
}

/// The pre-commit hook: the repo's own commit check, then the privacy check of the staged files.
pub fn script(lords_exe: &Path, secrets_only: bool, home: &Path) -> String {
    staged_script("pre-commit", lords_exe, secrets_only, false, home)
}

/// The pre-merge-commit hook, which an automatic merge commit runs instead of pre-commit: the same checks of what
/// the merge stages (orchestrator, 26 Oct).
pub fn merge_script(lords_exe: &Path, secrets_only: bool, home: &Path) -> String {
    staged_script("pre-merge-commit", lords_exe, secrets_only, false, home)
}

/// `stack`: the repo is a stack repo, whose entries `lords check --stack` checks before the privacy check.
fn staged_script(
    hook: &str,
    lords_exe: &Path,
    secrets_only: bool,
    stack: bool,
    home: &Path,
) -> String {
    let exe = lords_exe.to_string_lossy().replace('\\', "/");
    let flag = if secrets_only { " --secrets-only" } else { "" };
    let stack = if stack {
        format!("# A stack repo: its entries, as `lords check` sees them.\n\"{exe}\" check --stack || exit $?\n")
    } else {
        String::new()
    };
    format!(
        "#!/bin/sh\n{MARKER} of the staged files (installed by `lords hooks install`).\n\
         {probe}\
         # The repo's own commit check first, where it tracks one ({COMMIT_CHECK}).\n\
         top=$(git rev-parse --show-toplevel) || exit 1\n\
         if git ls-files --error-unmatch -- \"{COMMIT_CHECK}\" >/dev/null 2>&1; then\n\
         \x20   sh \"$top/{COMMIT_CHECK}\" || exit $?\n\
         fi\n\
         {stack}\
         exec \"{exe}\" privacy --staged{flag}\n",
        probe = probe_lines(home, hook),
    )
}

/// A stack repo's pre-push hook (orchestrator, 27 Oct): every commit's message and patch, secrets only where the
/// stack is private; a public stack's whole tree too, as `lords check --public` checks it.
fn pre_push_script(lords_exe: &Path, secrets_only: bool) -> String {
    let exe = lords_exe.to_string_lossy().replace('\\', "/");
    let tail = if secrets_only {
        format!("exec \"{exe}\" privacy --history --secrets-only\n")
    } else {
        format!("\"{exe}\" privacy --history || exit $?\nexec \"{exe}\" check --public\n")
    };
    format!(
        "#!/bin/sh\n{MARKER} of the history to push (installed by `lords hooks install`).\n{tail}"
    )
}

/// Whether the repo tracks a stack: `stack.toml` at its top (the private stack) or `stack/stack.toml` (the public).
fn is_stack_repo(repo: &Path) -> Result<bool, String> {
    Ok(!git::run(repo, &["ls-files", "--", ":/stack.toml", ":/stack/stack.toml"])?
        .trim()
        .is_empty())
}

/// The commit-msg hook: git passes the message file as `$1`.
pub fn commit_msg_script(lords_exe: &Path, secrets_only: bool, home: &Path) -> String {
    let exe = lords_exe.to_string_lossy().replace('\\', "/");
    let flag = if secrets_only { " --secrets-only" } else { "" };
    format!(
        "#!/bin/sh\n{MARKER} of the commit message (installed by `lords hooks install`).\n{probe}exec \"{exe}\" privacy --message \"$1\"{flag}\n",
        probe = probe_lines(home, "commit-msg"),
    )
}

/// A fresh nonce: 32 hex digits no earlier probe used.
fn nonce() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    fsguard::sha(format!("{}-{nanos}", crate::process::id()).as_bytes())[..32].to_string()
}

/// Whether a commit in `repo` reaches both privacy hooks, found out by running them (design-simplification-r6, rule
/// D): `git hook run`, as a commit would run them, through any hooks path and dispatcher, with a nonce whose file
/// is made in `home` right before and deleted right after. A hook verifies when its answer comes back; any other
/// outcome, a git error included, is one line saying it isn't. This says what holds now: `--no-verify`, a hooks path
/// set later, or a commit made elsewhere isn't prevented by it (design: Privacy, "Guarantee 7").
pub fn privacy_dispatch(repo: &Path, home: &Path) -> Vec<String> {
    let both = |why: String| vec![format!("{}: not verified ({why})", PRIVACY.join(", "))];
    let n = nonce();
    let made = (|| -> Result<(Contained, Contained), String> {
        let root = Root::home(home)?;
        let state = root.join(&[Part::new("state")?]);
        fsguard::create_dir_all(&state)?;
        let probe = state.child(Part::new(&format!("probe-{n}"))?);
        let message = state.child(Part::new(&format!("probe-{n}.msg"))?);
        fsguard::create_file(&probe, b"")?;
        fsguard::create_file(&message, b"a probe of the privacy hooks\n")?;
        Ok((probe, message))
    })();
    let (probe, message) = match made {
        Ok(m) => m,
        Err(e) => return both(e),
    };
    let mut problems = Vec::new();
    for hook in PRIVACY {
        let mut args = vec!["hook", "run", "--ignore-missing", hook];
        let msg = message.path().to_string_lossy().into_owned();
        if hook == "commit-msg" {
            args.extend(["--", msg.as_str()]);
        }
        let answer = probe_answer(&n, hook);
        match crate::process::git_env(repo, &args, &[(PROBE_VAR, &n)]) {
            Ok(out) => {
                let said = format!(
                    "{}{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                );
                if !out.status.success() || !said.contains(&answer) {
                    problems.push(format!(
                        "{hook}: not verified: a commit here doesn't reach the privacy hook (`git hook run {hook}` gave {} and no answer); `lords hooks install` puts it in the repo's own hooks dir",
                        out.status
                    ));
                }
            }
            Err(e) => problems.push(format!("{hook}: not verified ({e})")),
        }
    }
    for f in [&probe, &message] {
        if let Err(e) = fsguard::delete_file(f) {
            problems.push(format!("the probe's file {} stays: {e}", f.path().display()));
        }
    }
    problems
}

/// Whether the hook at `hook` is the tool's: its ledger record, and still exactly what sync wrote there.
fn ours(ledger: &Ledger, hook: &Path) -> Result<bool, String> {
    Ok(match (ledger.get(hook), fsguard::read(hook)?) {
        (Some(o), Some(bytes)) => o.sha == fsguard::sha(&bytes),
        _ => false,
    })
}

/// Puts `text` in as the hook at `hook`, replacing the tool's own hook there, and records it.
fn put_hook(ledger: &mut Ledger, hook: &Contained, text: &str, scope: &str) -> Result<(), String> {
    let old = match fsguard::kind(hook.path())? {
        fsguard::Kind::Missing => None,
        _ => Some(fsguard::move_aside(hook)?),
    };
    if let Err(e) = fsguard::put_file(hook, text.as_bytes()) {
        if let Some(old) = &old {
            old.back()?;
        }
        return Err(e);
    }
    if let Some(old) = old {
        old.delete()?;
    }
    fsguard::make_executable(hook)?;
    ledger.record(hook, "file", &fsguard::sha(text.as_bytes()), scope);
    Ok(())
}

/// A name beside `hook` that nothing has yet: where an owner's hook goes when `--force` replaces it.
fn backup_name(dir: &Root, name: &str) -> Result<Contained, String> {
    for n in 0.. {
        let suffix = if n == 0 { String::new() } else { format!("-{n}") };
        let b = dir.join(&[Part::new(&format!("{name}.bak-{}{suffix}", crate::ops::today()))?]);
        if fsguard::kind(b.path())? == fsguard::Kind::Missing {
            return Ok(b);
        }
    }
    unreachable!()
}

/// The repo's hooks dir as a verified root: the repo's checkout root (git's top level, exactly), then git's common
/// hooks dir, made where it isn't yet.
fn hooks_dir(repo: &Path) -> Result<Root, String> {
    if git::status(repo, &["config", "--local", "--get", "core.hooksPath"])? {
        let local = git::run(repo, &["config", "--local", "--get", "core.hooksPath"])?;
        return Err(format!(
            "this repo sets core.hooksPath = {} locally, so .git/hooks would not run; unset it or point it at a dispatcher first",
            local.trim()
        ));
    }
    let top = git::run(repo, &["rev-parse", "--show-toplevel"])?;
    let dir = Root::hooks(&Root::checkout(Path::new(top.trim()))?)?;
    dir.create()?;
    Ok(dir)
}

/// Installs the privacy hooks (`PRIVACY`); `home` holds the ledger that records them and the probe's files. A hook
/// there that isn't the tool's stops them all, unless `force`, which moves each one to a new `<name>.bak-<date>`
/// first.
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
    let stack = is_stack_repo(repo)?;
    let mut hooks = vec![
        ("pre-commit", staged_script("pre-commit", lords_exe, secrets_only, stack, home)),
        (
            "pre-merge-commit",
            staged_script("pre-merge-commit", lords_exe, secrets_only, stack, home),
        ),
        ("commit-msg", commit_msg_script(lords_exe, secrets_only, home)),
    ];
    if stack {
        hooks.push(("pre-push", pre_push_script(lords_exe, secrets_only)));
    }
    let mut foreign = Vec::new();
    for (name, _) in &hooks {
        let hook = dir.join(&[Part::new(name)?]);
        if fsguard::kind(hook.path())? != fsguard::Kind::Missing && !ours(&ledger, hook.path())? {
            foreign.push(name.to_string());
        }
    }
    if !force {
        if let Some(name) = foreign.first() {
            return Err(format!(
                "{} exists and isn't the tool's (no ledger record); --force moves it aside and replaces it",
                dir.path().join(name).display()
            ));
        }
    }
    let global_hooks_path = git::run(repo, &["config", "--global", "--get", "core.hooksPath"])
        .ok()
        .map(|s| s.trim().to_string());
    let scope = format!("hooks:{}", crate::ledger::key(dir.path()));
    let mut installed = Vec::new();
    for (name, text) in hooks {
        let hook = dir.join(&[Part::new(name)?]);
        let mut backup = None;
        if foreign.iter().any(|f| f == name) {
            let b = backup_name(&dir, name)?;
            fsguard::move_to(&hook, &b)?;
            backup = Some(b.path().to_path_buf());
        }
        put_hook(&mut ledger, &hook, &text, &scope)?;
        installed.push(Installed {
            hook: hook.path().to_path_buf(),
            backup,
            global_hooks_path: global_hooks_path.clone(),
        });
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
    let hook = dir.join(&[Part::new("post-checkout")?]);
    let mut backup = None;
    if fsguard::kind(hook.path())? != fsguard::Kind::Missing && !ours(&ledger, hook.path())? {
        let chained = dir.join(&[Part::new(CHAINED)?]);
        if fsguard::kind(chained.path())? != fsguard::Kind::Missing {
            return Err(format!(
                "{} isn't the tool's and {} exists already; merge them by hand first",
                hook.path().display(),
                chained.path().display()
            ));
        }
        fsguard::move_to(&hook, &chained)?;
        backup = Some(chained.path().to_path_buf());
    }
    let scope = format!("hooks:{}", crate::ledger::key(dir.path()));
    put_hook(&mut ledger, &hook, &post_checkout_script(lords_exe, args), &scope)?;
    ledger.save(home)?;
    let global_hooks_path = git::run(repo, &["config", "--global", "--get", "core.hooksPath"])
        .ok()
        .map(|s| s.trim().to_string());
    Ok(Installed { hook: hook.path().to_path_buf(), backup, global_hooks_path })
}
