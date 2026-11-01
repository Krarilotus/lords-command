//! `lords panel add`'s remote steps, run here: the same PowerShell the other PC runs, in a local powershell with its
//! `LOCALAPPDATA`, `USERPROFILE` and `APPDATA` in a temp dir of the test's own, and the OS's profiles directory (HKLM,
//! which a test can't change) answered by the test. What ssh adds (the transport, the other PC's shell) is not run here.

// Tests are outside the filesystem and process boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]
#![cfg(windows)]

use super::*;
use std::io::Write;
use std::process::{Command, Stdio};

/// Runs `step` as the other PC would (its arguments after it), with `stdin`, `home` its profile and its
/// `LOCALAPPDATA`, in a profiles directory that is `home`'s parent; its status, output and time.
fn run(
    step: &str,
    deadline: u64,
    name: &str,
    home: &Path,
    stdin: &[u8],
) -> (bool, String, Duration) {
    run_with(step, deadline, name, [home, home], home.parent(), None, stdin)
}

/// `run`, with `LOCALAPPDATA` and `USERPROFILE` as given, and the OS's profiles directory answered as `profiles`. The
/// step reads it with `Get-ItemProperty` from HKLM; here a function of that name, which PowerShell finds before the
/// cmdlet, answers instead (`-NoProfile` keeps any other out on the other PC). `None`: the registry answers.
/// `ssh`: a dir whose `ssh.exe` the step runs instead of the OS's, and the step held after it starts each ssh until
/// that ssh ends (a `Start-Process` function of the same kind), as if its powershell got no time meanwhile.
fn run_with(
    step: &str,
    deadline: u64,
    name: &str,
    [local_app_data, profile]: [&Path; 2],
    profiles: Option<&Path>,
    ssh: Option<&Path>,
    stdin: &[u8],
) -> (bool, String, Duration) {
    let os = match profiles {
        Some(_) => "function Get-ItemProperty { [pscustomobject]@{ ProfilesDirectory = $env:TEST_PROFILES } }; ",
        None => "",
    };
    let held = match ssh {
        Some(_) => "function Start-Process { param($FilePath, $WindowStyle, [switch]$PassThru, $ArgumentList) $c = Microsoft.PowerShell.Management\\Start-Process -FilePath $FilePath -WindowStyle $WindowStyle -PassThru -ArgumentList $ArgumentList; if ($FilePath -eq 'ssh.exe') { $c.WaitForExit() }; $c }; ",
        None => "",
    };
    let start = Instant::now();
    let mut command = Command::new("powershell");
    if let Some(dir) = profiles {
        command.env("TEST_PROFILES", dir);
    }
    if let Some(dir) = ssh {
        let path = std::env::var("PATH").unwrap_or_default();
        command.env("PATH", format!("{};{path}", dir.display()));
    }
    let mut child = command
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command"])
        .arg(format!("{os}{held}{step}"))
        .args([deadline.to_string().as_str(), "run-1", name])
        .env("LOCALAPPDATA", local_app_data)
        .env("USERPROFILE", profile)
        // The Start menu's place: never the real one.
        .env("APPDATA", roaming(profile))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut pipe = child.stdin.take().unwrap();
    pipe.write_all(stdin).unwrap();
    drop(pipe);
    let out = child.wait_with_output().unwrap();
    let said =
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    (out.status.success(), said, start.elapsed())
}

/// The `APPDATA` a step gets in `profile`.
fn roaming(profile: &Path) -> PathBuf {
    profile.join("AppData").join("Roaming")
}

fn alive(pid: u32) -> bool {
    lords_core::process::running().unwrap().iter().any(|r| r.pid == pid)
}

/// `path`'s access list as icacls prints it, lower case, one `account:(rights)` per entry; an inherited entry carries
/// `(i)`. Account names are the OS's language, so they are compared with `whoami`'s.
fn entries(path: &Path) -> Vec<String> {
    let out = Command::new("icacls").arg(path).output().unwrap();
    let shown = path.to_string_lossy().to_lowercase();
    String::from_utf8_lossy(&out.stdout)
        .to_lowercase()
        .lines()
        .filter(|l| l.contains(":("))
        .map(|l| l.trim().trim_start_matches(shown.as_str()).trim().to_string())
        .collect()
}

fn me() -> String {
    let out = Command::new("whoami").output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_lowercase()
}

/// A temp home as a profile is: this account, SYSTEM and Administrators, nothing inherited from the temp dir (whose
/// list grants other accounts here).
fn profile() -> tempfile::TempDir {
    let home = tempfile::TempDir::new().unwrap();
    let me = format!("{}:(OI)(CI)F", me());
    let set = Command::new("icacls")
        .arg(home.path())
        .args(["/inheritance:r", "/grant:r", "*S-1-5-18:(OI)(CI)F", "*S-1-5-32-544:(OI)(CI)F"])
        .arg(&me)
        .output()
        .unwrap();
    assert!(set.status.success(), "{}", String::from_utf8_lossy(&set.stdout));
    home
}

/// A step that starts a child (`ping`, a minute long), writes its pid into `$name`, and waits a minute.
const STUCK: &str = step!("$c = Start-Process ping -ArgumentList '-n','60','127.0.0.1' -WindowStyle Hidden -PassThru; [IO.File]::WriteAllText((Join-Path $env:LOCALAPPDATA $name), [string]$c.Id); Start-Sleep -Seconds 60");
/// A step that ends at once, saying its watchdog's pid.
const QUICK: &str = step!("[string]$watch.Id");

// Spec (review r1 R6, the Lords owner: every remote step runs under a remote-side watchdog that kills its own process
// tree): a step stuck past its deadline is ended there, by its deadline and not by this PC's wait, with everything it
// started; one that ends on its own ends its watchdog too, so nothing of the step is left running.
#[test]
fn a_step_is_ended_at_its_deadline_with_all_it_started() {
    let home = tempfile::TempDir::new().unwrap();
    let (ok, said, took) = run(STUCK, 3, "child.pid", home.path(), b"");
    assert!(!ok, "a stuck step fails: {said}");
    assert!(took < Duration::from_secs(30), "ended near its 3 s, not after its 60 s: {took:?}");
    let child: u32 =
        std::fs::read_to_string(home.path().join("child.pid")).unwrap().trim().parse().unwrap();
    let start = Instant::now();
    while alive(child) && start.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(!alive(child), "the step's child ({child}) ends with it");

    let (ok, said, _) = run(QUICK, 60, "none", home.path(), b"");
    assert!(ok, "{said}");
    let watchdog: u32 = said.lines().find_map(|l| l.trim().parse().ok()).unwrap();
    let start = Instant::now();
    while alive(watchdog) && start.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(!alive(watchdog), "the watchdog ({watchdog}) ends with the step");
}

// Spec (review r1 R6 and R7: transfers run under the same watchdog, into a staging dir of the run's own; cleanup takes
// only what the run created): this run's staging dir is made only where nothing is; a file sent on stdin arrives byte
// for byte and is never written over; the run's cleanup removes its dir and nothing beside it.
#[test]
fn files_reach_the_runs_own_staging_dir_byte_for_byte_and_only_it_is_removed() {
    let home = profile();
    let programs = home.path().join("Programs").join("lords-command");
    std::fs::create_dir_all(&programs).unwrap();
    std::fs::write(programs.join("beside.txt"), "not this run's").unwrap();
    assert!(run(STAGE, 30, "staging", home.path(), b"").0);
    let (again, said, _) = run(STAGE, 30, "staging", home.path(), b"");
    assert!(!again && said.contains("there already"), "{said}");
    let bytes: Vec<u8> = (0..=255u8).chain(*b"\r\n\x00end").collect();
    let (ok, said, _) = run(RECEIVE, 30, "payload.bin", home.path(), &bytes);
    assert!(ok, "{said}");
    let staged = programs.join("staging").join("run-1");
    assert_eq!(std::fs::read(staged.join("payload.bin")).unwrap(), bytes);
    let (over, said, _) = run(RECEIVE, 30, "payload.bin", home.path(), b"other");
    assert!(!over, "never written over: {said}");
    assert_eq!(std::fs::read(staged.join("payload.bin")).unwrap(), bytes);
    assert!(run(CLEAN, 30, "staging", home.path(), b"").0);
    assert!(!staged.exists());
    assert!(programs.join("beside.txt").exists(), "nothing beside it");
}

// Spec (fix check r1 R2, the Lords owner: the run's staging dir is owner-only from its creation, its protected list set
// while it is empty, before any secret byte arrives; a destination that grants anyone but its owner is refused; the
// token is written only inside it): the run's dir has one entry, this account's, for itself and what it holds, nothing
// from its parent; a file received there has only that entry; a staging dir that grants others (one made as any dir
// is, inheriting its parent's list) gets nothing written into it.
#[test]
fn the_runs_staging_dir_is_owner_only_from_its_creation_and_a_wider_one_gets_nothing() {
    let home = profile();
    let (ok, said, _) = run(STAGE, 30, "staging", home.path(), b"");
    assert!(ok, "{said}");
    let staged = home.path().join("Programs").join("lords-command").join("staging").join("run-1");
    let me = me();
    assert_eq!(entries(&staged), [format!("{me}:(oi)(ci)(f)")], "protected, this account only");
    let (ok, said, _) = run(RECEIVE, 30, "panel-token", home.path(), b"0123456789abcdef");
    assert!(ok, "{said}");
    assert_eq!(entries(&staged.join("panel-token")), [format!("{me}:(i)(f)")]);

    let wide = tempfile::TempDir::new().unwrap();
    let dir = wide.path().join("Programs").join("lords-command").join("staging").join("run-1");
    std::fs::create_dir_all(&dir).unwrap();
    assert!(entries(&dir).len() > 1, "the fixture's dir grants others: {:?}", entries(&dir));
    let (ok, said, _) = run(RECEIVE, 30, "panel-token", wide.path(), b"0123456789abcdef");
    assert!(!ok && said.contains("grants others"), "{said}");
    assert!(!dir.join("panel-token").exists(), "nothing written");
}

// Spec (fix check r1 R2, the Lords owner: the token keeps its owner-only list in the app dir only if its move there is
// a rename on one volume; across volumes a move copies, and the copy takes the app dir's list): a staging dir reached
// through a link (a junction, a mount point: the one way it can lie on another volume than the app dir it sits in) is
// refused before anything is made behind it.
#[test]
fn a_staging_dir_behind_a_link_is_refused_before_anything_is_made() {
    let home = profile();
    let app = home.path().join("Programs").join("lords-command");
    let elsewhere = home.path().join("elsewhere");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();
    let link = Command::new("cmd")
        .args(["/c", "mklink", "/J"])
        .arg(app.join("staging"))
        .arg(&elsewhere)
        .output()
        .unwrap();
    assert!(link.status.success(), "{}", String::from_utf8_lossy(&link.stdout));
    let (ok, said, _) = run(STAGE, 30, "staging", home.path(), b"");
    assert!(!ok && said.contains("a link"), "{said}");
    assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0, "nothing made behind it");
}

// Spec (fix check r2 N4, the Lords owner: the panel's threat model is the owner's own account and no other non-admin
// account with rights to change the install path; `panel add` refuses an install target unless the app dir and every
// dir above it up to the user profile let only the owner, SYSTEM and Administrators create, delete or change
// permissions there; the probe says it, STAGE checks it again): with a parent that grants another account
// FullControl, the probe names that dir and account and STAGE refuses before it makes anything; with only those
// three, the probe names nothing.
#[test]
fn an_install_path_another_account_may_change_is_named_and_refused() {
    let home = profile();
    let programs = home.path().join("Programs");
    std::fs::create_dir_all(programs.join("lords-command")).unwrap();
    let (ok, said, _) = run(PROBE, 30, "none", home.path(), b"");
    assert!(ok && said.contains("\"open_to\":[]"), "{said}");
    let grant = Command::new("icacls")
        .arg(&programs)
        .args(["/grant", "*S-1-5-32-545:(OI)(CI)F"])
        .output()
        .unwrap();
    assert!(grant.status.success(), "{}", String::from_utf8_lossy(&grant.stdout));
    let (ok, said, _) = run(PROBE, 30, "none", home.path(), b"");
    assert!(ok && said.contains("S-1-5-32-545"), "{said}");
    let (ok, said, _) = run(STAGE, 30, "staging", home.path(), b"");
    assert!(!ok && said.contains("S-1-5-32-545"), "{said}");
    assert!(!programs.join("lords-command").join("staging").exists(), "nothing made");
}

// Spec (fix check r3 N5, the Lords owner: a link among the install path's dirs hides the dir that really holds what is
// below it, whose rights the walk never sees; it is refused, fail closed, without following it): with `Programs` a
// junction (the review's case), the probe names that dir as a link, and STAGE refuses before it makes anything
// behind it.
#[test]
fn an_install_path_through_a_link_is_named_and_refused() {
    let home = profile();
    let backing = home.path().join("backing");
    std::fs::create_dir_all(backing.join("lords-command")).unwrap();
    let link = Command::new("cmd")
        .args(["/c", "mklink", "/J"])
        .arg(home.path().join("Programs"))
        .arg(&backing)
        .output()
        .unwrap();
    assert!(link.status.success(), "{}", String::from_utf8_lossy(&link.stdout));
    let (ok, said, _) = run(PROBE, 30, "none", home.path(), b"");
    assert!(ok && said.contains("Programs is a link"), "{said}");
    let (ok, said, _) = run(STAGE, 30, "staging", home.path(), b"");
    assert!(!ok && said.contains("is a link"), "{said}");
    assert_eq!(
        std::fs::read_dir(backing.join("lords-command")).unwrap().count(),
        0,
        "nothing made behind it"
    );
}

// Spec (fix check r4 N6, the Lords owner: links are refused on every dir from the app dir up to the drive's root, not
// only up to the profile; the review's witness): a profile reached through a junction above it, `LOCALAPPDATA` and
// `USERPROFILE` agreeing, is named as a link and refused by the probe and by STAGE, even where the profiles directory
// is said to be the profile's parent; nothing is made behind it.
#[test]
fn a_profile_reached_through_a_link_above_it_is_refused() {
    let root = profile();
    let real = root.path().join("real");
    let app = ["PanelOwner", "AppData", "Local", "Programs", "lords-command"];
    std::fs::create_dir_all(app.iter().fold(real.clone(), |p, c| p.join(c))).unwrap();
    std::fs::create_dir_all(root.path().join("routes")).unwrap();
    let current = root.path().join("routes").join("Current");
    let link =
        Command::new("cmd").args(["/c", "mklink", "/J"]).arg(&current).arg(&real).output().unwrap();
    assert!(link.status.success(), "{}", String::from_utf8_lossy(&link.stdout));
    let owner = current.join("PanelOwner");
    let local = owner.join("AppData").join("Local");
    let (ok, said, _) = run_with(PROBE, 30, "none", [&local, &owner], Some(&current), None, b"");
    assert!(ok && said.contains("Current is a link"), "{said}");
    let (ok, said, _) = run_with(STAGE, 30, "staging", [&local, &owner], Some(&current), None, b"");
    assert!(!ok && said.contains("is a link"), "{said}");
    let staging = app.iter().fold(real.clone(), |p, c| p.join(c)).join("staging");
    assert!(!staging.exists(), "nothing made behind it");
}

// Spec (fix check r4 N6, the Lords owner: the profile's parent must be the OS's own profiles directory,
// `ProfilesDirectory` under ProfileList, which the OS protects): a profile elsewhere is named and refused by the probe
// and by STAGE, and nothing is made. Read from this PC's registry itself, the profiles directory is a real one, and a
// temp dir's profile isn't in it.
#[test]
fn a_profile_outside_the_os_profiles_directory_is_refused() {
    let root = profile();
    let owner = root.path().join("elsewhere").join("PanelOwner");
    let local = owner.join("AppData").join("Local");
    let app = local.join("Programs").join("lords-command");
    std::fs::create_dir_all(&app).unwrap();
    let profiles = root.path().join("Users");
    std::fs::create_dir_all(&profiles).unwrap();
    let (ok, said, _) = run_with(PROBE, 30, "none", [&local, &owner], Some(&profiles), None, b"");
    assert!(ok && said.contains("not a profile in the OS profiles directory"), "{said}");
    let (ok, said, _) =
        run_with(STAGE, 30, "staging", [&local, &owner], Some(&profiles), None, b"");
    assert!(!ok && said.contains("not a profile in the OS profiles directory"), "{said}");
    assert!(!app.join("staging").exists(), "nothing made");
    let (ok, said, _) = run_with(PROBE, 30, "none", [&local, &owner], None, None, b"");
    let named =
        said.split("not a profile in the OS profiles directory ").nth(1).unwrap_or_default();
    let dir = named.split('"').next().unwrap_or_default().replace("\\\\", "\\");
    assert!(ok && !dir.is_empty() && Path::new(&dir).is_dir(), "{said}");
}

// Spec (review r1 R6: a step's arguments are data, never code): a run's id or a file's name that isn't a plain word
// never reaches a command line.
#[test]
fn only_plain_words_become_a_steps_arguments() {
    for bad in ["", "-x", "a b", "a;b", "a'b", "$x", "a\"b", "..\\x", ".", ".."] {
        assert!(step_command(QUICK, 30, "run-1", bad).is_err(), "{bad:?}");
        assert!(step_command(QUICK, 30, bad, "name").is_err(), "{bad:?}");
    }
    assert!(step_command(QUICK, 30, "2026-10-31-1793-12", "lords-command.exe").is_ok());
}

/// The other PC's `~/.ssh/config` as the fixtures have it (overseer's route), the panel's alias a plan appends to it,
/// the panel key's `.pub`, and the config.txt a plan writes where there is none.
const SSH_CONFIG: &str = "Host fleet-panel-main\r\n    HostName 127.0.0.1\r\n    IdentityFile ~/.ssh/fleet_panel_main\r\n    LocalForward 127.0.0.1:17878 127.0.0.1:7878\r\n";
const BLOCK: &str = "Host lords-panel-main\r\n    HostName 127.0.0.1\r\n    IdentityFile ~/.ssh/lords_panel_main\r\n    LocalForward 127.0.0.1:17879 127.0.0.1:7879\r\n";
const PUBLIC: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExample0123456789abcdefABCDEF lords-panel-other";
const CONFIG_TXT: &str = "api = http://127.0.0.1:17879\r\ntunnel = lords-panel-main\r\n";

/// A plan for a PC with overseer's route, as `add::plan` makes one; `block`: the alias to append (`None`: it is there).
fn fixture_plan(client: Client, block: Option<&str>) -> Plan {
    Plan {
        client,
        alias: "lords-panel-main".into(),
        base: "fleet-panel-main".into(),
        key: "lords_panel_main".into(),
        comment: "lords-panel-other".into(),
        public: Some(PUBLIC.into()),
        block: block.map(str::to_string),
        config_before: SSH_CONFIG.into(),
        sshd_config: None,
        config_txt: Some(CONFIG_TXT.into()),
        create_task: false,
        run_task: false,
    }
}

/// A fixture token, built at run time so the repo's own privacy check doesn't take this source for one.
fn fixture_token(digit: char) -> http::Token {
    http::Token::new(&digit.to_string().repeat(64)).unwrap()
}

/// Stand-ins for the window's three files.
fn window_stand_ins(exe: &[u8]) -> Vec<Sent> {
    vec![
        ("README.md", b"the README sent".to_vec(), STEP),
        ("LICENSE", b"the LICENSE sent".to_vec(), STEP),
        ("lords-command.exe", exe.to_vec(), STEP),
    ]
}

/// A profile (`profile()`) as the other PC's is before `panel add`: overseer's route in `~/.ssh/config`, and the
/// panel's key there already, so the install makes none (no ssh-keygen runs here).
fn other_pc() -> tempfile::TempDir {
    let home = profile();
    let ssh = home.path().join(".ssh");
    std::fs::create_dir_all(&ssh).unwrap();
    std::fs::write(ssh.join("config"), SSH_CONFIG).unwrap();
    std::fs::write(ssh.join("lords_panel_main"), "the fixture's stand-in for the key").unwrap();
    std::fs::write(ssh.join("lords_panel_main.pub"), PUBLIC).unwrap();
    home
}

/// Sends `files` into this run's staging dir and installs from it on the PC whose profile is `home`, as `panel add
/// --apply` does (ssh aside); then removes the run's staging dir. The install's status and what it said.
fn install(home: &Path, files: Vec<Sent>) -> (bool, String) {
    let (ok, said, _) = run(STAGE, 30, "staging", home, b"");
    assert!(ok, "{said}");
    for (name, bytes, _) in files {
        let (ok, said, _) = run(RECEIVE, 30, name, home, &bytes);
        assert!(ok, "{name}: {said}");
    }
    let (ok, said, _) = run(INSTALL_STEP, 60, "install", home, b"");
    let (cleaned, why, _) = run(CLEAN, 30, "staging", home, b"");
    assert!(cleaned, "{why}");
    (ok, said)
}

/// What the install says it did (its line of JSON).
fn done(said: &str) -> Vec<String> {
    let line = said.lines().find(|l| l.trim_start().starts_with('{'));
    let line = line.unwrap_or_else(|| panic!("no JSON: {said}"));
    let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    v["done"].as_array().unwrap().iter().map(|d| d.as_str().unwrap().to_string()).collect()
}

/// The names in `dir`, sorted.
fn names(dir: &Path) -> Vec<String> {
    let mut all: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    all.sort();
    all
}

/// The Start-menu entry's place in `home`'s profile.
fn shortcut(home: &Path) -> PathBuf {
    roaming(home).join("Microsoft\\Windows\\Start Menu\\Programs\\lords-command.lnk")
}

/// A child process, ended with the test however the test ends.
struct Running(std::process::Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

// Spec (design note 2026-11-01-lords-panel-embedded E2: an embedded run sends the other PC no lords-command.exe,
// README.md or LICENSE, and needs no lords-command.exe beside this lords.exe; panel.json tells install.ps1 which client
// it sets up; a standalone run sends today's seven files in today's order, and is refused before anything without the
// window's exe): what each run sends, by name, in order.
#[test]
fn an_embedded_run_sends_only_what_its_client_reads() {
    let dir = tempfile::TempDir::new().unwrap();
    let exe = dir.path().join("lords.exe");
    let token = fixture_token('a');
    let sent = |client: Client| {
        let window = window_files(client, &exe).unwrap();
        let files = payload(&fixture_plan(client, Some(BLOCK)), "run-1", window, &token);
        let params: serde_json::Value = serde_json::from_slice(&files[0].1).unwrap();
        (files.iter().map(|f| f.0).collect::<Vec<_>>(), params["embedded"].clone())
    };
    let (names, embedded) = sent(Client::Embedded);
    assert_eq!(names, ["panel.json", "install.ps1", "check.ps1", "panel-token"]);
    assert_eq!(embedded, true);
    let refused = window_files(Client::Standalone, &exe).map(|_| ());
    assert!(
        refused.as_ref().is_err_and(|e| e.contains("no lords-command.exe beside")),
        "{refused:?}"
    );
    std::fs::write(dir.path().join("lords-command.exe"), b"the window").unwrap();
    let (names, embedded) = sent(Client::Standalone);
    assert_eq!(
        names,
        [
            "panel.json",
            "install.ps1",
            "check.ps1",
            "README.md",
            "LICENSE",
            "lords-command.exe",
            "panel-token"
        ]
    );
    assert_eq!(embedded, false);
}

// Spec (E2, E5.3 and E5.4, install.ps1 called directly: an embedded client's PC gets the alias, config.txt and the
// token, owner-only, by the same steps as a standalone one, and no lords-command.exe, README.md, LICENSE, backup dir
// or Start-menu entry, even were the window's files in the staging dir): on a PC with overseer's route and the
// panel's key, the install backs ~/.ssh/config up and appends the alias, writes config.txt and places the token with
// one entry, this account; then the app dir holds config.txt and the token alone, and the Start menu nothing.
#[test]
fn an_embedded_install_writes_only_what_its_client_reads() {
    let home = other_pc();
    // A profile's Start menu is there (powershell itself makes APPDATA at its start, so its being there says nothing).
    let menu = shortcut(home.path()).parent().unwrap().to_path_buf();
    std::fs::create_dir_all(&menu).unwrap();
    let token = fixture_token('b');
    let plan = fixture_plan(Client::Embedded, Some(BLOCK));
    let (ok, said) = install(home.path(), payload(&plan, "run-1", window_stand_ins(b"x"), &token));
    assert!(ok, "{said}");
    let app = home.path().join("Programs").join("lords-command");
    let ssh = home.path().join(".ssh");
    assert_eq!(names(&app), ["config.txt", "panel-token", "staging"]);
    assert!(names(&app.join("staging")).is_empty(), "the run's staging dir is gone");
    assert_eq!(std::fs::read_to_string(app.join("config.txt")).unwrap(), CONFIG_TXT);
    assert_eq!(std::fs::read_to_string(app.join("panel-token")).unwrap(), token.secret());
    assert_eq!(entries(&app.join("panel-token")), [format!("{}:(f)", me())]);
    assert_eq!(
        std::fs::read_to_string(ssh.join("config")).unwrap(),
        format!("{SSH_CONFIG}{BLOCK}")
    );
    assert_eq!(std::fs::read_to_string(ssh.join("config.before-lords-panel")).unwrap(), SSH_CONFIG);
    assert!(names(&menu).is_empty(), "no Start-menu entry: {:?}", names(&menu));
    assert_eq!(
        done(&said),
        [
            format!("backed up {}", ssh.join("config.before-lords-panel").display()),
            format!("added Host lords-panel-main to {}", ssh.join("config").display()),
            format!("wrote {}", app.join("config.txt").display()),
            format!("placed {} (owner-only)", app.join("panel-token").display()),
        ]
    );
}

// Spec (E4, the choice for a PC with the standalone install: kept as it is, never updated or removed; a window running
// there is no refusal for an embedded run, as nothing it runs from is replaced; a standalone run is refused as
// today): with the window installed and running from the app dir, the alias there and config.txt the owner's, a
// standalone install is refused and changes nothing; an embedded one leaves the window's files, the Start-menu entry,
// config.txt and ~/.ssh byte for byte, makes no backup dir, and replaces only the token, owner-only.
#[test]
fn an_embedded_install_keeps_a_standalone_install_and_its_running_window() {
    let home = other_pc();
    let ssh = home.path().join(".ssh");
    std::fs::write(ssh.join("config"), format!("{SSH_CONFIG}{BLOCK}")).unwrap();
    let app = home.path().join("Programs").join("lords-command");
    std::fs::create_dir_all(&app).unwrap();
    // The window: a copy of ping, so a process runs from the app dir under the window's name.
    let window = app.join("lords-command.exe");
    let system = std::env::var("SystemRoot").unwrap();
    std::fs::copy(Path::new(&system).join("System32").join("PING.EXE"), &window).unwrap();
    let mine = format!("{CONFIG_TXT}# the owner's own line\r\n");
    std::fs::write(app.join("config.txt"), &mine).unwrap();
    std::fs::write(app.join("README.md"), "the installed README").unwrap();
    std::fs::write(app.join("LICENSE"), "the installed LICENSE").unwrap();
    std::fs::write(app.join("panel-token"), fixture_token('c').secret()).unwrap();
    let lnk = shortcut(home.path());
    std::fs::create_dir_all(lnk.parent().unwrap()).unwrap();
    std::fs::write(&lnk, "the installed shortcut").unwrap();
    let kept: Vec<(PathBuf, Vec<u8>)> =
        [&window, &app.join("README.md"), &app.join("LICENSE"), &app.join("config.txt"), &lnk]
            .into_iter()
            .chain([&ssh.join("config"), &ssh.join("lords_panel_main.pub")])
            .map(|p| (p.clone(), std::fs::read(p).unwrap()))
            .collect();
    let running = Running(
        Command::new(&window)
            .args(["-n", "120", "127.0.0.1"])
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );

    let token = fixture_token('d');
    let standalone = fixture_plan(Client::Standalone, None);
    let (ok, said) =
        install(home.path(), payload(&standalone, "run-1", window_stand_ins(b"new"), &token));
    assert!(!ok && said.contains("lords-command runs here"), "{said}");
    assert_eq!(
        std::fs::read_to_string(app.join("panel-token")).unwrap(),
        fixture_token('c').secret()
    );

    let embedded = fixture_plan(Client::Embedded, None);
    let (ok, said) =
        install(home.path(), payload(&embedded, "run-1", window_stand_ins(b"new"), &token));
    assert!(ok, "{said}");
    let mut running = running;
    assert!(running.0.try_wait().unwrap().is_none(), "the window ran through the install");
    drop(running);
    for (path, bytes) in &kept {
        assert_eq!(&std::fs::read(path).unwrap(), bytes, "{} as it was", path.display());
    }
    assert_eq!(
        names(&app),
        ["LICENSE", "README.md", "config.txt", "lords-command.exe", "panel-token", "staging"]
    );
    assert_eq!(names(&ssh), ["config", "lords_panel_main", "lords_panel_main.pub"]);
    assert_eq!(std::fs::read_to_string(app.join("panel-token")).unwrap(), token.secret());
    assert_eq!(entries(&app.join("panel-token")), [format!("{}:(f)", me())]);
    assert_eq!(done(&said), [format!("placed {} (owner-only)", app.join("panel-token").display())]);
}

// Spec (E2: without --embedded nothing changes; proposal P3 steps 7 and 8, review r1 R7): a standalone install puts
// the window's three files in the app dir and makes the Start-menu entry; run again, it backs each file up into the
// run's backup dir before it replaces it, and keeps config.txt.
#[test]
fn a_standalone_install_still_installs_the_window_and_its_start_menu_entry() {
    let home = other_pc();
    let app = home.path().join("Programs").join("lords-command");
    let lnk = shortcut(home.path());
    // A profile's Start menu is there; the test's isn't until made.
    std::fs::create_dir_all(lnk.parent().unwrap()).unwrap();
    let token = fixture_token('e');
    let first = fixture_plan(Client::Standalone, Some(BLOCK));
    let (ok, said) =
        install(home.path(), payload(&first, "run-1", window_stand_ins(b"window 1"), &token));
    assert!(ok, "{said}");
    assert_eq!(
        names(&app),
        ["LICENSE", "README.md", "config.txt", "lords-command.exe", "panel-token", "staging"]
    );
    assert_eq!(std::fs::read(app.join("lords-command.exe")).unwrap(), b"window 1");
    assert!(lnk.is_file(), "{said}");

    std::fs::write(app.join("config.txt"), "the owner's").unwrap();
    let again = fixture_plan(Client::Standalone, None);
    let (ok, said) =
        install(home.path(), payload(&again, "run-1", window_stand_ins(b"window 2"), &token));
    assert!(ok, "{said}");
    assert_eq!(std::fs::read(app.join("lords-command.exe")).unwrap(), b"window 2");
    let backup = app.join("backup-run-1");
    assert_eq!(names(&backup), ["LICENSE", "README.md", "lords-command.exe"]);
    assert_eq!(std::fs::read(backup.join("lords-command.exe")).unwrap(), b"window 1");
    assert_eq!(std::fs::read_to_string(app.join("config.txt")).unwrap(), "the owner's");
}

/// A stand-in for the panel's server behind a forward: on its own loopback port, `200` to a request carrying `token`
/// in `Lords-Panel-Token`, `401` to any other; a connection that sends nothing (the check's look at the port) gets
/// nothing. Serves until the test ends.
fn forward_stand_in(token: &http::Token) -> u16 {
    use std::io::Read;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let want = format!("lords-panel-token: {}", token.secret().to_ascii_lowercase());
    std::thread::spawn(move || {
        for mut c in listener.incoming().flatten() {
            let mut head = Vec::new();
            let mut buf = [0u8; 1024];
            while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                match c.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => head.extend_from_slice(&buf[..n]),
                }
            }
            if head.is_empty() {
                continue;
            }
            let ok = String::from_utf8_lossy(&head).to_ascii_lowercase().contains(&want);
            let status = if ok { "200 OK" } else { "401 Unauthorized" };
            let answer =
                format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            let _ = c.write_all(answer.as_bytes());
        }
    });
    port
}

// Spec (check.ps1's invariant, sol-loop r1 S1: the route is checked through the forward on the panel's port; on a
// re-run, a window or an embedded view of the same alias may hold that port already, and the check's own ssh then
// fails its forward and ends; that is no failure of the route): with a forward that answers the token on the port,
// and the check's ssh ended before the check first looks at the port (its powershell held until then), the check
// passes through that forward; with nothing on the port, it fails, saying so.
#[test]
fn the_route_check_goes_through_a_forward_already_on_its_port() {
    let home = profile();
    let app = home.path().join("Programs").join("lords-command");
    std::fs::create_dir_all(&app).unwrap();
    let token = fixture_token('f');
    std::fs::write(app.join("panel-token"), token.secret()).unwrap();
    // The check's ssh: one that ends at once, as one whose forward finds the port taken does.
    let ssh = home.path().join("ssh-stand-in");
    std::fs::create_dir_all(&ssh).unwrap();
    let system = std::env::var("SystemRoot").unwrap();
    std::fs::copy(Path::new(&system).join("System32").join("whoami.exe"), ssh.join("ssh.exe"))
        .unwrap();
    let check = |port: u16| {
        let (ok, said, _) = run(STAGE, 30, "staging", home.path(), b"");
        assert!(ok, "{said}");
        let params = serde_json::json!({ "alias": "lords-panel-main", "local_port": port });
        for (name, bytes) in
            [("panel.json", params.to_string().into_bytes()), ("check.ps1", CHECK.into())]
        {
            let (ok, said, _) = run(RECEIVE, 30, name, home.path(), &bytes);
            assert!(ok, "{name}: {said}");
        }
        let h = home.path();
        let (ok, said, _) = run_with(CHECK_STEP, 60, "check", [h, h], h.parent(), Some(&ssh), b"");
        let (cleaned, why, _) = run(CLEAN, 30, "staging", home.path(), b"");
        assert!(cleaned, "{why}");
        (ok, said)
    };
    let (ok, said) = check(forward_stand_in(&token));
    assert!(ok && said.contains("{\"ok\":true,\"status\":200}"), "{said}");
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let (ok, said) = check(free);
    assert!(!ok && said.contains("didn't come up"), "{said}");
}
