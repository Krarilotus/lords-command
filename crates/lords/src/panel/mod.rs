//! `lords panel serve` and `lords panel add` (design note 2026-10-31-lords-panel-remote, P1, P3, P4).
//!
//! `serve` answers the window on another PC (through an SSH forward) with this PC's stacks: `panel::handle` on
//! 127.0.0.1 alone. `add` sets a benchmon PC up for it, from this PC: without `--apply` it reads both PCs and prints
//! the exact targets; with it, it carries them out in that order, each change backed up first and made only over what
//! was read. Every remote call has a deadline here, and each script it runs there stops itself; no key material is
//! read or copied: the key is made on the other PC and only its `.pub` comes here.

mod add;

/// A line on stdout that never panics: a server whose stdout went away keeps serving, and an apply whose pipe closed
/// doesn't stop half way (`crate::say`).
macro_rules! say {
    ($($line:tt)*) => {
        crate::say(&format!($($line)*))
    };
}

use crate::Args;
use add::{Client, Local, Plan, Remote, LOCAL_PORT, SERVER_PORT, TASK};
use lords_core::fsguard::{self, Contained, Part};
use lords_core::{http, panel, process, sync};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const USAGE: &str =
    "usage: lords panel serve [--port N] | lords panel add ALIAS --bridge DIR [--embedded] [--apply]";

pub fn cmd(mut a: Args) -> Result<bool, String> {
    match a.positional().as_deref() {
        Some("serve") => serve(a),
        Some("add") => add_pc(a),
        _ => Err(USAGE.into()),
    }
}

/// `lords panel serve [--port N]`: until the process ends.
fn serve(mut a: Args) -> Result<bool, String> {
    let port = match a.value("--port")? {
        Some(p) => p.parse::<u16>().map_err(|_| "--port takes a port number".to_string())?,
        None => SERVER_PORT,
    };
    let p = crate::paths(&mut a)?;
    a.done()?;
    let paths = panel::Paths { public: p.public, private: p.private };
    // Made once, in the tool's home, owner-only; only its file is named, never its text (review r1 R2).
    let home = sync::default_home()?;
    let token = panel::token(&home)?;
    let listener = http::bind(port)
        .map_err(|e| format!("127.0.0.1:{port}: {e} (does a panel serve there already?)"))?;
    let bound = listener.local_addr().map_err(|e| e.to_string())?;
    say!("lords panel: serving the stacks on {bound}");
    say!("lords panel: requests carry the token in {}", home.join(panel::TOKEN_FILE).display());
    for line in panel::push_ready(&paths) {
        say!("lords panel: {line}");
    }
    http::serve(listener, http::Limits::default(), token, move |r| panel::handle(&paths, r));
    Ok(true)
}

/// How long a remote step may run there (its watchdog then ends it and all it started), the exe's transfer, how much
/// longer this PC waits for a step than the step may run, and a check on this PC.
const STEP: u64 = 60;
const SEND_EXE: u64 = 300;
const GRACE: Duration = Duration::from_secs(20);
const HERE: Duration = Duration::from_secs(20);

/// One remote step's PowerShell (review r1 R6): its block runs beside a watchdog, a process of its own that ends the
/// step's whole process tree at the step's deadline however the step is stuck; at its end the step ends the watchdog.
/// A step whose watchdog can't start stops before it does anything, with an error. `$staging` is this run's own dir
/// there (review r1 R7); `$me` is the account the step runs as, and `Only-Mine` refuses a dir that grants anyone else
/// (fix check r1 R2). `Open-Path` says why a dir's path isn't closed to other accounts, or nothing, failing closed:
/// any dir from it up to the drive's root that is a link (a junction, symlink or mount point: the dir behind it, and
/// what may change it, the walk can't see), never followed (fix check r3 N5, r4 N6); a profile whose parent isn't the
/// OS's own profiles directory (`ProfilesDirectory` under ProfileList, which the OS protects; fix check r4 N6); or the
/// first dir from it up to the profile that lets an account other than this one, SYSTEM and Administrators (and the
/// owner placeholders, CREATOR OWNER and OWNER RIGHTS) create, delete, delete what it holds, or change its permissions
/// or owner, as an entry or as its owner (fix check r2 N4: the panel's threat model; above the profile, a drive's root
/// lets every user create, so no list rule goes there). A dir's attributes are read without following it, so a link to
/// nothing counts too. `Hold-Install` waits, up to past an install step's own deadline, for the lock that the run's
/// install holds while it runs (sol-loop S3 on cbc9b9b), and gives it, held. Static: the
/// literals are joined at compile time, and what changes per run (the deadline, the run's id, a file's name) comes in
/// as arguments, each checked to be a plain word. No double quote and no `%` in any of it: cmd.exe, ssh's shell there,
/// would read them.
macro_rules! step {
    ($body:literal) => {
        concat!(
            "& { param($deadline, $run, $name) $ErrorActionPreference = 'Stop'; ",
            "$watch = Start-Process powershell -WindowStyle Hidden -PassThru -ArgumentList '-NoProfile', ",
            "'-NonInteractive', '-Command', ('Start-Sleep -Seconds {0}; taskkill /T /F /PID {1}' -f $deadline, $PID); ",
            "try { $staging = Join-Path $env:LOCALAPPDATA ('Programs\\lords-command\\staging\\' + $run); ",
            "$me = [Security.Principal.WindowsIdentity]::GetCurrent().User; ",
            "function Only-Mine($d) { $others = @((Get-Acl -LiteralPath $d).GetAccessRules($true, $true, ",
            "[Security.Principal.SecurityIdentifier]) | Where-Object { $_.AccessControlType -eq 'Allow' -and ",
            "$_.IdentityReference -ne $me }); if ($others.Count) { throw ('this run''s staging dir grants others ",
            "than this account: ' + $d) } }; ",
            "function Open-To($d) { $a = Get-Acl -LiteralPath $d; ",
            "$keep = @('S-1-5-18', 'S-1-5-32-544', 'S-1-3-0', 'S-1-3-4', $me.Value); ",
            "$o = $a.GetOwner([Security.Principal.SecurityIdentifier]).Value; if ($keep -notcontains $o) { return $o }; ",
            "foreach ($r in $a.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier])) { ",
            "if ($r.AccessControlType -eq 'Allow' -and $keep -notcontains $r.IdentityReference.Value -and ",
            "([int64]$r.FileSystemRights -band 0x500D0046)) { return $r.IdentityReference.Value } } }; ",
            "function Attr($d) { try { [IO.File]::GetAttributes($d) } ",
            "catch [IO.FileNotFoundException], [IO.DirectoryNotFoundException] {} }; ",
            "function Open-Path($p) { $top = $env:USERPROFILE.TrimEnd('\\'); $p = $p.TrimEnd('\\'); ",
            "if (-not $p.StartsWith($top + '\\', [StringComparison]::OrdinalIgnoreCase)) { ",
            "return ($p + ' is outside the user profile ' + $top) }; ",
            "$q = $p; while ($q) { $at = Attr $q; if ($null -ne $at -and ($at -band [IO.FileAttributes]::ReparsePoint)) { ",
            "return ($q + ' is a link (a junction, symlink or mount point): the dir behind it is not checked') }; ",
            "$q = Split-Path -Parent $q }; ",
            "$os = [Environment]::ExpandEnvironmentVariables((Get-ItemProperty -LiteralPath ",
            "'HKLM:\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\ProfileList').ProfilesDirectory).TrimEnd('\\'); ",
            "if ((Split-Path -Parent $top) -ine $os) { ",
            "return ($top + ' is not a profile in the OS profiles directory ' + $os) }; ",
            "while ($true) { if ($null -ne (Attr $p)) { $who = Open-To $p; if ($who) { $n = $who; try { $n = (New-Object ",
            "Security.Principal.SecurityIdentifier($who)).Translate([Security.Principal.NTAccount]).Value + ' (' + ",
            "$who + ')' } catch {}; return ($p + ' lets ' + $n + ' create, delete or change permissions there') } }; ",
            "if ($p -ieq $top) { return }; $p = Split-Path -Parent $p } }; ",
            "function Hold-Install { $l = Join-Path $staging 'install.lock'; $until = (Get-Date).AddSeconds(90); ",
            "while ($true) { try { return [IO.File]::Open($l, 'OpenOrCreate', 'ReadWrite', 'None') } ",
            "catch [IO.IOException] { if ((Get-Date) -gt $until) { throw ('this run''s install still runs there: ' + ",
            "$staging) }; Start-Sleep -Milliseconds 500 } } }; ",
            $body,
            " } finally { taskkill /T /F /PID $watch.Id 2>&1 | Out-Null } }"
        )
    };
}

/// The probe: it reads, writes nothing, and reads no private key (only `.pub` files). `open_to`: who else may change
/// the install path (fix check r2 N4). `listening`: what listens on the forward's port, read with failures stopping
/// the probe (embedded panel review r1 R1). `earlier`: what is in `staging\`, earlier runs' dirs (E12 R2).
const PROBE: &str = step!("$h = $env:USERPROFILE; $s = Join-Path $h '.ssh'; $a = Join-Path $env:LOCALAPPDATA 'Programs\\lords-command'; $c = Join-Path $s 'config'; $t = Join-Path $a 'config.txt'; [ordered]@{ open_to = @(Open-Path $a); home = $h; local_app_data = $env:LOCALAPPDATA; app_data = $env:APPDATA; ssh = [bool](Get-Command ssh.exe -ErrorAction SilentlyContinue); keygen = [bool](Get-Command ssh-keygen.exe -ErrorAction SilentlyContinue); ssh_config = $(if (Test-Path -LiteralPath $c) { [IO.File]::ReadAllText($c) } else { $null }); keys = @(Get-ChildItem -LiteralPath $s -Filter 'lords_panel_*.pub' -ErrorAction SilentlyContinue | ForEach-Object { [ordered]@{ name = $_.Name; text = [IO.File]::ReadAllText($_.FullName) } }); app = (Test-Path -LiteralPath (Join-Path $a 'lords-command.exe')); config_txt = $(if (Test-Path -LiteralPath $t) { [IO.File]::ReadAllText($t) } else { $null }); running = @(Get-Process lords-command -ErrorAction SilentlyContinue | Where-Object { $_.Path -like ($a + '\\*') }).Count; listening = @(Get-NetTCPConnection -State Listen | Where-Object { $_.LocalPort -eq 17879 }).Count; earlier = @($st = Join-Path $a 'staging'; if (Test-Path -LiteralPath $st) { Get-ChildItem -LiteralPath $st -Force | ForEach-Object { $_.Name } }) } | ConvertTo-Json -Compress -Depth 4");
/// This run's staging dir, made only where nothing is (a dir an earlier run left is never taken over), owner-only from
/// its creation (fix check r1 R2): its list protected, one entry, this account, for the dir and what it will hold, set
/// by the call that makes it. Its parents are made as any dir is. One that appeared meanwhile and grants others is
/// refused. Neither it nor its parent may be a link: inside the app dir and reached without one, it is on the app
/// dir's volume, so the token's move into the app dir is a rename and keeps its owner-only list (across volumes a move
/// copies, and the copy would take the app dir's list). Before anything, the install path is checked again as the probe
/// checked it (fix check r2 N4): no account other than this one, SYSTEM and Administrators may change it, so none but
/// an administrator can put another dir in the staging dir's place.
const STAGE: &str = step!("if (Test-Path -LiteralPath $staging) { throw ('this run''s staging dir is there already: ' + $staging) }; $up = Split-Path -Parent $staging; $why = @(Open-Path $up); if ($why.Count) { throw ('the install path: ' + $why[0]) }; New-Item -ItemType Directory -Force -Path $up | Out-Null; $link = { param($d) if ((Get-Item -Force -LiteralPath $d).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw ('a link on the way to this run''s staging dir, which must be on the app dir''s volume: ' + $d) } }; & $link $up; $sec = New-Object System.Security.AccessControl.DirectorySecurity; $sec.SetAccessRuleProtection($true, $false); $sec.AddAccessRule((New-Object System.Security.AccessControl.FileSystemAccessRule($me, 'FullControl', 'ContainerInherit, ObjectInherit', 'None', 'Allow'))); [void][IO.Directory]::CreateDirectory($staging, $sec); & $link $staging; Only-Mine $staging; 'staged'");
/// One file, from stdin, into this run's staging dir, only where nothing is, and only while that dir grants no one but
/// this account (fix check r1 R2: the token's bytes arrive only there, owner-only from their first byte).
const RECEIVE: &str = step!("Only-Mine $staging; $out = [IO.File]::Open((Join-Path $staging $name), 'CreateNew', 'Write'); try { [Console]::OpenStandardInput().CopyTo($out) } finally { $out.Close() }; 'received'");
const INSTALL_STEP: &str = step!("& (Join-Path $staging 'install.ps1')");
/// The run's install undone from its journal (E12 R2), by the run's own undo.ps1, once that install has ended (it holds
/// its lock while it runs: sol-loop S3); a run whose install never began has nothing to undo, and gets a journal that
/// says so, so that an install arriving late won't begin.
const UNDO_STEP: &str = step!("if (-not (Test-Path -LiteralPath $staging)) { 'nothing'; return }; $held = Hold-Install; try { $j = Join-Path $staging 'journal'; if (Test-Path -LiteralPath $j) { & (Join-Path $staging 'undo.ps1') } else { Add-Content -LiteralPath $j -Value 'undone' -Encoding UTF8; 'nothing' } } finally { $held.Close() }");
const CHECK_STEP: &str = step!("& (Join-Path $staging 'check.ps1')");
/// The run's staging dir, which that run made (STAGE refuses one that is there): only what a run created goes; one
/// that isn't there (its STAGE failed before making it) is nothing to remove. Once the run's install has ended, all but
/// its lock goes while the lock is held (an install arriving late finds no script), then the rest.
const CLEAN: &str = step!("if (Test-Path -LiteralPath $staging) { $held = Hold-Install; try { Get-ChildItem -Force -LiteralPath $staging | Where-Object { $_.Name -ne 'install.lock' } | Remove-Item -Recurse -Force } finally { $held.Close() }; Remove-Item -Recurse -Force -LiteralPath $staging }; 'cleaned'");
/// How long the undo and clean steps may run: their wait for an install still running (`Hold-Install`, 90 s, past an
/// install step's own deadline), and then their work.
const UNDO_DEADLINE: u64 = 150;

const INSTALL: &str = include_str!("install.ps1");
const UNDO: &str = include_str!("undo.ps1");
const CHECK: &str = include_str!("check.ps1");
const TASK_XML: &str = include_str!("task.xml");
const README: &str = include_str!("../../../../README.md");
const LICENSE: &str = include_str!("../../../../LICENSE");

/// A plain word: what a step takes as an argument (a run's id, a file's name).
fn plain(word: &str) -> Result<&str, String> {
    // Not `.` or `..` either: a word only of dots names a dir above.
    let ok = !word.is_empty()
        && !word.starts_with('-')
        && !word.bytes().all(|b| b == b'.')
        && word.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
    if ok {
        Ok(word)
    } else {
        Err(format!("`{word}` is no plain word"))
    }
}

/// The command line that runs `step` with its arguments, as ssh passes it to the other PC's shell.
fn step_command(step: &str, deadline: u64, run: &str, name: &str) -> Result<Vec<String>, String> {
    let words = [plain(run)?, plain(name)?];
    let mut line: Vec<String> =
        ["powershell", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command"]
            .iter()
            .map(|s| s.to_string())
            .collect();
    line.push(format!("\"{step}\""));
    line.push(deadline.to_string());
    line.extend(words.iter().map(|w| w.to_string()));
    Ok(line)
}

/// Runs one step on `alias`, its own deadline `deadline` seconds there, `GRACE` more here: its last line of output,
/// or why not.
fn remote(
    alias: &str,
    step: &str,
    deadline: u64,
    run: &str,
    name: &str,
    stdin: Option<Vec<u8>>,
) -> Result<String, String> {
    let command = step_command(step, deadline, run, name)?;
    let args: Vec<&str> = command.iter().map(String::as_str).collect();
    let out = process::ssh(alias, &args, stdin, Duration::from_secs(deadline) + GRACE)?;
    let said = String::from_utf8_lossy(&out.stdout);
    let last = said.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
        return Err(format!(
            "on {alias}, a step ({run} {name}) failed or was ended at its {deadline} s ({}): {err}",
            out.status
        ));
    }
    Ok(last)
}

fn probe(other: &dyn Other, alias: &str, run: &str) -> Result<Remote, String> {
    let line = other
        .step(PROBE, STEP, run, "probe", None)
        .map_err(|e| format!("{alias} can't be read over ssh: {e}"))?;
    serde_json::from_str(line.trim_start_matches('\u{feff}')).map_err(|e| {
        format!("{alias}'s probe said `{}`: {e}", line.chars().take(80).collect::<String>())
    })
}

fn same_path(a: &Path, b: &Path) -> bool {
    let key = |p: &Path| p.to_string_lossy().replace('/', "\\").to_lowercase();
    key(a) == key(b)
}

/// This PC's side: the bridge's files, its listener and sessions, the task, the panel's token, and whether the server
/// answers. Reads only.
fn read_local(bridge: &Path, home: &Path) -> Result<Local, String> {
    let read = |name: &str| fsguard::read_text(&bridge.join(name));
    let sshd_config = read("sshd_config")?
        .ok_or_else(|| format!("{}: no sshd_config, so no SSH bridge", bridge.display()))?;
    let status: serde_json::Value = match read("status.json")? {
        Some(t) => serde_json::from_str(t.trim_start_matches('\u{feff}'))
            .map_err(|e| format!("the bridge's status.json: {e}"))?,
        None => serde_json::Value::Null,
    };
    let sshd = bridge.join("sshd.exe");
    let all = process::running()?;
    // The listener its supervisor names, only while it is that sshd: a stale status is no listener.
    let listener = status
        .as_array()
        .into_iter()
        .flatten()
        .find(|c| c["Component"] == "Sshd" && c["Running"] == true)
        .and_then(|c| c["ProcessId"].as_u64())
        .map(|p| p as u32)
        .filter(|p| all.iter().any(|r| r.pid == *p && same_path(&r.image, &sshd)));
    let sessions = listener
        .map(|l| {
            all.iter()
                .filter(|r| r.parent == l && same_path(&r.image, &sshd))
                .map(|r| r.pid)
                .collect()
        })
        .unwrap_or_default();
    let queried = process::schtasks(&["/Query", "/TN", TASK], HERE)?;
    let task = match queried.status.code() {
        Some(0) => true,
        Some(1) => false,
        _ => return Err(format!("schtasks /Query /TN {TASK} failed ({})", queried.status)),
    };
    // A server answers only with the token; with none made yet, none is serving.
    let token = panel::existing_token(home)?;
    let served = token.as_ref().is_some_and(answers);
    Ok(Local {
        bridge: bridge.to_path_buf(),
        sshd_config,
        authorized_keys: read("authorized_keys")?.unwrap_or_default(),
        listener,
        sessions,
        task,
        served,
        home: home.to_path_buf(),
        token: token.is_some(),
    })
}

/// Whether the panel's server on this PC answers its token.
fn answers(token: &http::Token) -> bool {
    http::Client::new(SERVER_PORT, token.clone())
        .call("GET", "/status", None, Duration::from_secs(5))
        .is_ok_and(|(s, _)| s == 200)
}

/// This run's own id: it names the run's staging dirs on both PCs and its backup dir there.
fn run_id() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{}-{secs}-{}", lords_core::ops::today(), process::id())
}

/// `lords panel add ALIAS --bridge DIR [--embedded] [--apply]`.
fn add_pc(mut a: Args) -> Result<bool, String> {
    let apply = a.flag("--apply");
    // Before `--bridge`'s value, so `--bridge --embedded` can't take the flag for the dir.
    let client = if a.flag("--embedded") { Client::Embedded } else { Client::Standalone };
    let bridge = a.value("--bridge")?.map(PathBuf::from).ok_or(
        "panel add needs --bridge DIR: this PC's SSH bridge (its sshd_config, authorized_keys, status.json, sshd.exe)",
    )?;
    let alias =
        a.positional().ok_or("panel add needs the SSH alias this PC reaches the other PC by")?;
    a.done()?;
    lords_core::config::check_alias(&alias)?;
    let home_dir = sync::default_home()?;
    // One `--apply` at a time on this PC, held from before the probe to the end (E12 R2): every staging dir the probe
    // finds on the other PC is then an earlier run's, ended, and is undone and removed first.
    let _one_at_a_time = if apply {
        let home = fsguard::Root::home(&home_dir)?;
        home.create()?;
        let lock = home.join(&[Part::new("panel-add.lock")?]);
        match fsguard::try_lock(&lock, &format!("lords panel add {alias}"))? {
            fsguard::TryLock::Held(held) => Some(held),
            fsguard::TryLock::Busy(who) => {
                return Err(format!(
                    "another `lords panel add --apply` runs on this PC ({who}); run again when it has ended"
                ))
            }
        }
    } else {
        None
    };
    let run = run_id();
    let local = read_local(&bridge, &home_dir)?;
    let ssh = Ssh(&alias);
    let other = probe(&ssh, &alias, &run)?;
    let plan = add::plan(&alias, &local, &other, client)?;
    for line in plan.describe(&alias, &local, &other, &run) {
        say!("{line}");
    }
    if !apply {
        say!("dry run: nothing written (`--apply` carries these out, in this order)");
        return Ok(true);
    }
    let plan = plan_to_apply(&ssh, &alias, &local, &run, plan)?;
    // This run's own dir in the tool's home, made only where nothing is, and removed whole at the end: it holds only
    // what this run puts there (review r1 R7).
    let home = fsguard::Root::home(&home_dir)?;
    let dir = home.join(&[Part::new(&format!("panel-add-{run}"))?]);
    if fsguard::kind(dir.path())? != fsguard::Kind::Missing {
        return Err(format!("{}: this run's dir is there already", dir.path().display()));
    }
    fsguard::create_dir_all(&dir)?;
    let done = carry_out(&alias, &local, &plan, &dir, &run);
    let cleaned = fsguard::delete_tree(&dir);
    let done = done?;
    cleaned?;
    Ok(done)
}

fn put(dir: &Contained, name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    let f = dir.child(Part::new(name)?);
    fsguard::create_file(&f, bytes)?;
    Ok(f.path().to_path_buf())
}

fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Changes one of the bridge's files: a backup of what is there (once, never over an earlier one), then the new text,
/// only while the file still holds what was read.
fn change_bridge_file(bridge: &Path, name: &str, was: &str, new: &str) -> Result<(), String> {
    let root = fsguard::Root::bridge(bridge)?;
    let file = root.join(&[Part::new(name)?]);
    let backup = root.join(&[Part::new(&format!("{name}.before-lords-panel"))?]);
    if fsguard::kind(file.path())? == fsguard::Kind::File
        && fsguard::kind(backup.path())? == fsguard::Kind::Missing
    {
        fsguard::create_file(&backup, was.as_bytes())?;
        say!("backed up {}", backup.path().display());
    }
    if fsguard::kind(file.path())? == fsguard::Kind::Missing {
        fsguard::create_file(&file, new.as_bytes())?;
    } else {
        fsguard::replace_file_if(&file, was.as_bytes(), new.as_bytes())?;
    }
    say!("wrote {}", file.path().display());
    Ok(())
}

fn carry_out(
    alias: &str,
    local: &Local,
    plan: &Plan,
    dir: &Contained,
    run: &str,
) -> Result<bool, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let window = window_files(plan.client, &exe)?;
    if exe.to_string_lossy().contains('\'') {
        return Err(format!("{}: a path with `'` can't go in the task's command", exe.display()));
    }
    // The panel's token, made here once if there is none yet; a copy goes to the other PC over ssh, from memory.
    let token = panel::token(&local.home)?;
    let files = payload(plan, run, window, &token);
    let other = Ssh(alias);
    // 1. The other PC: this run's staging dir, each file into it over stdin, then installed from it.
    let installed = install_there(&other, alias, run, files)?;
    let finished = finish(alias, local, plan, dir, run, &installed, &exe, &token);
    let cleaned = other.step(CLEAN, UNDO_DEADLINE, run, "staging", None);
    let checked = finished?;
    cleaned.map_err(|e| format!("{e} (this run's staging dir stays on {alias})"))?;
    say!("{alias}: removed this run's staging dir");
    Ok(checked)
}

/// The other PC, as `panel add` runs its steps there: over ssh (`Ssh`); a test runs them here.
trait Other {
    /// Runs `step` there as the run `run`, its own deadline `deadline` seconds: its last line of output, or why not.
    fn step(
        &self,
        step: &str,
        deadline: u64,
        run: &str,
        name: &str,
        stdin: Option<Vec<u8>>,
    ) -> Result<String, String>;
}

/// The other PC this PC reaches as the alias.
struct Ssh<'a>(&'a str);

impl Other for Ssh<'_> {
    fn step(
        &self,
        step: &str,
        deadline: u64,
        run: &str,
        name: &str,
        stdin: Option<Vec<u8>>,
    ) -> Result<String, String> {
        remote(self.0, step, deadline, run, name, stdin)
    }
}

/// What an undo step said, in words: undo.ps1's JSON `state`, or the step's word for a run without a journal.
fn undone(said: &str) -> &'static str {
    let state = serde_json::from_str::<serde_json::Value>(said.trim_start_matches('\u{feff}'))
        .ok()
        .and_then(|v| v["state"].as_str().map(str::to_string));
    match state.as_deref() {
        Some("complete") => "its install had finished; nothing undone",
        Some("undone") => "its install undone from its journal",
        _ => "no install begun; nothing to undo",
    }
}

/// The other PC's part of a run (E12 R2): this run's staging dir, each file into it, then installed from it. After
/// any failure the install is undone there from its journal (nothing, if it never began) and the staging dir removed;
/// where even the undo fails, the dir and its journal stay, and the next run undoes them first (`undo_earlier`). Only
/// the install changes the other PC, so it is left as it was or completely set up. What the install said.
fn install_there(
    other: &dyn Other,
    alias: &str,
    run: &str,
    files: Vec<Sent>,
) -> Result<serde_json::Value, String> {
    let result = (|| {
        other.step(STAGE, STEP, run, "staging", None)?;
        say!("{alias}: made this run's staging dir");
        for (name, bytes, deadline) in files {
            other.step(RECEIVE, deadline, run, name, Some(bytes))?;
        }
        say!("{alias}: sent this run's files");
        serde_json::from_str(&other.step(INSTALL_STEP, STEP, run, "install", None)?)
            .map_err(|e| format!("{alias}'s install said no JSON: {e}"))
    })();
    let failed = match result {
        Ok(installed) => return Ok(installed),
        Err(e) => e,
    };
    let said = other.step(UNDO_STEP, UNDO_DEADLINE, run, "undo", None).map_err(|e| {
        format!("{failed}; undoing it on {alias} failed too ({e}): this run's staging dir and its journal stay there, and the next panel add undoes them first")
    })?;
    let cleaned = match other.step(CLEAN, UNDO_DEADLINE, run, "staging", None) {
        Ok(_) => "its staging dir removed".to_string(),
        Err(e) => format!("its staging dir stays ({e}); the next panel add removes it"),
    };
    Err(format!("{failed}; on {alias}: {}, {cleaned}", undone(&said)))
}

/// The plan `--apply` carries out (sol-loop S2 on cbc9b9b): earlier runs' staging dirs on the other PC undone and
/// removed first, then the PC read and planned again, and that plan said: what their undo takes back (an alias, a key)
/// the first reading saw there. Without earlier runs, `first` itself.
fn plan_to_apply(
    other: &dyn Other,
    alias: &str,
    local: &Local,
    run: &str,
    first: Plan,
) -> Result<Plan, String> {
    if first.earlier.is_empty() {
        return Ok(first);
    }
    for earlier in &first.earlier {
        undo_earlier(other, alias, earlier)?;
    }
    let read = probe(other, alias, run)?;
    let plan = add::plan(alias, local, &read, first.client)?;
    if !plan.earlier.is_empty() {
        return Err(format!(
            "{alias}: earlier runs' staging dirs are there still: {:?}",
            plan.earlier
        ));
    }
    say!("{alias} read again, after the earlier runs were undone; carried out:");
    for line in plan.describe(alias, local, &read, run) {
        say!("{line}");
    }
    Ok(plan)
}

/// An earlier run's staging dir on the other PC (E12 R2): that run's install undone from its journal unless it
/// finished or was undone, then the dir removed.
fn undo_earlier(other: &dyn Other, alias: &str, run: &str) -> Result<(), String> {
    let said = other
        .step(UNDO_STEP, UNDO_DEADLINE, run, "undo", None)
        .map_err(|e| format!("undoing run {run}'s install on {alias}: {e}"))?;
    other
        .step(CLEAN, UNDO_DEADLINE, run, "staging", None)
        .map_err(|e| format!("removing run {run}'s staging dir on {alias}: {e}"))?;
    say!("{alias}: run {run}'s staging dir: {}, then removed", undone(&said));
    Ok(())
}

/// One file a run sends into its staging dir there: its name, its bytes, and its step's deadline.
type Sent = (&'static str, Vec<u8>, u64);

/// The standalone window's files, which a run sends for it alone: README.md, LICENSE, and `lords-command.exe` from
/// beside this `lords.exe` (`exe`), which must be there. An embedded client gets none of them.
fn window_files(client: Client, exe: &Path) -> Result<Vec<Sent>, String> {
    if client == Client::Embedded {
        return Ok(Vec::new());
    }
    let window = exe.with_file_name("lords-command.exe");
    if fsguard::kind(&window)? != fsguard::Kind::File {
        return Err(format!(
            "{}: no lords-command.exe beside this lords.exe; build both first",
            window.display()
        ));
    }
    let bytes = fsguard::read_bytes(&window).map_err(|e| format!("{}: {e}", window.display()))?;
    Ok(vec![
        ("README.md", README.as_bytes().to_vec(), STEP),
        ("LICENSE", LICENSE.as_bytes().to_vec(), STEP),
        ("lords-command.exe", bytes, SEND_EXE),
    ])
}

/// What a run sends, in this order: its parameters (`panel.json`, whose `embedded` tells install.ps1 to leave the
/// window's files and Start-menu entry alone), the three scripts, the window's files (`window`), the token last.
fn payload(plan: &Plan, run: &str, window: Vec<Sent>, token: &http::Token) -> Vec<Sent> {
    let params = serde_json::json!({
        "run": run, "alias": plan.alias, "key": plan.key, "comment": plan.comment, "block": plan.block,
        "config_before": plan.config_before, "config_txt": plan.config_txt, "local_port": LOCAL_PORT,
        "embedded": plan.client == Client::Embedded,
    });
    let mut files = vec![
        ("panel.json", params.to_string().into_bytes(), STEP),
        ("install.ps1", INSTALL.as_bytes().to_vec(), STEP),
        ("undo.ps1", UNDO.as_bytes().to_vec(), STEP),
        ("check.ps1", CHECK.as_bytes().to_vec(), STEP),
    ];
    files.extend(window);
    files.push((panel::TOKEN_FILE, token.secret().as_bytes().to_vec(), STEP));
    files
}

/// After the other PC's install: this PC's side (the key's line, the port permitted, the listener restarted, the
/// task), then the route checked from the other PC. Nothing here changes the other PC.
#[allow(clippy::too_many_arguments)]
fn finish(
    alias: &str,
    local: &Local,
    plan: &Plan,
    dir: &Contained,
    run: &str,
    installed: &serde_json::Value,
    exe: &Path,
    token: &http::Token,
) -> Result<bool, String> {
    for d in installed["done"].as_array().into_iter().flatten().filter_map(|d| d.as_str()) {
        say!("{alias}: {d}");
    }
    let public =
        installed["public"].as_str().ok_or(format!("{alias}'s install gave no public key"))?;

    // 2. This PC: the key's one line, then the port permitted and the listener restarted, sshd -t first.
    let read =
        |name: &str| fsguard::read_text(&local.bridge.join(name)).map(Option::unwrap_or_default);
    let keys_before = read("authorized_keys")?;
    if let Some(new) =
        add::authorized_with(&keys_before, &add::authorized_line(public, &plan.comment)?)?
    {
        change_bridge_file(&local.bridge, "authorized_keys", &keys_before, &new)?;
    }
    if plan.sshd_config.is_some() {
        let before = read("sshd_config")?;
        if let Some(new) = add::sshd_config_with(&before)? {
            change_bridge_file(&local.bridge, "sshd_config", &before, &new)?;
            let checked = process::sshd_check(
                &local.bridge.join("sshd.exe"),
                &local.bridge.join("sshd_config"),
                HERE,
            )?;
            if !checked.status.success() {
                let root = fsguard::Root::bridge(&local.bridge)?;
                fsguard::replace_file_if(
                    &root.join(&[Part::new("sshd_config")?]),
                    new.as_bytes(),
                    before.as_bytes(),
                )?;
                return Err(format!(
                    "sshd -t refused the new sshd_config, which was put back: {}",
                    String::from_utf8_lossy(&checked.stderr).trim()
                ));
            }
            restart_listener(local)?;
        }
    }

    // 3. The task that serves the panel at the owner's logon, run once now if nothing serves yet.
    if plan.create_task {
        let user = format!(
            "{}\\{}",
            std::env::var("USERDOMAIN").unwrap_or_default(),
            std::env::var("USERNAME").unwrap_or_default()
        );
        let text =
            TASK_XML.replace("{user}", &xml(&user)).replace("{exe}", &xml(&exe.to_string_lossy()));
        let mut utf16 = vec![0xFF, 0xFE];
        utf16.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
        let file = put(dir, "task.xml", &utf16)?;
        let made =
            process::schtasks(&["/Create", "/TN", TASK, "/XML", &file.to_string_lossy()], HERE)?;
        if !made.status.success() {
            return Err(format!(
                "schtasks /Create {TASK}: {}",
                String::from_utf8_lossy(&made.stderr).trim()
            ));
        }
        say!("made the scheduled task {TASK}");
    }
    if plan.run_task {
        let ran = process::schtasks(&["/Run", "/TN", TASK], HERE)?;
        if !ran.status.success() {
            return Err(format!(
                "schtasks /Run {TASK}: {}",
                String::from_utf8_lossy(&ran.stderr).trim()
            ));
        }
        wait("127.0.0.1:7879 to answer", || answers(token))?;
        say!("{TASK} serves 127.0.0.1:{SERVER_PORT}");
    }

    // 4. The route, checked from the other PC through the new forward, with the token's copy there.
    let checked: serde_json::Value =
        serde_json::from_str(&remote(alias, CHECK_STEP, STEP, run, "check", None)?)
            .map_err(|e| format!("{alias}'s check said no JSON: {e}"))?;
    if checked["ok"] != true {
        return Err(format!("{alias}'s check through {}: {checked}", plan.alias));
    }
    say!(
        "{alias}: GET 127.0.0.1:{LOCAL_PORT}/status through {} answered {}",
        plan.alias,
        checked["status"]
    );
    Ok(true)
}

/// Waits up to `HERE` for `done`, checking twice a second.
fn wait(what: &str, done: impl Fn() -> bool) -> Result<(), String> {
    let start = Instant::now();
    while !done() {
        if start.elapsed() >= HERE {
            return Err(format!("no {what} within {} s", HERE.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Ok(())
}

/// Stops the bridge's listener alone, by its PID (its image checked), and waits for its supervisor to start another.
fn restart_listener(local: &Local) -> Result<(), String> {
    let old = local.listener.ok_or("the bridge's listener isn't running")?;
    let sshd = local.bridge.join("sshd.exe");
    process::stop_one(old, &sshd)?;
    say!(
        "stopped the bridge's listener (PID {old}); its {} live session(s) stay",
        local.sessions.len()
    );
    wait("new listener from the bridge's supervisor", || {
        read_local_listener(&local.bridge).is_some_and(|p| p != old)
    })?;
    say!("the bridge's supervisor started a new listener");
    Ok(())
}

fn read_local_listener(bridge: &Path) -> Option<u32> {
    let text = fsguard::read_text(&bridge.join("status.json")).ok()??;
    let status: serde_json::Value =
        serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
    let pid =
        status.as_array()?.iter().find(|c| c["Component"] == "Sshd" && c["Running"] == true)?
            ["ProcessId"]
            .as_u64()? as u32;
    let sshd = bridge.join("sshd.exe");
    process::running()
        .ok()?
        .iter()
        .any(|r| r.pid == pid && same_path(&r.image, &sshd))
        .then_some(pid)
}

// After the steps: the macro is in scope for the tests by its place in this file.
#[cfg(test)]
mod tests;
