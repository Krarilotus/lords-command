//! `hooks install`: the privacy check as a repo's pre-commit, pre-merge-commit and commit-msg hooks, in
//! `.git/hooks` (design: Privacy); with `--post-checkout`, the hook that gives a new worktree its place files.
//! Never through `core.hooksPath`: a global hooks path whose dispatcher runs the repo's own hooks keeps working. The
//! post-checkout hook stands only where a probe sees a new worktree reach it, through a local or global hooks path too.
//! A hook is the tool's only by its ledger record (design: Ownership boundary): any other hook there, readable or
//! not, is the owner's (Sol review r2, R2-16). Every write goes through `fsguard`, below the checkout's verified
//! hooks root (design-simplification-r6, rule A), and nothing is overwritten. Installing takes the apply lock, so
//! the ledger has one writer at a time (Sol review r3, R3-09).

use crate::fsguard::{self, Contained, Part, Root};
use crate::git;
use crate::ledger::Ledger;
use crate::sync::ApplyLock;
use std::path::{Path, PathBuf};

pub mod config;

pub const MARKER: &str = "# lords-command: privacy check";

/// The hooks the tool installs: the only names a `hooks:` ledger record may have.
pub const NAMES: [&str; 6] =
    ["pre-commit", "pre-merge-commit", "commit-msg", "pre-push", "post-checkout", "post-commit"];

/// A declared repo's post-checkout hook, as a plan finds it (F8, C3): what `sync --apply` installs or replaces.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct RepoHook {
    pub repo: String,
    /// The checkout it is installed from: the repo's first declared one.
    pub checkout: PathBuf,
    pub state: HookState,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", content = "why", rename_all = "kebab-case")]
pub enum HookState {
    /// Lords' hook, as this build writes it.
    Current,
    /// None recorded: `sync --apply` installs it.
    Missing,
    /// Lords' hook, unchanged since it was written, in another form than this build's: `sync --apply` replaces it.
    Outdated,
    /// Recorded, but changed since Lords wrote it, or it can't be read: never written, only named.
    Changed(String),
}

/// The privacy hooks, which a commit or an automatic merge commit runs; `lords check` verifies each by its probe.
pub const PRIVACY: [&str; 3] = ["pre-commit", "pre-merge-commit", "commit-msg"];

pub struct Installed {
    pub hook: PathBuf,
    pub backup: Option<PathBuf>,
    /// The global `core.hooksPath`, if any: its dispatcher must run `.git/hooks/pre-commit`.
    pub global_hooks_path: Option<String>,
    /// The post-checkout hook: the `core.hooksPath` its probe went through, if one is set.
    pub via: Option<String>,
    /// The post-checkout hook as a config hook (`hook` is then the config file): why the hooks dir isn't its place.
    pub config: Option<String>,
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
/// It answers a probe like the commit hooks, so a push can tell that it is there and dispatches (review r1 R4).
fn pre_push_script(lords_exe: &Path, secrets_only: bool, home: &Path) -> String {
    let exe = lords_exe.to_string_lossy().replace('\\', "/");
    let tail = if secrets_only {
        format!("exec \"{exe}\" privacy --history --secrets-only\n")
    } else {
        format!("\"{exe}\" privacy --history || exit $?\nexec \"{exe}\" check --public\n")
    };
    format!(
        "#!/bin/sh\n{MARKER} of the history to push (installed by `lords hooks install`).\n{}{tail}",
        probe_lines(home, "pre-push")
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
    dispatch(repo, home, &PRIVACY)
}

/// Whether a push from `repo` reaches the privacy pre-push hook, found out as `privacy_dispatch` finds out the commit
/// hooks: `Err` with why not (review r1 R4: a fresh clone has no local hooks, and a push then checks nothing).
pub fn push_dispatch(repo: &Path, home: &Path) -> Result<(), String> {
    let problems = dispatch(repo, home, &["pre-push"]);
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("; "))
    }
}

/// Probes `hooks` in `repo` (see `privacy_dispatch`): one line for each that doesn't answer.
fn dispatch(repo: &Path, home: &Path, hooks: &[&str]) -> Vec<String> {
    let both = |why: String| vec![format!("{}: not verified ({why})", hooks.join(", "))];
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
    for &hook in hooks {
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
                        "{hook}: not verified: a {} here doesn't reach the privacy hook (`git hook run {hook}` gave {} and no answer); `lords hooks install` puts it in the repo's own hooks dir",
                        if hook == "pre-push" { "push" } else { "commit" },
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

/// The repo's hooks dir for the privacy hooks: `own_hooks_dir`, refused where a local `core.hooksPath` bypasses it.
fn hooks_dir(repo: &Path) -> Result<Root, String> {
    if git::status(repo, &["config", "--local", "--get", "core.hooksPath"])? {
        let local = git::run(repo, &["config", "--local", "--get", "core.hooksPath"])?;
        return Err(format!(
            "this repo sets core.hooksPath = {} locally, so .git/hooks would not run; unset it or point it at a dispatcher first",
            local.trim()
        ));
    }
    own_hooks_dir(repo)
}

/// The repo's own hooks dir as a verified root: the repo's checkout root (git's top level, exactly), then git's common
/// hooks dir, made where it isn't yet.
fn own_hooks_dir(repo: &Path) -> Result<Root, String> {
    let top = git::run(repo, &["rev-parse", "--show-toplevel"])?;
    let dir = Root::hooks(&Root::checkout(Path::new(top.trim()))?)?;
    dir.create()?;
    Ok(dir)
}

/// Installs the privacy hooks (`PRIVACY`), and in a stack repo its pre-push and post-commit (`post_commit_script`, with
/// no stacks of its own: its `lords` reads config.txt); `home` holds the ledger that records them and the probe's files.
/// A hook there that isn't the tool's stops them all, unless `force`, which moves each one to a new
/// `<name>.bak-<date>` first.
pub fn install(
    repo: &Path,
    lords_exe: &Path,
    secrets_only: bool,
    force: bool,
    home: &Path,
) -> Result<Vec<Installed>, String> {
    let none = SyncArgs { repo: String::new(), public_stack: None, private_stack: None };
    install_with(repo, lords_exe, secrets_only, force, home, &none)
}

/// `install`, with the stacks a stack repo's post-commit delivers from (`stacks`; its `repo` isn't read).
pub fn install_with(
    repo: &Path,
    lords_exe: &Path,
    secrets_only: bool,
    force: bool,
    home: &Path,
    stacks: &SyncArgs,
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
        hooks.push(("pre-push", pre_push_script(lords_exe, secrets_only, home)));
        hooks.push(("post-commit", post_commit_script(lords_exe, home, stacks)));
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
            via: None,
            config: None,
        });
    }
    ledger.save(home)?;
    Ok(installed)
}

// ---------------------------------------------------------------- post-commit

pub const DELIVER_MARKER: &str = "# lords-command: delivery after a store commit";

/// ` --public-stack '…' --private-stack '…'` for the stacks `args` names, or nothing.
fn stack_flags(args: &SyncArgs) -> String {
    let mut flags = String::new();
    if let Some(p) = &args.public_stack {
        flags.push_str(&format!(" --public-stack {}", quoted(p)));
    }
    if let Some(p) = &args.private_stack {
        flags.push_str(&format!(" --private-stack {}", quoted(p)));
    }
    flags
}

/// A stack repo's post-commit hook (F8, design note C1): a commit to the store reaches every place with nobody running
/// a command (`lords deliver --on-commit`, which delivers only for a store `[delivery] on_commit` lists, and only when
/// the stores moved). It never fails or slows the commit: it acts only in the store's own checkout (a linked
/// worktree's commit is no commit to the store), never for a delivery's own commit (`deliver::MARKER`), runs in the
/// background, bounds nothing itself (the delivery bounds its waits), and only logs, to
/// `<git common dir>/lords-post-commit.log`.
fn post_commit_script(lords_exe: &Path, home: &Path, stacks: &SyncArgs) -> String {
    format!(
        r#"#!/bin/sh
{DELIVER_MARKER} (installed by `lords hooks install`).
# Never fails or slows a commit: only the store's own checkout, never a delivery's own commit, in the background, logged.
[ -n "${{{marker}:-}}" ] && exit 0
{{ read -r gitdir; read -r common; }} <<EOF
$(git rev-parse --path-format=absolute --git-dir --git-common-dir 2>/dev/null)
EOF
[ -n "$common" ] && [ "$gitdir" = "$common" ] || exit 0
exe={exe}
log="$common/lords-post-commit.log"
top="$(git rev-parse --show-toplevel 2>/dev/null)"
if [ ! -f "$exe" ]; then
  echo "$(date '+%F %T') $top: lords not found at $exe; nothing delivered" >>"$log" 2>/dev/null
  exit 0
fi
(
  echo "$(date '+%F %T') $top: lords deliver" >>"$log"
  "$exe" deliver --on-commit "$top" --home {home}{stacks} >>"$log" 2>&1
  echo "$(date '+%F %T') $top: lords deliver exit $?" >>"$log"
) </dev/null >/dev/null 2>&1 &
exit 0
"#,
        marker = crate::deliver::MARKER,
        exe = quoted(lords_exe),
        home = quoted(home),
        stacks = stack_flags(stacks),
    )
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

/// A post-checkout hook that delivers a new worktree's place files (orchestrator, 25 Oct; F8, design note C3): it runs
/// `lords deliver --new-worktree` for the worktree, the safe-only delivery of that one place, which bounds its own waits
/// and records what it held. It never fails or slows the checkout: it acts only when the old HEAD is all zeros (`git
/// worktree add`), runs in the background, and only logs (to `<git common dir>/lords-post-checkout.log`); a hook that
/// was here before runs first, and its exit status is the hook's (`chain`; a config hook has no such neighbour). It
/// answers the install's probe first, with `label` (`probe_label`), and does nothing else then (`probe_lines`; D14,
/// design note H5).
pub fn post_checkout_script(
    lords_exe: &Path,
    args: &SyncArgs,
    home: &Path,
    label: &str,
    chain: bool,
) -> String {
    let chain = if chain {
        format!("before=\"$(dirname \"$0\")/{CHAINED}\"\nif [ -f \"$before\" ]; then\n  \"$before\" \"$@\"\n  status=$?\nfi\n")
    } else {
        String::new()
    };
    format!(
        r#"#!/bin/sh
{SYNC_MARKER} (installed by `lords sync --apply` or `lords hooks install --post-checkout`).
# Never fails or slows a checkout: only a new worktree (old HEAD all zeros), in the background, logged.
{probe}status=0
{chain}case "$1" in
  "" | *[!0]*) exit $status ;;
esac
exe={exe}
log="$(git rev-parse --git-common-dir 2>/dev/null || echo .)/lords-post-checkout.log"
place="$(pwd -W 2>/dev/null || pwd)"
if [ ! -f "$exe" ]; then
  echo "$(date '+%F %T') $place: lords not found at $exe; no place files written" >>"$log" 2>/dev/null
  exit $status
fi
(
  echo "$(date '+%F %T') $place: lords deliver" >>"$log"
  "$exe" deliver --repo {repo} --place "$place" --new-worktree --home {home}{stacks} >>"$log" 2>&1
  echo "$(date '+%F %T') $place: lords deliver exit $?" >>"$log"
) </dev/null >/dev/null 2>&1 &
exit $status
"#,
        exe = quoted(lords_exe),
        repo = quoted(Path::new(&args.repo)),
        home = quoted(home),
        stacks = stack_flags(args),
        probe = probe_lines(home, label),
    )
}

/// The probe label a Lords post-checkout script answers with (`probe_label`), read back from its text.
fn label_in(script: &str) -> Option<String> {
    let (_, rest) = script.split_once("lords-hook-probe:$probe:")?;
    let label = rest.split('"').next()?;
    label.starts_with("post-checkout-").then(|| label.to_string())
}

/// What sync writes into a declared repo's post-checkout (F8, C3, D-3): the guard copy delivers the new worktree,
/// from the stacks sync ran with, into `home`.
pub fn sync_args(stacks: &crate::stack::Stacks, repo: &str) -> SyncArgs {
    SyncArgs {
        repo: repo.to_string(),
        public_stack: Some(stacks.public.root.clone()),
        private_stack: stacks.private.as_ref().map(|p| p.root.clone()),
    }
}

/// Where a declared repo's post-checkout hook stands against what this build writes there with `exe` and `args`
/// (C3): read only. Lords' hook is found by its ledger record, in the hooks dir or as a config hook (D14b), and must
/// still hold what the record says; its text, but for the probe label it was made with, is then this build's or not.
/// An error where git can't tell the checkout's common dir or config.
pub fn post_checkout_state(
    checkout: &Path,
    ledger: &Ledger,
    exe: &Path,
    args: &SyncArgs,
    home: &Path,
) -> Result<HookState, String> {
    {
        let common =
            git::run(checkout, &["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
        let common = PathBuf::from(common.trim());
        let cfg = common.join("config");
        if config::recorded(checkout, ledger)? {
            let recorded = ledger.owned.get(&config::key(&cfg)).map(|o| o.sha.clone());
            return Ok(match config::entries(&cfg)? {
                Some(e) if Some(fsguard::sha(&e)) == recorded => {
                    let have = config::command_of(&e);
                    match label_in(&have) {
                        Some(l) if have == config::command_for(exe, args, home, &l) => {
                            HookState::Current
                        }
                        _ => HookState::Outdated,
                    }
                }
                _ => HookState::Changed(format!(
                    "{} in {} changed since Lords wrote it",
                    config::SECTION,
                    cfg.display()
                )),
            });
        }
        let hook = common.join("hooks").join("post-checkout");
        let Some(o) = ledger.get(&hook) else { return Ok(HookState::Missing) };
        Ok(match fsguard::read(&hook)? {
            // Lords' own, gone: put in again, as where none was recorded.
            None => HookState::Missing,
            Some(b) if fsguard::sha(&b) == o.sha => {
                let have = String::from_utf8_lossy(&b).into_owned();
                match label_in(&have) {
                    Some(l) if have == post_checkout_script(exe, args, home, &l, true) => {
                        HookState::Current
                    }
                    _ => HookState::Outdated,
                }
            }
            Some(_) => {
                HookState::Changed(format!("{} changed since Lords wrote it", hook.display()))
            }
        })
    }
}

/// What a post-checkout hook answers a probe with: its name and a token made for this install alone, so no other hook
/// can answer for it, another repo's or a copy of an earlier one (sol-loop r1 and r2, S2).
fn probe_label() -> String {
    format!("post-checkout-{}", &nonce()[..16])
}

/// Installs the post-checkout hook in the repo's own hooks dir (the common dir, so every worktree has it); `home`
/// holds the ledger. A hook there that isn't the tool's moves to `post-checkout.before-lords` and is chained; the
/// tool's own is replaced. It stands only once a new worktree's post-checkout is seen to reach it, through whatever
/// `core.hooksPath` or dispatcher is set (D14, design note 2026-11-02-lords-hooks-dispatcher, H1); otherwise the dir is
/// put back as it was, nothing is recorded, and the refusal names the case (H4, H6). `core.hooksPath` and a
/// dispatcher are never changed (H3). A conditional include that may route hooks for a new worktree, which the probe's
/// stand-in can't see, refuses before anything is written (review r1, R1-01). Where the probe refuses, the hook goes
/// into the repo's local config instead (`config`; D14b, design note 2026-11-02-lords-hooks-config); a repo whose config
/// hook the ledger records keeps that route (C6).
pub fn install_post_checkout(
    repo: &Path,
    lords_exe: &Path,
    args: &SyncArgs,
    home: &Path,
) -> Result<Installed, String> {
    let lock = ApplyLock::take(home, 60)?;
    install_post_checkout_held(repo, lords_exe, args, home, &lock)
}

/// `install_post_checkout` under the apply lock its caller holds (`sync --apply`, F8 C3).
pub fn install_post_checkout_held(
    repo: &Path,
    lords_exe: &Path,
    args: &SyncArgs,
    home: &Path,
    _lock: &ApplyLock,
) -> Result<Installed, String> {
    let unseen = routing_includes(repo)?;
    if !unseen.is_empty() {
        return Err(format!(
            "the repo's config has a conditional include that may route hooks (core.hooksPath, attributionGuard.*, hook.*, a further include, or a file that can't be read) in a new worktree, where the probe can't see it: {}; no post-checkout installed",
            unseen.join("; ")
        ));
    }
    let ledger = Ledger::load(home)?;
    if config::recorded(repo, &ledger)? {
        let why = "Lords' config hook is this repo's route (its ledger record)".to_string();
        return config::install(repo, lords_exe, args, home, ledger, why)
            .map_err(|e| format!("{e}; no post-checkout installed"));
    }
    match in_hooks_dir(repo, lords_exe, args, home, ledger) {
        Ok(installed) => Ok(installed),
        Err(Refused::Failed(e)) => Err(e),
        Err(Refused::Unreached(why)) => {
            config::install(repo, lords_exe, args, home, Ledger::load(home)?, why.clone())
                .map_err(|e| format!("{why}; nor a config hook: {e}; no post-checkout installed, and core.hooksPath and what it runs are as they were"))
        }
    }
}

/// Why `in_hooks_dir` installed nothing.
enum Refused {
    /// The probe got no answer, and the hooks dir is as it was: a config hook may stand in.
    Unreached(String),
    Failed(String),
}

impl From<String> for Refused {
    fn from(e: String) -> Refused {
        Refused::Failed(e)
    }
}

/// D14's install in the repo's own hooks dir, under the caller's apply lock.
fn in_hooks_dir(
    repo: &Path,
    lords_exe: &Path,
    args: &SyncArgs,
    home: &Path,
    mut ledger: Ledger,
) -> Result<Installed, Refused> {
    let dir = own_hooks_dir(repo)?;
    let hook = dir.join(&[Part::new("post-checkout")?]);
    let chained = dir.join(&[Part::new(CHAINED)?]);
    let mut backup = None;
    // The tool's earlier hook, aside until the new one is proven (sol-loop r1, S1).
    let mut earlier = None;
    if fsguard::kind(hook.path())? != fsguard::Kind::Missing {
        if ours(&ledger, hook.path())? {
            earlier = Some(fsguard::move_aside(&hook)?);
        } else {
            if fsguard::kind(chained.path())? != fsguard::Kind::Missing {
                return Err(Refused::Failed(format!(
                    "{} isn't the tool's and {} exists already; merge them by hand first",
                    hook.path().display(),
                    chained.path().display()
                )));
            }
            fsguard::move_to(&hook, &chained)?;
            backup = Some(chained.path().to_path_buf());
        }
    }
    let scope = format!("hooks:{}", crate::ledger::key(dir.path()));
    let label = probe_label();
    let text = post_checkout_script(lords_exe, args, home, &label, true);
    let mut unreached = false;
    // Written, proven, recorded: the ledger save is the last step that can still be undone (review r1, R1-02).
    let done = put_hook(&mut ledger, &hook, &text, &scope)
        .and_then(|()| {
            post_checkout_reaches_new_worktree(repo, home, &label).map_err(|why| {
                unreached = true;
                refusal(repo, &dir, &why)
            })
        })
        .and_then(|()| ledger.save(home));
    if let Err(why) = done {
        // As it was (H4): this hook gone, then the tool's earlier one or the owner's back at its name, where nothing is
        // overwritten; the ledger isn't saved.
        let undone = fsguard::kind(hook.path())
            .and_then(|k| match k {
                fsguard::Kind::Missing => Ok(()),
                _ => fsguard::delete_file(&hook),
            })
            .and_then(|()| match (&earlier, &backup) {
                (Some(aside), _) => aside.back(),
                (None, Some(_)) => fsguard::move_to(&chained, &hook),
                (None, None) => Ok(()),
            });
        let path = hook.path().display();
        return Err(match undone {
            Ok(()) if unreached => Refused::Unreached(format!("{why}; {path} is as it was")),
            Ok(()) => Refused::Failed(format!("{why}; {path} is as it was")),
            Err(e) => Refused::Failed(format!("{why}; putting {path} back failed: {e}")),
        });
    }
    if let Some(aside) = earlier {
        let at = aside.path().to_path_buf();
        aside.delete().map_err(|e| {
            format!(
                "installed and recorded, but the earlier hook's copy {} stays: {e}",
                at.display()
            )
        })?;
    }
    Ok(Installed {
        hook: hook.path().to_path_buf(),
        backup,
        global_hooks_path: None,
        via: hooks_path(repo).map(|(path, _)| path),
        config: None,
    })
}

/// The conditional includes of the repo's effective config (`includeIf.<condition>.path`, with where each is set) whose
/// file may route hooks: it sets `core.hooksPath`, `attributionGuard.*` or `hook.*`, includes further, or can't be read.
/// A new worktree may match a condition the probe's stand-in doesn't (`gitdir:` on its own git dir, `onbranch:` on its
/// branch), so the probe can't vouch for such an include; one that routes no hook can't move a hook (review r1, R1-01).
fn routing_includes(repo: &Path) -> Result<Vec<String>, String> {
    let pattern = r"^includeif\..*\.path$";
    if !git::status(repo, &["config", "--get-regexp", pattern])? {
        return Ok(Vec::new());
    }
    let out =
        git::run(repo, &["config", "-z", "--show-origin", "--path", "--get-regexp", pattern])?;
    let fields: Vec<&str> = out.split('\0').collect();
    let mut found = Vec::new();
    for [origin, entry] in fields.as_chunks::<2>().0 {
        let (key, value) = entry.split_once('\n').unwrap_or((entry, ""));
        // A relative include is relative to the file that sets it, whose own path is relative to the repo.
        let from = repo.join(origin.strip_prefix("file:").unwrap_or(origin));
        let file = from.parent().map_or_else(|| PathBuf::from(value), |d| d.join(value));
        let routes = match fsguard::kind(&file) {
            Ok(fsguard::Kind::Missing) => false,
            Ok(fsguard::Kind::File) => {
                let path = file.to_string_lossy();
                git::run(repo, &["config", "--file", &path, "--name-only", "--list"])
                    .map_or(true, |keys| keys.lines().any(routes_hooks))
            }
            _ => true,
        };
        if routes {
            found.push(format!("{key} = {value} ({origin})"));
        }
    }
    Ok(found)
}

/// Whether a config key routes hooks: where they are, what a dispatcher reads, config-defined hooks, further includes.
fn routes_hooks(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    k == "core.hookspath"
        || ["attributionguard.", "hook.", "include.", "includeif."].iter().any(|p| k.starts_with(p))
}

/// The effective `core.hooksPath` and the scope that sets it (`local`, `global`, …), if one is set.
fn hooks_path(repo: &Path) -> Option<(String, String)> {
    let out = git::run(repo, &["config", "--show-scope", "--get", "core.hooksPath"]).ok()?;
    let (scope, path) = out.trim().split_once('\t')?;
    Some((path.to_string(), scope.to_string()))
}

/// Whether a new worktree's post-checkout reaches the tool's hook, found out by running it as `git worktree add` would
/// (H1): `git hook run post-checkout` in an empty stand-in for the new worktree (the repo's common dir as its git dir),
/// so whatever lies between, a hooks path, a dispatcher, a path relative to the worktree, a skip, is run, never read.
/// The hook answers a nonce whose file is made in `home` just before, with `label`, the token of the install that wrote
/// it, so no other Lords hook can answer for it (`probe_label`); the nonce file and the stand-in go after.
fn post_checkout_reaches_new_worktree(repo: &Path, home: &Path, label: &str) -> Result<(), String> {
    let common = git::run(repo, &["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
    let n = nonce();
    let state = Root::home(home)?.join(&[Part::new("state")?]);
    fsguard::create_dir_all(&state)?;
    let probe = state.child(Part::new(&format!("probe-{n}"))?);
    let stand_in = state.child(Part::new(&format!("probe-{n}.worktree"))?);
    fsguard::create_file(&probe, b"")?;
    let ran = fsguard::create_dir_all(&stand_in).and_then(|()| {
        let wt = stand_in.path().to_string_lossy().into_owned();
        let zeros = "0".repeat(40);
        let args = ["hook", "run", "--ignore-missing", "post-checkout", "--", &zeros, &zeros, "1"];
        let env = [(PROBE_VAR, n.as_str()), ("GIT_DIR", common.trim()), ("GIT_WORK_TREE", &wt)];
        let out = crate::process::git_env(stand_in.path(), &args, &env)?;
        let said = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        if out.status.success() && said.contains(&probe_answer(&n, label)) {
            Ok(())
        } else {
            Err(format!(
                "`git hook run post-checkout` from a new worktree ended with {} and no answer",
                out.status
            ))
        }
    });
    // Both go, whatever the other did.
    let probe_gone = fsguard::delete_file(&probe);
    let stand_in_gone = fsguard::kind(stand_in.path()).and_then(|k| match k {
        fsguard::Kind::Missing => Ok(()),
        _ => fsguard::delete_tree(&stand_in),
    });
    match (ran, probe_gone.and(stand_in_gone)) {
        (Ok(()), gone) => gone,
        (Err(e), Ok(())) => Err(e),
        (Err(e), Err(stays)) => Err(format!("{e}; the probe's files stay: {stays}")),
    }
}

/// Why a new worktree's post-checkout doesn't reach `dir`'s hook, from the config the hooks path and the attribution
/// guard's dispatcher read (H6); `why` is the probe's outcome.
fn refusal(repo: &Path, dir: &Root, why: &str) -> String {
    let hook = dir.path().join("post-checkout");
    let Some((path, scope)) = hooks_path(repo) else {
        return format!("a new worktree's post-checkout doesn't reach {} ({why})", hook.display());
    };
    let skips = git::run(repo, &["config", "--get-all", "attributionGuard.skipPreviousHook"])
        .unwrap_or_default()
        .lines()
        .any(|l| l.trim() == "post-checkout");
    let previous =
        git::run(repo, &["config", "--path", "--get", "attributionGuard.previousHooksPath"])
            .ok()
            .map(|p| p.trim().to_string());
    let case = match previous {
        _ if skips => "its dispatcher skips the repo's own post-checkout (attributionGuard.skipPreviousHook)".to_string(),
        Some(p) if Path::new(&p).is_relative() => format!(
            "its dispatcher forwards to attributionGuard.previousHooksPath = {p}, a relative path, so to each worktree's own copy of it (tracked files); a hook installed once can't reach a new worktree"
        ),
        Some(p) if crate::ledger::key(Path::new(&p)) != crate::ledger::key(dir.path()) => format!(
            "its dispatcher forwards to attributionGuard.previousHooksPath = {p}, not to this repo's own hooks dir {}, the only one Lords writes",
            dir.path().display()
        ),
        _ => format!("it doesn't run {} for a new worktree", hook.display()),
    };
    format!("core.hooksPath = {path} ({scope}): {case} ({why})")
}
