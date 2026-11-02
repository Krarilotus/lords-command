//! The post-checkout hook as a config hook (D14b, design note 2026-11-02-lords-hooks-config): where the repo's own hooks
//! dir isn't reached, `hook.lords-post-checkout` in the repo's local config file, which Git (2.54 and later) runs beside
//! whatever `core.hooksPath` runs. It stands only once the probe gets its answer through it (C3); a failure puts the
//! config file back as it was (C7); `lords uninstall` removes it while it holds what Lords wrote (C8).

use super::{
    ours, post_checkout_reaches_new_worktree, post_checkout_script, probe_label, Installed,
    SyncArgs,
};
use crate::fsguard;
use crate::git;
use crate::ledger::{self, Ledger, Owned};
use crate::sync::ApplyLock;
use std::path::{Path, PathBuf};

/// The hook's section: `hook.<friendly name>.event` and `.command`. Also the one part of its ledger record.
pub const SECTION: &str = "hook.lords-post-checkout";
const PATTERN: &str = r"^hook\.lords-post-checkout\.";
/// Its ledger record's scope, `hookconfig:<repo name>`, the only one with kind `config`.
pub const SCOPE: &str = "hookconfig:";

/// The repo's local config file: `<git common dir>/config`.
fn file(repo: &Path) -> Result<PathBuf, String> {
    let common = git::run(repo, &["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
    Ok(Path::new(common.trim()).join("config"))
}

/// The key of the ledger's record of `file`'s entry.
fn key(file: &Path) -> String {
    ledger::key(&file.join(SECTION))
}

/// The entry in `file` alone (`git config --file … -z`), or `None` where it has none.
fn entries(file: &Path) -> Result<Option<Vec<u8>>, String> {
    let (dir, f) = (file.parent().unwrap_or(file), file.to_string_lossy());
    let args = ["config", "--file", &f, "-z", "--get-regexp", PATTERN];
    Ok(if git::status(dir, &args)? { Some(git::bytes(dir, &args)?) } else { None })
}

fn git_file(file: &Path, args: &[&str]) -> Result<(), String> {
    let f = file.to_string_lossy();
    git::run(file.parent().unwrap_or(file), &[&["config", "--file", &f][..], args].concat())
        .map(|_| ())
}

/// Takes Lords' keys out of `file`, the event first, each where it is set. Git drops the section only where that leaves
/// it empty, so an owner's note in it stays (sol-loop r1, S1).
fn unset(file: &Path) -> Result<(), String> {
    let (dir, f) = (file.parent().unwrap_or(file), file.to_string_lossy());
    for key in [format!("{SECTION}.event"), format!("{SECTION}.command")] {
        if git::status(dir, &["config", "--file", &f, "--get-all", &key])? {
            git_file(file, &["--unset-all", &key])?;
        }
    }
    Ok(())
}

/// Puts `command` in as the hook's, then the event (an event without a command makes every hook run fail), each in
/// place where it is set, so an earlier entry put back is what it was.
fn put(file: &Path, command: &str) -> Result<(), String> {
    git_file(file, &[&format!("{SECTION}.command"), command])?;
    git_file(file, &[&format!("{SECTION}.event"), "post-checkout"])
}

/// The command of Lords' entry (`entries`); Lords' entry always has one.
fn command_of(entry: &[u8]) -> String {
    let text = String::from_utf8_lossy(entry).into_owned();
    let found = text.split('\0').find_map(|e| e.strip_prefix(&format!("{SECTION}.command\n")));
    found.unwrap_or_default().to_string()
}

/// A failure's rollback (C7): where `file` differs from `before`, `undo`, then whether it is byte for byte as before.
fn back(
    file: &Path,
    before: &Option<Vec<u8>>,
    undo: impl FnOnce() -> Result<(), String>,
) -> String {
    let at = file.display();
    let now = fsguard::read(file).and_then(|now| {
        if now == *before {
            Ok(now)
        } else {
            undo().and_then(|()| fsguard::read(file))
        }
    });
    match now {
        Ok(now) if now == *before => format!("{at} is as it was"),
        Ok(_) => format!("{at} differs from before, outside what Lords wrote"),
        Err(u) => format!("putting {at} back failed: {u}"),
    }
}

/// Whether the ledger records a config hook for `repo`: it then stays the repo's route (C6).
pub(super) fn recorded(repo: &Path, ledger: &Ledger) -> Result<bool, String> {
    Ok(ledger.owned.get(&key(&file(repo)?)).is_some_and(|o| o.kind == "config"))
}

/// Writes the hook into `repo`'s local config, under the caller's apply lock, and keeps it only once the probe answers
/// through it and the ledger is saved; `why` says why the hooks dir isn't its place. Refused before any write where Git
/// doesn't list config hooks (C4), where Lords' own hook stands in the hooks dir (C6), or where the name is set
/// anywhere but in Lords' recorded entry (C5); after one, the config file goes back as it was and is compared (C7).
pub(super) fn install(
    repo: &Path,
    lords_exe: &Path,
    args: &SyncArgs,
    home: &Path,
    mut ledger: Ledger,
    why: String,
) -> Result<Installed, String> {
    let file = file(repo)?;
    // Git 2.43 says "unknown subcommand" (129); 2.54 and later list them, or warn that there are none (1).
    git::status(repo, &["hook", "list", "post-checkout"]).map_err(|e| {
        format!("`git hook list post-checkout` failed, so this Git doesn't list config hooks (2.54 and later do): {e}")
    })?;
    let hook = file.with_file_name("hooks").join("post-checkout");
    if ours(&ledger, &hook)? {
        return Err(format!("Lords' own post-checkout stands in {}, and both would run: take it out (and a `{}` back to its name) first", hook.display(), super::CHAINED));
    }
    let local = entries(&file)?;
    let all = ["config", "-z", "--show-scope", "--get-regexp", PATTERN];
    let seen = if git::status(repo, &all)? { git::run(repo, &all)? } else { String::new() };
    let scopes: Vec<&str> = seen.split('\0').step_by(2).filter(|s| !s.is_empty()).collect();
    let mine = ledger.owned.get(&key(&file)).filter(|o| o.kind == "config").map(|o| o.sha.clone());
    let earlier = match (&local, mine) {
        (None, _) if scopes.is_empty() => None,
        (Some(b), Some(sha))
            if b.iter().filter(|&&c| c == 0).count() == scopes.len() && fsguard::sha(b) == sha =>
        {
            Some(command_of(b))
        }
        _ => {
            let mut s = scopes.clone();
            s.sort();
            s.dedup();
            return Err(format!(
                "{SECTION} is set already ({}) and isn't the tool's",
                s.join(", ")
            ));
        }
    };
    let before = fsguard::read(&file)?;
    let label = probe_label();
    let script = post_checkout_script(lords_exe, args, home, &label, false);
    // A function, so the hook's arguments, which Git appends as `"$@"`, are its own.
    let command = format!(
        "lords_post_checkout() {{\n{}}}\nlords_post_checkout",
        script.strip_prefix("#!/bin/sh\n").unwrap_or(&script)
    );
    let done = put(&file, &command)
        .and_then(|()| post_checkout_reaches_new_worktree(repo, home, &label))
        .and_then(|()| {
            let now = entries(&file)?.ok_or("its entry is gone")?;
            let owned = Owned {
                root: file.to_string_lossy().into_owned(),
                parts: vec![SECTION.to_string()],
                kind: "config".into(),
                sha: fsguard::sha(&now),
                scope: format!("{SCOPE}{}", args.repo),
            };
            ledger.owned.insert(key(&file), owned);
            ledger.save(home)
        });
    if let Err(e) = done {
        let undo = || match &earlier {
            Some(old) => put(&file, old),
            None => unset(&file),
        };
        return Err(format!("{e}; {}", back(&file, &before, undo)));
    }
    Ok(Installed {
        hook: file,
        backup: None,
        global_hooks_path: None,
        via: None,
        config: Some(why),
    })
}

/// `lords uninstall`'s config hooks (C8), each the ledger records for `repo` (for every repo where `None`): an entry that
/// still holds what Lords wrote goes, with its record; one that is gone or changed loses its record, and a changed one
/// stays as it is; one that can't be read stays, with its record. Only `apply` changes anything. One line each. All or
/// nothing, as install is (review r1, D14b-R1-01): the ledger is saved first, so a save that fails leaves every entry as
/// it was; a removal that fails after it puts its entry back as a refused install does, and its record.
pub fn uninstall(home: &Path, repo: Option<&str>, apply: bool) -> Result<Vec<String>, String> {
    let _lock = if apply { Some(ApplyLock::take(home, 60)?) } else { None };
    let mut ledger = Ledger::load(home)?;
    let mine: Vec<Owned> = ledger
        .owned
        .values()
        .filter(|o| o.scope.strip_prefix(SCOPE).is_some_and(|n| repo.is_none_or(|r| r == n)))
        .cloned()
        .collect();
    let (mut lines, mut going) = (Vec::new(), Vec::new());
    for o in &mine {
        let file = PathBuf::from(&o.root);
        let what = format!("{SECTION} in {}", file.display());
        let line = match entries(&file).and_then(|e| fsguard::read(&file).map(|b| (e, b))) {
            Ok((None, _)) => "gone already; its ledger record goes",
            Ok((Some(entry), before)) if fsguard::sha(&entry) == o.sha => {
                going.push((o, file.clone(), before, command_of(&entry)));
                "Lords', unchanged: it goes"
            }
            Ok(_) => "changed since Lords wrote it: left as it is, and no longer the tool's (its record goes)",
            Err(e) => {
                lines.push(format!("{what}: can't check it ({e}); left as it is, with its record"));
                continue;
            }
        };
        ledger.forget(&o.path());
        lines.push(format!("{what}: {line}"));
    }
    if !apply || mine.is_empty() {
        return Ok(lines);
    }
    ledger.save(home)?;
    let mut kept = false;
    for (o, file, before, old) in going {
        if let Err(e) = unset(&file) {
            let said = back(&file, &before, || put(&file, &old));
            ledger.owned.insert(key(&file), o.clone());
            kept = true;
            lines.push(format!(
                "{SECTION} in {}: can't remove it ({e}); {said}, with its record",
                file.display()
            ));
        }
    }
    if kept {
        ledger.save(home)?;
    }
    Ok(lines)
}
