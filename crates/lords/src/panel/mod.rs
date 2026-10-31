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
use add::{Local, Plan, Remote, LOCAL_PORT, SERVER_PORT, TASK};
use lords_core::fsguard::{self, Contained, Part};
use lords_core::{http, panel, process, sync};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const USAGE: &str =
    "usage: lords panel serve [--port N] | lords panel add ALIAS --bridge DIR [--apply]";

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
        Some(p) => p.parse::<u16>().map_err(|_| format!("--port {p}: a port number"))?,
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
/// (fix check r1 R2). `Open-Path` says why a dir and every dir above it up to the profile aren't closed to other
/// accounts, or nothing: the first that lets an account other than this one, SYSTEM and Administrators (and the owner
/// placeholders, CREATOR OWNER and OWNER RIGHTS) create, delete, delete what it holds, or change its permissions or
/// owner, as an entry or as its owner (fix check r2 N4: the panel's threat model); or the first that is a link (a
/// junction, symlink or mount point: the dir behind it, and what may change it, the walk can't see), never followed
/// (fix check r3 N5). A dir's attributes are read without following it, so a link to nothing counts too. Static: the
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
            "function Open-Path($p) { $top = $env:USERPROFILE.TrimEnd('\\'); $p = $p.TrimEnd('\\'); ",
            "if (-not $p.StartsWith($top + '\\', [StringComparison]::OrdinalIgnoreCase)) { ",
            "return ($p + ' is outside the user profile ' + $top) }; while ($true) { $at = $null; ",
            "try { $at = [IO.File]::GetAttributes($p) } catch [IO.FileNotFoundException], [IO.DirectoryNotFoundException] {}; ",
            "if ($null -ne $at) { if ($at -band [IO.FileAttributes]::ReparsePoint) { return ($p + ' is a link (a junction, ",
            "symlink or mount point): the dir behind it is not checked') }; ",
            "$who = Open-To $p; if ($who) { $n = $who; try { $n = (New-Object ",
            "Security.Principal.SecurityIdentifier($who)).Translate([Security.Principal.NTAccount]).Value + ' (' + ",
            "$who + ')' } catch {}; return ($p + ' lets ' + $n + ' create, delete or change permissions there') } }; ",
            "if ($p -ieq $top) { return }; $p = Split-Path -Parent $p } }; ",
            $body,
            " } finally { taskkill /T /F /PID $watch.Id 2>&1 | Out-Null } }"
        )
    };
}

/// The probe: it reads, writes nothing, and reads no private key (only `.pub` files). `open_to`: who else may change
/// the install path (fix check r2 N4).
const PROBE: &str = step!("$h = $env:USERPROFILE; $s = Join-Path $h '.ssh'; $a = Join-Path $env:LOCALAPPDATA 'Programs\\lords-command'; $c = Join-Path $s 'config'; $t = Join-Path $a 'config.txt'; [ordered]@{ open_to = @(Open-Path $a); home = $h; local_app_data = $env:LOCALAPPDATA; app_data = $env:APPDATA; ssh = [bool](Get-Command ssh.exe -ErrorAction SilentlyContinue); keygen = [bool](Get-Command ssh-keygen.exe -ErrorAction SilentlyContinue); ssh_config = $(if (Test-Path -LiteralPath $c) { [IO.File]::ReadAllText($c) } else { $null }); keys = @(Get-ChildItem -LiteralPath $s -Filter 'lords_panel_*.pub' -ErrorAction SilentlyContinue | ForEach-Object { [ordered]@{ name = $_.Name; text = [IO.File]::ReadAllText($_.FullName) } }); app = (Test-Path -LiteralPath (Join-Path $a 'lords-command.exe')); config_txt = $(if (Test-Path -LiteralPath $t) { [IO.File]::ReadAllText($t) } else { $null }); running = @(Get-Process lords-command -ErrorAction SilentlyContinue | Where-Object { $_.Path -like ($a + '\\*') }).Count; listening = @(Get-NetTCPConnection -State Listen -LocalPort 17879 -ErrorAction SilentlyContinue).Count } | ConvertTo-Json -Compress -Depth 4");
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
const CHECK_STEP: &str = step!("& (Join-Path $staging 'check.ps1')");
/// This run's staging dir, which this run made (STAGE refuses one that is there): only what this run created goes.
const CLEAN: &str = step!("Remove-Item -Recurse -Force -LiteralPath $staging; 'cleaned'");

const INSTALL: &str = include_str!("install.ps1");
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

fn probe(alias: &str, run: &str) -> Result<Remote, String> {
    let line = remote(alias, PROBE, STEP, run, "probe", None)
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

/// `lords panel add ALIAS --bridge DIR [--apply]`.
fn add_pc(mut a: Args) -> Result<bool, String> {
    let apply = a.flag("--apply");
    let bridge = a.value("--bridge")?.map(PathBuf::from).ok_or(
        "panel add needs --bridge DIR: this PC's SSH bridge (its sshd_config, authorized_keys, status.json, sshd.exe)",
    )?;
    let alias =
        a.positional().ok_or("panel add needs the SSH alias this PC reaches the other PC by")?;
    a.done()?;
    lords_core::config::check_alias(&alias)?;
    let home_dir = sync::default_home()?;
    let run = run_id();
    let local = read_local(&bridge, &home_dir)?;
    let other = probe(&alias, &run)?;
    let plan = add::plan(&alias, &local, &other)?;
    for line in plan.describe(&alias, &local, &other, &run) {
        say!("{line}");
    }
    if !apply {
        say!("dry run: nothing written (`--apply` carries these out, in this order)");
        return Ok(true);
    }
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
    let window = exe.with_file_name("lords-command.exe");
    if fsguard::kind(&window)? != fsguard::Kind::File {
        return Err(format!(
            "{}: no lords-command.exe beside this lords.exe; build both first",
            window.display()
        ));
    }
    if exe.to_string_lossy().contains('\'') {
        return Err(format!("{}: a path with `'` can't go in the task's command", exe.display()));
    }
    // The panel's token, made here once if there is none yet; a copy goes to the other PC over ssh, from memory.
    let token = panel::token(&local.home)?;
    // 1. The other PC: this run's staging dir, each file into it over stdin, then installed from it (the key made
    // there, the alias added, each installed file backed up first), the route checked; the staging dir removed at the
    // end, whatever happened.
    remote(alias, STAGE, STEP, run, "staging", None)?;
    say!("{alias}: made this run's staging dir");
    let sent = send_and_install(alias, local, plan, dir, run, &window, &exe, &token);
    let cleaned = remote(alias, CLEAN, STEP, run, "staging", None);
    let checked = sent?;
    cleaned.map_err(|e| format!("{e} (this run's staging dir stays on {alias})"))?;
    say!("{alias}: removed this run's staging dir");
    Ok(checked)
}

#[allow(clippy::too_many_arguments)]
fn send_and_install(
    alias: &str,
    local: &Local,
    plan: &Plan,
    dir: &Contained,
    run: &str,
    window: &Path,
    exe: &Path,
    token: &http::Token,
) -> Result<bool, String> {
    let params = serde_json::json!({
        "run": run, "alias": plan.alias, "key": plan.key, "comment": plan.comment, "block": plan.block,
        "config_before": plan.config_before, "config_txt": plan.config_txt, "local_port": LOCAL_PORT,
    });
    let exe_bytes =
        fsguard::read_bytes(window).map_err(|e| format!("{}: {e}", window.display()))?;
    let files: [(&str, Vec<u8>, u64); 7] = [
        ("panel.json", params.to_string().into_bytes(), STEP),
        ("install.ps1", INSTALL.as_bytes().to_vec(), STEP),
        ("check.ps1", CHECK.as_bytes().to_vec(), STEP),
        ("README.md", README.as_bytes().to_vec(), STEP),
        ("LICENSE", LICENSE.as_bytes().to_vec(), STEP),
        ("lords-command.exe", exe_bytes, SEND_EXE),
        (panel::TOKEN_FILE, token.secret().as_bytes().to_vec(), STEP),
    ];
    for (name, bytes, deadline) in files {
        remote(alias, RECEIVE, deadline, run, name, Some(bytes))?;
    }
    say!("{alias}: sent this run's files");
    let installed: serde_json::Value =
        serde_json::from_str(&remote(alias, INSTALL_STEP, STEP, run, "install", None)?)
            .map_err(|e| format!("{alias}'s install said no JSON: {e}"))?;
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
