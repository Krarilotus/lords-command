//! `lords panel serve` and `lords panel add` (design note 2026-10-31-lords-panel-remote, P1, P3, P4).
//!
//! `serve` answers the window on another PC (through an SSH forward) with this PC's stacks: `panel::handle` on
//! 127.0.0.1 alone. `add` sets a benchmon PC up for it, from this PC: without `--apply` it reads both PCs and prints
//! the exact targets; with it, it carries them out in that order, each change backed up first and made only over what
//! was read. Every remote call has a deadline here, and each script it runs there stops itself; no key material is
//! read or copied: the key is made on the other PC and only its `.pub` comes here.

mod add;

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
    let listener = http::bind(port)
        .map_err(|e| format!("127.0.0.1:{port}: {e} (does a panel serve there already?)"))?;
    let bound = listener.local_addr().map_err(|e| e.to_string())?;
    println!("lords panel: serving the stacks on {bound}");
    http::serve(listener, move |r| panel::handle(&paths, r));
    Ok(true)
}

/// How long a remote step may take here (each script there stops itself sooner), a copy of the exe, and a check on
/// this PC.
const REMOTE: Duration = Duration::from_secs(60);
const COPY: Duration = Duration::from_secs(300);
const HERE: Duration = Duration::from_secs(20);

/// What the probe runs on the other PC: one line of PowerShell, its own deadline 20 s. It reads and writes nothing
/// else, and no private key (only `.pub` files). No double quote and no `%` in it: cmd.exe, ssh's shell there, would
/// read them.
const PROBE: &str = "$j = Start-Job -ScriptBlock { $h = $env:USERPROFILE; $s = Join-Path $h '.ssh'; $a = Join-Path $env:LOCALAPPDATA 'Programs\\lords-command'; $c = Join-Path $s 'config'; $t = Join-Path $a 'config.txt'; [ordered]@{ home = $h; local_app_data = $env:LOCALAPPDATA; app_data = $env:APPDATA; ssh = [bool](Get-Command ssh.exe -ErrorAction SilentlyContinue); keygen = [bool](Get-Command ssh-keygen.exe -ErrorAction SilentlyContinue); ssh_config = $(if (Test-Path -LiteralPath $c) { [IO.File]::ReadAllText($c) } else { $null }); keys = @(Get-ChildItem -LiteralPath $s -Filter 'lords_panel_*.pub' -ErrorAction SilentlyContinue | ForEach-Object { [ordered]@{ name = $_.Name; text = [IO.File]::ReadAllText($_.FullName) } }); app = (Test-Path -LiteralPath (Join-Path $a 'lords-command.exe')); config_txt = $(if (Test-Path -LiteralPath $t) { [IO.File]::ReadAllText($t) } else { $null }); running = @(Get-Process lords-command -ErrorAction SilentlyContinue | Where-Object { $_.Path -like ($a + '\\*') }).Count; listening = @(Get-NetTCPConnection -State Listen -LocalPort 17879 -ErrorAction SilentlyContinue).Count } | ConvertTo-Json -Compress -Depth 4 }; if (Wait-Job $j -Timeout 20) { Receive-Job $j } else { 'timed out' }; Remove-Job $j -Force";

/// The dir panel add copies into on the other PC, relative to its home (the probe checks LOCALAPPDATA is there).
const INCOMING: &str = "AppData/Local/Programs/lords-command/incoming";
const MAKE_INCOMING: &str = "New-Item -ItemType Directory -Force -Path (Join-Path $env:LOCALAPPDATA 'Programs\\lords-command\\incoming') | Out-Null";

const INSTALL: &str = include_str!("install.ps1");
const CHECK: &str = include_str!("check.ps1");
const TASK_XML: &str = include_str!("task.xml");
const README: &str = include_str!("../../../../README.md");
const LICENSE: &str = include_str!("../../../../LICENSE");

/// `powershell … -Command "<line>"` there: the one line, quoted for cmd.exe.
fn powershell_line(line: &str) -> Vec<String> {
    ["powershell", "-NoProfile", "-NonInteractive", "-Command"]
        .iter()
        .map(|s| s.to_string())
        .chain([format!("\"{line}\"")])
        .collect()
}

/// `powershell … -File` of a script panel add copied into the incoming dir there.
fn powershell_file(script: &str) -> Vec<String> {
    let path = format!("\"%LOCALAPPDATA%\\Programs\\lords-command\\incoming\\{script}\"");
    ["powershell", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"]
        .iter()
        .map(|s| s.to_string())
        .chain([path])
        .collect()
}

/// Runs `command` there and gives its last line of output, or why not.
fn remote(alias: &str, command: &[String], deadline: Duration) -> Result<String, String> {
    let args: Vec<&str> = command.iter().map(String::as_str).collect();
    let out = process::ssh(alias, &args, None, deadline)?;
    let said = String::from_utf8_lossy(&out.stdout);
    let last = said.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
        return Err(format!(
            "on {alias}: {} ({}) {err}",
            command.join(" ").chars().take(60).collect::<String>(),
            out.status
        ));
    }
    if last == "timed out" {
        return Err(format!("on {alias}: the script stopped itself at its deadline"));
    }
    Ok(last)
}

fn probe(alias: &str) -> Result<Remote, String> {
    let line = remote(alias, &powershell_line(PROBE), REMOTE)
        .map_err(|e| format!("{alias} can't be read over ssh: {e}"))?;
    serde_json::from_str(line.trim_start_matches('\u{feff}')).map_err(|e| {
        format!("{alias}'s probe said `{}`: {e}", line.chars().take(80).collect::<String>())
    })
}

fn same_path(a: &Path, b: &Path) -> bool {
    let key = |p: &Path| p.to_string_lossy().replace('/', "\\").to_lowercase();
    key(a) == key(b)
}

/// This PC's side: the bridge's files, its listener and sessions, the task, and whether the server answers.
fn read_local(bridge: &Path) -> Result<Local, String> {
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
    let served = http::Client::new(SERVER_PORT)
        .call("GET", "/status", None, Duration::from_secs(5))
        .is_ok_and(|(s, _)| s == 200);
    Ok(Local {
        bridge: bridge.to_path_buf(),
        sshd_config,
        authorized_keys: read("authorized_keys")?.unwrap_or_default(),
        listener,
        sessions,
        task,
        served,
    })
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
    let local = read_local(&bridge)?;
    let other = probe(&alias)?;
    let plan = add::plan(&alias, &local, &other)?;
    for line in plan.describe(&alias, &local, &other) {
        println!("{line}");
    }
    if !apply {
        println!("dry run: nothing written (`--apply` carries these out, in this order)");
        return Ok(true);
    }
    // The files for the other PC go through a dir of this run's own in the tool's home, removed at the end.
    let home = fsguard::Root::home(&sync::default_home()?)?;
    let dir = home.join(&[Part::new(&format!("panel-add-{}", process::id()))?]);
    fsguard::create_dir_all(&dir)?;
    let done = carry_out(&alias, &local, &plan, &dir);
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
        println!("backed up {}", backup.path().display());
    }
    if fsguard::kind(file.path())? == fsguard::Kind::Missing {
        fsguard::create_file(&file, new.as_bytes())?;
    } else {
        fsguard::replace_file_if(&file, was.as_bytes(), new.as_bytes())?;
    }
    println!("wrote {}", file.path().display());
    Ok(())
}

fn carry_out(alias: &str, local: &Local, plan: &Plan, dir: &Contained) -> Result<bool, String> {
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
    // 1. The other PC: its files copied in, then installed there (the key made there, the alias added).
    let params = serde_json::json!({
        "alias": plan.alias, "key": plan.key, "comment": plan.comment, "block": plan.block,
        "config_before": plan.config_before, "config_txt": plan.config_txt, "local_port": LOCAL_PORT,
    });
    let files = [
        ("panel.json", put(dir, "panel.json", params.to_string().as_bytes())?),
        ("install.ps1", put(dir, "install.ps1", INSTALL.as_bytes())?),
        ("check.ps1", put(dir, "check.ps1", CHECK.as_bytes())?),
        ("README.md", put(dir, "README.md", README.as_bytes())?),
        ("LICENSE", put(dir, "LICENSE", LICENSE.as_bytes())?),
        ("lords-command.exe", window),
    ];
    remote(alias, &powershell_line(MAKE_INCOMING), REMOTE)?;
    for (name, from) in &files {
        let out = process::scp(from, alias, &format!("{INCOMING}/{name}"), COPY)?;
        if !out.status.success() {
            return Err(format!(
                "scp {name} to {alias}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
    }
    let installed: serde_json::Value =
        serde_json::from_str(&remote(alias, &powershell_file("install.ps1"), REMOTE)?)
            .map_err(|e| format!("{alias}'s install said no JSON: {e}"))?;
    for d in installed["done"].as_array().into_iter().flatten().filter_map(|d| d.as_str()) {
        println!("{alias}: {d}");
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
        println!("made the scheduled task {TASK}");
    }
    if plan.run_task {
        let ran = process::schtasks(&["/Run", "/TN", TASK], HERE)?;
        if !ran.status.success() {
            return Err(format!(
                "schtasks /Run {TASK}: {}",
                String::from_utf8_lossy(&ran.stderr).trim()
            ));
        }
        wait("127.0.0.1:7879 to answer", || {
            http::Client::new(SERVER_PORT)
                .call("GET", "/status", None, Duration::from_secs(3))
                .is_ok_and(|(s, _)| s == 200)
        })?;
        println!("{TASK} serves 127.0.0.1:{SERVER_PORT}");
    }

    // 4. The route, checked from the other PC through the new forward.
    let checked: serde_json::Value =
        serde_json::from_str(&remote(alias, &powershell_file("check.ps1"), REMOTE)?)
            .map_err(|e| format!("{alias}'s check said no JSON: {e}"))?;
    if checked["ok"] != true {
        return Err(format!("{alias}'s check through {}: {checked}", plan.alias));
    }
    println!(
        "{alias}: GET 127.0.0.1:{LOCAL_PORT}/status through {} answered {}",
        plan.alias, checked["status"]
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
    println!(
        "stopped the bridge's listener (PID {old}); its {} live session(s) stay",
        local.sessions.len()
    );
    wait("new listener from the bridge's supervisor", || {
        read_local_listener(&local.bridge).is_some_and(|p| p != old)
    })?;
    println!("the bridge's supervisor started a new listener");
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
