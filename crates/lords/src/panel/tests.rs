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
    run_in(step, deadline, "run-1", name, home, &Instead::default(), stdin)
}

/// What a test puts in place of what a step would call: PowerShell functions defined before the step, which
/// PowerShell finds before the cmdlets of the same name (`-NoProfile` keeps any other out on the other PC), and the
/// environment they read.
#[derive(Default)]
struct Instead {
    functions: String,
    env: Vec<(&'static str, String)>,
}

/// `run`, as the run `run`, with what the test puts `instead`.
fn run_in(
    step: &str,
    deadline: u64,
    run: &str,
    name: &str,
    home: &Path,
    instead: &Instead,
    stdin: &[u8],
) -> (bool, String, Duration) {
    run_with(step, deadline, run, name, [home, home], home.parent(), instead, stdin)
}

/// `run_in`, with `LOCALAPPDATA` and `USERPROFILE` as given, and the OS's profiles directory answered as `profiles`.
/// The step reads it with `Get-ItemProperty` from HKLM; here a function of that name answers instead. `None`: the
/// registry answers.
#[allow(clippy::too_many_arguments)]
fn run_with(
    step: &str,
    deadline: u64,
    run: &str,
    name: &str,
    [local_app_data, profile]: [&Path; 2],
    profiles: Option<&Path>,
    instead: &Instead,
    stdin: &[u8],
) -> (bool, String, Duration) {
    let os = match profiles {
        Some(_) => "function Get-ItemProperty { [pscustomobject]@{ ProfilesDirectory = $env:TEST_PROFILES } }; ",
        None => "",
    };
    let start = Instant::now();
    let mut command = Command::new("powershell");
    if let Some(dir) = profiles {
        command.env("TEST_PROFILES", dir);
    }
    for (name, value) in &instead.env {
        command.env(name, value);
    }
    let mut child = command
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command"])
        .arg(format!("{os}{}{step}", instead.functions))
        .args([deadline.to_string().as_str(), run, name])
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
    let (ok, said, _) = run_with(
        PROBE,
        30,
        "run-1",
        "none",
        [&local, &owner],
        Some(&current),
        &Instead::default(),
        b"",
    );
    assert!(ok && said.contains("Current is a link"), "{said}");
    let (ok, said, _) = run_with(
        STAGE,
        30,
        "run-1",
        "staging",
        [&local, &owner],
        Some(&current),
        &Instead::default(),
        b"",
    );
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
    let (ok, said, _) = run_with(
        PROBE,
        30,
        "run-1",
        "none",
        [&local, &owner],
        Some(&profiles),
        &Instead::default(),
        b"",
    );
    assert!(ok && said.contains("not a profile in the OS profiles directory"), "{said}");
    let (ok, said, _) = run_with(
        STAGE,
        30,
        "run-1",
        "staging",
        [&local, &owner],
        Some(&profiles),
        &Instead::default(),
        b"",
    );
    assert!(!ok && said.contains("not a profile in the OS profiles directory"), "{said}");
    assert!(!app.join("staging").exists(), "nothing made");
    let (ok, said, _) =
        run_with(PROBE, 30, "run-1", "none", [&local, &owner], None, &Instead::default(), b"");
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
        earlier: Vec::new(),
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
    // E12 R2: undo.ps1 travels with every run, embedded and standalone.
    assert_eq!(names, ["panel.json", "install.ps1", "undo.ps1", "check.ps1", "panel-token"]);
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
            "undo.ps1",
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
// panel's key, the install backs ~/.ssh/config up and appends the alias, places the token with one entry, this account,
// then writes config.txt (E12 R2: config.txt last, so it never stands without its token); then the app dir holds
// config.txt and the token alone, and the Start menu nothing.
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
            format!("placed {} (owner-only)", app.join("panel-token").display()),
            format!("wrote {}", app.join("config.txt").display()),
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

/// An ordinary service on a loopback port of its own: `200` with `body` to any request, whatever it carries (a health
/// endpoint). Serves until the test ends.
fn answers_anything(body: &'static str) -> u16 {
    use std::io::Read;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
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
            let answer = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = c.write_all(answer.as_bytes());
        }
    });
    port
}

/// A service that knows the panel's protocol but not its token, on a loopback port of its own (sol-loop S1 on cbc9b9b):
/// to `/proof?nonce=N` it answers the proof made from whatever token the request carries (none: the proof of an empty
/// key).
fn reflects_the_token() -> u16 {
    use std::io::Read;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
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
            let head = String::from_utf8_lossy(&head).into_owned();
            let nonce = head.split("nonce=").nth(1).unwrap_or("").split([' ', '&']).next();
            let token = head
                .lines()
                .find_map(|l| l.strip_prefix("Lords-Panel-Token: "))
                .unwrap_or("")
                .trim()
                .to_string();
            let message = format!("lords-panel proof {}", nonce.unwrap_or(""));
            let body = serde_json::json!({ "proof": http::hmac_sha256(token.as_bytes(), message.as_bytes()) })
                .to_string();
            let answer = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = c.write_all(answer.as_bytes());
        }
    });
    port
}

/// The panel's own server (`lords_core::http::serve`) with `token`, on a loopback port of its own; every request it
/// takes gets `{}`.
fn panel_server(token: &http::Token) -> u16 {
    let listener = http::bind(0).unwrap();
    let port = listener.local_addr().unwrap().port();
    let token = token.clone();
    std::thread::spawn(move || {
        http::serve(listener, http::Limits::default(), token, |_| {
            http::Reply::json(&serde_json::json!({}))
        })
    });
    port
}

/// A loopback port nothing listens on.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// The check's ssh, stood in for: `Start-Process ssh.exe …` starts a powershell running `$env:TEST_SSH` and gives that
/// process, so what that "ssh" listens on is its own, as a real ssh's forward is.
const SSH_STAND_IN: &str = "function Start-Process { param($FilePath, $WindowStyle, [switch]$PassThru, $ArgumentList) if ($FilePath -ne 'ssh.exe') { return Microsoft.PowerShell.Management\\Start-Process -FilePath $FilePath -WindowStyle $WindowStyle -PassThru -ArgumentList $ArgumentList }; Microsoft.PowerShell.Management\\Start-Process -FilePath powershell -WindowStyle Hidden -PassThru -ArgumentList '-NoProfile', '-NonInteractive', '-Command', 'Invoke-Expression $env:TEST_SSH' }; ";

/// A stood-in ssh whose forward works: it listens on 127.0.0.1:`port` and carries each connection to 127.0.0.1:`to`,
/// until either side closes it.
fn forwarding(port: u16, to: u16) -> String {
    format!(
        "$l = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, {port}); $l.Start(); while ($true) {{ $c = $l.AcceptTcpClient(); $u = [Net.Sockets.TcpClient]::new('127.0.0.1', {to}); $a = $c.GetStream(); $b = $u.GetStream(); [Threading.Tasks.Task[]]$both = @($a.CopyToAsync($b), $b.CopyToAsync($a)); [void][Threading.Tasks.Task]::WaitAny($both); $c.Close(); $u.Close() }}"
    )
}

// Spec (embedded panel review r1 R1, the Lords owner: the route check passes only on an answer that proves it came
// from this PC's panel server through this run's forward; any other responder on that port, an ssh that exited, or a
// missing reading fails closed): check.ps1 itself, its ssh stood in for. Through the check's own forward to the
// panel's server with the token: it passes. Through its own forward to an ordinary service answering 200 to anything,
// to a panel server with another token, or to a service that makes the proof from whatever token it is sent (sol-loop
// S1 on cbc9b9b: the check sends none): it fails. Its ssh ended at once while an ordinary service holds the port (the
// review's witness), or while nothing does: it fails. Its own forward to the panel's server, but the reading of who
// listens on the port failing: it fails.
#[test]
fn the_route_check_passes_only_on_the_panels_proof_through_its_own_forward() {
    let home = profile();
    let app = home.path().join("Programs").join("lords-command");
    std::fs::create_dir_all(&app).unwrap();
    let token = fixture_token('f');
    std::fs::write(app.join("panel-token"), token.secret()).unwrap();
    let check = |port: u16, ssh: String, more: &str| {
        let (ok, said, _) = run(STAGE, 30, "staging", home.path(), b"");
        assert!(ok, "{said}");
        let params = serde_json::json!({ "alias": "lords-panel-main", "local_port": port });
        for (name, bytes) in
            [("panel.json", params.to_string().into_bytes()), ("check.ps1", CHECK.into())]
        {
            let (ok, said, _) = run(RECEIVE, 30, name, home.path(), &bytes);
            assert!(ok, "{name}: {said}");
        }
        let instead =
            Instead { functions: format!("{SSH_STAND_IN}{more}"), env: vec![("TEST_SSH", ssh)] };
        let (ok, said, _) = run_in(CHECK_STEP, 60, "run-1", "check", home.path(), &instead, b"");
        let (cleaned, why, _) = run(CLEAN, 30, "staging", home.path(), b"");
        assert!(cleaned, "{why}");
        (ok, said)
    };
    let server = panel_server(&token);
    let port = free_port();
    let (ok, said) = check(port, forwarding(port, server), "");
    assert!(
        ok && said.contains("\"ok\":true"),
        "the panel's proof through its own forward: {said}"
    );

    let ordinary = answers_anything(r#"{"proof":"00"}"#);
    let another = panel_server(&fixture_token('9'));
    let reflector = reflects_the_token();
    for (to, who) in [(reflector, "reflector"), (ordinary, "ordinary"), (another, "another token")]
    {
        let port = free_port();
        let (ok, said) = check(port, forwarding(port, to), "");
        assert!(!ok && said.contains("without the panel's proof"), "{who}: {said}");
    }
    let held = answers_anything("{}");
    let (ok, said) = check(held, "exit 255".into(), "");
    let named = said.contains("ended") || said.contains("not this run's ssh");
    assert!(!ok && named, "its ssh ended, another on the port: {said}");
    let (ok, said) = check(free_port(), "exit 255".into(), "");
    assert!(!ok && said.contains("ended"), "its ssh ended, nothing on the port: {said}");
    let port = free_port();
    let blind = "function Get-NetTCPConnection { throw 'no reading of the listeners here' }; ";
    let (ok, said) = check(port, forwarding(port, server), blind);
    assert!(!ok && said.contains("no reading of the listeners here"), "{said}");
}

/// Every file in `home`'s profile but the run's staging dir, by its path below `home`: its bytes; a shortcut only as
/// being there (it names its own profile's paths).
fn files_of(home: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    fn walk(home: &Path, dir: &Path, out: &mut std::collections::BTreeMap<String, Vec<u8>>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let path = e.unwrap().path();
            let rel = path.strip_prefix(home).unwrap().to_string_lossy().into_owned();
            if rel.eq_ignore_ascii_case("Programs\\lords-command\\staging") {
                continue;
            }
            if path.is_dir() {
                walk(home, &path, out);
            } else if rel.ends_with(".lnk") {
                out.insert(rel, b"a shortcut".to_vec());
            } else {
                out.insert(rel, std::fs::read(&path).unwrap());
            }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    walk(home, home, &mut out);
    out
}

/// What differs between two `files_of`, for a failure's message.
fn differs(
    a: &std::collections::BTreeMap<String, Vec<u8>>,
    b: &std::collections::BTreeMap<String, Vec<u8>>,
) -> Vec<String> {
    let keys: std::collections::BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    keys.into_iter().filter(|k| a.get(*k) != b.get(*k)).cloned().collect()
}

/// ssh-keygen, stood in for: `Start-Process ssh-keygen.exe …` writes a stand-in key and its `.pub` (`$env:TEST_PUB`)
/// where `-f` says, and ends with 0; anything else starts as it would.
const KEYGEN_STAND_IN: &str = "function Start-Process { param($FilePath, [switch]$NoNewWindow, [switch]$PassThru, $ArgumentList, $WindowStyle) if ($FilePath -ne 'ssh-keygen.exe') { return Microsoft.PowerShell.Management\\Start-Process @PSBoundParameters }; $f = $ArgumentList[[array]::IndexOf($ArgumentList, '-f') + 1]; [IO.File]::WriteAllText($f, 'a stand-in key'); [IO.File]::WriteAllText($f + '.pub', $env:TEST_PUB); $ended = [pscustomobject]@{ ExitCode = 0 }; $ended | Add-Member -MemberType ScriptMethod -Name WaitForExit -Value {}; $ended }; ";

/// The install's journal writer, stood in for: at its `$env:TEST_AT`th line, `kill` ends the step's whole process tree
/// before the line is written (as its watchdog would), `written` writes the line and then does that (the change it
/// names not made), `error` fails there, and `hold` waits 8 s there, then goes on (an install that runs on after panel
/// add gave up on it); `$env:TEST_FIRED` gets the line when it does. Every other line is written as it would be.
const JOURNAL_STOPS: &str = "$global:lines = 0; function Add-Content { param($LiteralPath, $Value, $Encoding) $global:lines++; $at = $global:lines -eq [int]$env:TEST_AT; if ($at) { [IO.File]::WriteAllText($env:TEST_FIRED, [string]$Value) }; if ($at -and $env:TEST_HOW -eq 'kill') { taskkill /T /F /PID $PID | Out-Null; Start-Sleep -Seconds 30 }; if ($at -and $env:TEST_HOW -eq 'error') { throw 'an error injected here' }; if ($at -and $env:TEST_HOW -eq 'hold') { Start-Sleep -Seconds 8 }; Microsoft.PowerShell.Management\\Add-Content @PSBoundParameters; if ($at -and $env:TEST_HOW -eq 'written') { taskkill /T /F /PID $PID | Out-Null; Start-Sleep -Seconds 30 } }; ";

/// A PC before a run (E12, R2's tests): its set-up, and the plan's client, alias block (`None`: there) and config.txt
/// (`None`: there).
struct Before {
    client: Client,
    block: Option<&'static str>,
    config_txt: Option<&'static str>,
    set_up: fn(&Path),
}

/// Overseer's route in `~/.ssh/config`, a profile's Start menu, and no key yet.
fn fresh(home: &Path) {
    std::fs::create_dir_all(home.join(".ssh")).unwrap();
    std::fs::write(home.join(".ssh").join("config"), SSH_CONFIG).unwrap();
    std::fs::create_dir_all(shortcut(home).parent().unwrap()).unwrap();
}

/// `fresh`, and a token left in the app dir without config.txt.
fn token_alone(home: &Path) {
    fresh(home);
    let app = home.join("Programs").join("lords-command");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(app.join("panel-token"), fixture_token('c').secret()).unwrap();
}

/// The standalone panel set up: key, alias, the window's files, config.txt, its token and Start-menu entry.
fn standalone_there(home: &Path) {
    fresh(home);
    let ssh = home.join(".ssh");
    std::fs::write(ssh.join("config"), format!("{SSH_CONFIG}{BLOCK}")).unwrap();
    std::fs::write(ssh.join("lords_panel_main"), "the fixture's stand-in for the key").unwrap();
    std::fs::write(ssh.join("lords_panel_main.pub"), PUBLIC).unwrap();
    let app = home.join("Programs").join("lords-command");
    std::fs::create_dir_all(&app).unwrap();
    for (name, text) in [
        ("lords-command.exe", "the installed window"),
        ("README.md", "the installed README"),
        ("LICENSE", "the installed LICENSE"),
        (
            "config.txt",
            "api = http://127.0.0.1:17879\r\ntunnel = lords-panel-main\r\n# the owner's\r\n",
        ),
    ] {
        std::fs::write(app.join(name), text).unwrap();
    }
    std::fs::write(app.join("panel-token"), fixture_token('c').secret()).unwrap();
    std::fs::write(shortcut(home), "the installed shortcut").unwrap();
}

/// This run's staging dir for `before`, filled as STAGE and RECEIVE leave it, and the install run from it there,
/// stopped at journal line `at` the way `how` says (`JOURNAL_STOPS`); `fired` gets that line. The install's status and
/// what it said.
fn install_from_staging(
    home: &Path,
    before: &Before,
    stop: Option<(usize, &str, &Path)>,
) -> (bool, String) {
    let instead = staged_install(home, before, stop, "");
    let (ok, said, _) = run_in(INSTALL_STEP, 60, "run-1", "install", home, &instead, b"");
    (ok, said)
}

/// This run's staging dir for `before`, filled as STAGE and RECEIVE leave it, and what its install step is to run
/// with: ssh-keygen stood in for, the journal stopped as `stop` says (`JOURNAL_STOPS`), and the functions `more`.
fn staged_install(
    home: &Path,
    before: &Before,
    stop: Option<(usize, &str, &Path)>,
    more: &str,
) -> Instead {
    let plan = Plan {
        config_txt: before.config_txt.map(str::to_string),
        ..fixture_plan(before.client, before.block)
    };
    let window = match before.client {
        Client::Standalone => window_stand_ins(b"the window sent"),
        Client::Embedded => Vec::new(),
    };
    let staging = home.join("Programs").join("lords-command").join("staging").join("run-1");
    std::fs::create_dir_all(&staging).unwrap();
    for (name, bytes, _) in payload(&plan, "run-1", window, &fixture_token('7')) {
        std::fs::write(staging.join(name), bytes).unwrap();
    }
    let mut instead = Instead {
        functions: format!("{KEYGEN_STAND_IN}{more}"),
        env: vec![("TEST_PUB", PUBLIC.to_string())],
    };
    if let Some((at, how, fired)) = stop {
        instead.functions.push_str(JOURNAL_STOPS);
        instead.env.push(("TEST_AT", at.to_string()));
        instead.env.push(("TEST_HOW", how.to_string()));
        instead.env.push(("TEST_FIRED", fired.display().to_string()));
    }
    instead
}

/// R2's matrix for one PC: the install stopped at each line of its journal, each way, then panel add's ROLLBACK step,
/// then the staging dir removed (as CLEAN does): the PC's files byte for byte as before, or as a run nobody stopped
/// leaves them (from the same start, in a profile of its own).
fn stopped_at_every_line_it_is_as_it_was_or_complete(before: &Before) {
    let reference = profile();
    (before.set_up)(reference.path());
    let start = files_of(reference.path());
    let (ok, said) = install_from_staging(reference.path(), before, None);
    assert!(ok, "a run nobody stops: {said}");
    let complete = files_of(reference.path());
    assert_ne!(complete, start);
    let marks = tempfile::TempDir::new().unwrap();
    for how in ["kill", "written", "error"] {
        for at in 1.. {
            let home = profile();
            (before.set_up)(home.path());
            let fired = marks.path().join(format!("{how}-{at}"));
            let (ok, said) = install_from_staging(home.path(), before, Some((at, how, &fired)));
            let Ok(line) = std::fs::read_to_string(&fired) else {
                assert!(ok && at > 1, "{how}: the journal ends at line {}: {said}", at - 1);
                assert_eq!(files_of(home.path()), complete, "{how}: no stop");
                break;
            };
            assert!(!ok, "{how} at line {at} ({line}): the install failed: {said}");
            let (ok, said, _) =
                run_in(UNDO_STEP, 60, "run-1", "undo", home.path(), &Instead::default(), b"");
            assert!(ok, "{how} at line {at} ({line}), ROLLBACK: {said}");
            std::fs::remove_dir_all(home.path().join("Programs\\lords-command\\staging\\run-1"))
                .unwrap();
            let now = files_of(home.path());
            assert!(
                now == start || now == complete,
                "{how} at line {at} ({line}): neither as before ({:?}) nor complete ({:?})",
                differs(&now, &start),
                differs(&now, &complete)
            );
        }
    }
}

// Spec (embedded panel review r1 R2, the Lords owner: an --apply that fails at any step leaves the other PC as it was
// before (from its backups) or completely set up, in both embedded and standalone mode; never a live config.txt
// without its token, or the reverse; tested with a failure injected at each step), on a fresh PC, embedded.
#[test]
fn an_embedded_install_on_a_fresh_pc_stopped_anywhere_is_as_it_was_or_complete() {
    stopped_at_every_line_it_is_as_it_was_or_complete(&Before {
        client: Client::Embedded,
        block: Some(BLOCK),
        config_txt: Some(CONFIG_TXT),
        set_up: fresh,
    });
}

// Spec (as above), on a PC with a token left without config.txt, embedded.
#[test]
fn an_embedded_install_over_a_token_alone_stopped_anywhere_is_as_it_was_or_complete() {
    stopped_at_every_line_it_is_as_it_was_or_complete(&Before {
        client: Client::Embedded,
        block: Some(BLOCK),
        config_txt: Some(CONFIG_TXT),
        set_up: token_alone,
    });
}

// Spec (as above), on a PC with the standalone panel set up, embedded (E4: the window's files kept).
#[test]
fn an_embedded_install_over_the_standalone_one_stopped_anywhere_is_as_it_was_or_complete() {
    stopped_at_every_line_it_is_as_it_was_or_complete(&Before {
        client: Client::Embedded,
        block: None,
        config_txt: None,
        set_up: standalone_there,
    });
}

// Spec (as above), on a fresh PC, standalone.
#[test]
fn a_standalone_install_on_a_fresh_pc_stopped_anywhere_is_as_it_was_or_complete() {
    stopped_at_every_line_it_is_as_it_was_or_complete(&Before {
        client: Client::Standalone,
        block: Some(BLOCK),
        config_txt: Some(CONFIG_TXT),
        set_up: fresh,
    });
}

// Spec (as above), run again on a PC with the standalone panel set up, standalone (the window's files replaced, each
// from its backup).
#[test]
fn a_standalone_install_run_again_stopped_anywhere_is_as_it_was_or_complete() {
    stopped_at_every_line_it_is_as_it_was_or_complete(&Before {
        client: Client::Standalone,
        block: None,
        config_txt: None,
        set_up: standalone_there,
    });
}

// Spec (embedded panel review r1 R2's witness, the Lords owner: never a live config.txt without its token): on a
// fresh PC, the token's placement failing (its access list refused) fails the install, and after panel add's
// ROLLBACK and CLEAN the PC is as it was: no config.txt, no token, no alias or key it made.
#[test]
fn a_failed_token_placement_leaves_the_pc_as_it_was() {
    let home = other_pc();
    let start = files_of(home.path());
    let (ok, said, _) = run(STAGE, 30, "staging", home.path(), b"");
    assert!(ok, "{said}");
    let plan = fixture_plan(Client::Embedded, Some(BLOCK));
    for (name, bytes, _) in payload(&plan, "run-1", Vec::new(), &fixture_token('b')) {
        let (ok, said, _) = run(RECEIVE, 30, name, home.path(), &bytes);
        assert!(ok, "{name}: {said}");
    }
    let refused = Instead {
        functions: "function Set-Acl { throw 'the access list was refused' }; ".into(),
        env: Vec::new(),
    };
    let (ok, said, _) = run_in(INSTALL_STEP, 60, "run-1", "install", home.path(), &refused, b"");
    assert!(!ok && said.contains("the access list was refused"), "{said}");
    let (ok, said, _) =
        run_in(UNDO_STEP, 60, "run-1", "undo", home.path(), &Instead::default(), b"");
    assert!(ok, "{said}");
    let (ok, said, _) = run(CLEAN, 30, "staging", home.path(), b"");
    assert!(ok, "{said}");
    let now = files_of(home.path());
    assert!(now == start, "as it was: {:?}", differs(&now, &start));
}

/// An install started on `home` for `before`, held at its journal line `line` (`hold`) on a thread of its own: the
/// thread, once the install has reached that line.
fn held_install(
    home: &Path,
    before: &Before,
    line: usize,
    marks: &Path,
) -> std::thread::JoinHandle<(bool, String, Duration)> {
    let fired = marks.join(format!("held-{line}"));
    let instead = staged_install(home, before, Some((line, "hold", &fired)), "");
    let h = home.to_path_buf();
    let install =
        std::thread::spawn(move || run_in(INSTALL_STEP, 60, "run-1", "install", &h, &instead, b""));
    let started = Instant::now();
    while !fired.exists() {
        assert!(started.elapsed() < Duration::from_secs(30), "the install reached line {line}");
        std::thread::sleep(Duration::from_millis(100));
    }
    install
}

// Spec (sol-loop S3 on cbc9b9b, R2's contract: panel add's ROLLBACK and CLEAN never race an install that still runs
// there after panel add gave up on it, its ssh gone): with the install held at the journal line after the alias's (the
// alias in, the token not yet), a ROLLBACK started meanwhile, and in a second run a CLEAN, end only after the install
// has: the PC is then as it was or completely set up, never half of each.
#[test]
fn rollback_and_clean_wait_for_an_install_that_still_runs() {
    let before = Before {
        client: Client::Embedded,
        block: Some(BLOCK),
        config_txt: Some(CONFIG_TXT),
        set_up: fresh,
    };
    let reference = profile();
    fresh(reference.path());
    let (ok, said) = install_from_staging(reference.path(), &before, None);
    assert!(ok, "{said}");
    let complete = files_of(reference.path());
    let marks = tempfile::TempDir::new().unwrap();
    let none = Instead::default();
    let (undo, clean) = (&none, &none);
    for meanwhile in ["ROLLBACK", "CLEAN"] {
        let home = profile();
        fresh(home.path());
        let start = files_of(home.path());
        let install = held_install(home.path(), &before, 6, marks.path());
        let (ok, said, _) = match meanwhile {
            "ROLLBACK" => run_in(UNDO_STEP, UNDO_DEADLINE, "run-1", "undo", home.path(), undo, b""),
            _ => run_in(CLEAN, UNDO_DEADLINE, "run-1", "staging", home.path(), clean, b""),
        };
        assert!(ok, "{meanwhile}: {said}");
        let (_, installed, _) = install.join().unwrap();
        let (ok, said, _) =
            run_in(CLEAN, UNDO_DEADLINE, "run-1", "staging", home.path(), clean, b"");
        assert!(ok, "{said}");
        let now = files_of(home.path());
        assert!(
            now == start || now == complete,
            "{meanwhile} while the install ran ({installed}): neither as before ({:?}) nor complete ({:?})",
            differs(&now, &start),
            differs(&now, &complete)
        );
    }
}

// Spec (sol-loop S4 on cbc9b9b, R2's contract: a backup is a source to restore from only once it is whole): run again
// on a PC with the standalone panel, the window's backup copy failing half written (the disk full), the install fails;
// with panel add's ROLLBACK and CLEAN the PC is as it was: the window's exe whole, no half copy left behind.
#[test]
fn a_backup_that_failed_half_written_is_never_restored() {
    let before = Before {
        client: Client::Standalone,
        block: None,
        config_txt: None,
        set_up: standalone_there,
    };
    let home = profile();
    standalone_there(home.path());
    let start = files_of(home.path());
    let full = "function Copy-Item { param($LiteralPath, $Destination) if ([string]$Destination -like '*backup-*lords-command.exe') { [IO.File]::WriteAllText($Destination, 'half'); throw 'There is not enough space on the disk.' }; Microsoft.PowerShell.Management\\Copy-Item @PSBoundParameters }; ";
    let instead = staged_install(home.path(), &before, None, full);
    let (ok, said, _) = run_in(INSTALL_STEP, 60, "run-1", "install", home.path(), &instead, b"");
    assert!(!ok && said.contains("not enough space"), "{said}");
    let none = Instead::default();
    let (ok, said, _) = run_in(UNDO_STEP, UNDO_DEADLINE, "run-1", "undo", home.path(), &none, b"");
    assert!(ok, "{said}");
    let (ok, said, _) = run_in(CLEAN, UNDO_DEADLINE, "run-1", "staging", home.path(), &none, b"");
    assert!(ok, "{said}");
    let now = files_of(home.path());
    assert!(now == start, "as it was: {:?}", differs(&now, &start));
}

/// This PC's bridge, as `add::plan` needs it: overseer's port permitted, its listener running.
fn bridge() -> Local {
    Local {
        bridge: "X:\\fixture-bridge".into(),
        sshd_config: "AllowTcpForwarding local\r\nPermitOpen 127.0.0.1:7878\r\n".into(),
        listener: Some(1),
        home: "X:\\fixture-tool-home".into(),
        token: true,
        ..Local::default()
    }
}

// Spec (sol-loop S2 on cbc9b9b, R2's contract: what an earlier run's undo takes back, the plan carried out doesn't take
// for there): an earlier run's install killed at the line after the alias's (the alias in), its undo lost with the
// connection; the next run's first reading sees that alias, so its first plan adds none. Its --apply undoes the earlier
// run, reads the PC again and plans anew (`plan_to_apply`): the plan carried out adds the alias again, and the PC ends
// with the alias, config.txt and the token.
#[test]
fn the_plan_carried_out_is_made_after_earlier_runs_are_undone() {
    let home = profile();
    // LOCALAPPDATA where Windows puts it in a profile: the plan holds it to that.
    let app_data = home.path().join("AppData").join("Local");
    std::fs::create_dir_all(&app_data).unwrap();
    std::fs::create_dir_all(home.path().join(".ssh")).unwrap();
    std::fs::write(home.path().join(".ssh").join("config"), SSH_CONFIG).unwrap();
    let marks = tempfile::TempDir::new().unwrap();
    let killed = marks.path().join("killed");
    let keygen = || Instead {
        functions: KEYGEN_STAND_IN.to_string(),
        env: vec![("TEST_PUB", PUBLIC.to_string())],
    };
    let at = |fail| {
        let mut here = Here::new(home.path(), fail);
        here.app_data = app_data.clone();
        here.install = keygen();
        here
    };
    let read = probe(&at(Fail::No), "Other", "run-1").unwrap();
    let plan = add::plan("Other", &bridge(), &read, Client::Embedded).unwrap();
    assert!(plan.block.is_some(), "{plan:?}");
    // STAGE, five files, INSTALL: from the eighth call on (ROLLBACK), the connection is gone.
    let mut earlier = at(Fail::From(8));
    earlier.install.functions.push_str(JOURNAL_STOPS);
    earlier.install.env.extend([
        ("TEST_AT", "6".to_string()),
        ("TEST_HOW", "kill".to_string()),
        ("TEST_FIRED", killed.display().to_string()),
    ]);
    let files = |plan: &Plan, run: &str| payload(plan, run, Vec::new(), &fixture_token('a'));
    assert!(install_there(&earlier, "Other", "run-1", files(&plan, "run-1")).is_err());
    assert!(killed.exists(), "the earlier install was killed at its line 6");

    let next = at(Fail::No);
    let read = probe(&next, "Other", "run-2").unwrap();
    let first = add::plan("Other", &bridge(), &read, Client::Embedded).unwrap();
    assert!(first.block.is_none() && first.earlier == ["run-1"], "{first:?}");
    let plan = plan_to_apply(&next, "Other", &bridge(), "run-2", first).unwrap();
    assert!(plan.earlier.is_empty(), "{plan:?}");
    let block = plan.block.clone().expect("the alias added again");
    install_there(&next, "Other", "run-2", files(&plan, "run-2")).unwrap();
    let config = std::fs::read_to_string(home.path().join(".ssh").join("config")).unwrap();
    assert!(config.ends_with(&block), "{config}");
    let app = app_data.join("Programs").join("lords-command");
    assert!(app.join("config.txt").is_file() && app.join("panel-token").is_file());
}

/// How a stand-in other PC fails `panel add`'s calls: not at all, the `n`th call before it reaches the PC, the `n`th
/// after it ran there (its answer lost), or every call from the `n`th on (the connection gone).
#[derive(Clone, Copy, Debug)]
enum Fail {
    No,
    Before(usize),
    After(usize),
    From(usize),
}

/// The other PC, stood in for by this one: each step run here, in the profile `home` with `app_data` its
/// `LOCALAPPDATA`, failing as `fail` says; its install step with what `install` puts in place.
struct Here<'a> {
    home: &'a Path,
    app_data: PathBuf,
    fail: Fail,
    calls: std::cell::Cell<usize>,
    install: Instead,
}

impl<'a> Here<'a> {
    fn new(home: &'a Path, fail: Fail) -> Here<'a> {
        let app_data = home.to_path_buf();
        Here { home, app_data, fail, calls: Default::default(), install: Instead::default() }
    }
}

impl Other for Here<'_> {
    fn step(
        &self,
        step: &str,
        deadline: u64,
        run: &str,
        name: &str,
        stdin: Option<Vec<u8>>,
    ) -> Result<String, String> {
        let n = self.calls.get() + 1;
        self.calls.set(n);
        match self.fail {
            Fail::Before(k) if n == k => return Err(format!("call {n} lost before it ran")),
            Fail::From(k) if n >= k => return Err(format!("call {n}: the connection is gone")),
            _ => {}
        }
        let stdin = stdin.unwrap_or_default();
        let nothing = Instead::default();
        let instead = if step == INSTALL_STEP { &self.install } else { &nothing };
        let places = [self.app_data.as_path(), self.home];
        let (ok, said, _) =
            run_with(step, deadline, run, name, places, self.home.parent(), instead, &stdin);
        if matches!(self.fail, Fail::After(k) if n == k) {
            return Err(format!("call {n}'s answer lost"));
        }
        let last = said.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
        if ok {
            Ok(last.to_string())
        } else {
            Err(said)
        }
    }
}

// Spec (E12 R2, the Lords owner: an --apply that fails at any step leaves the other PC as it was or completely set
// up; tested with a failure injected at each step): panel add's part on the other PC (`install_there`: STAGE, each
// file, INSTALL; on any failure ROLLBACK, then CLEAN) with each of its calls failing in turn, before it reaches the PC,
// after it ran (its answer lost), or with the connection gone from it on: the PC is as it was or complete, and its
// staging dir is gone, unless the connection went before ROLLBACK could run; then the next probe names that dir, and
// the next run's undo of it (`undo_earlier`) leaves the PC as it was or complete, and no staging dir. Nothing on this
// PC is run here: after INSTALL nothing changes the other PC.
#[test]
fn panel_adds_part_on_the_other_pc_leaves_it_as_it_was_or_complete_whatever_step_fails() {
    let plan = fixture_plan(Client::Embedded, Some(BLOCK));
    let files = || payload(&plan, "run-1", Vec::new(), &fixture_token('d'));
    let reference = other_pc();
    let start = files_of(reference.path());
    let here = Here::new(reference.path(), Fail::No);
    install_there(&here, "Other", "run-1", files()).unwrap();
    let calls = here.calls.get();
    let complete = files_of(reference.path());
    let staging = |home: &Path| home.join("Programs\\lords-command\\staging\\run-1");
    for k in 1..=calls + 2 {
        for fail in [Fail::Before(k), Fail::After(k), Fail::From(k)] {
            let home = other_pc();
            let here = Here::new(home.path(), fail);
            let result = install_there(&here, "Other", "run-1", files());
            let now = files_of(home.path());
            assert!(
                now == start || now == complete,
                "{fail:?}: {result:?}: neither as before ({:?}) nor complete ({:?})",
                differs(&now, &start),
                differs(&now, &complete)
            );
            // A run that succeeds keeps its staging dir for the route check; panel add removes it after.
            if result.is_ok() || !staging(home.path()).exists() {
                continue;
            }
            let (ok, said, _) = run(PROBE, 30, "probe", home.path(), b"");
            assert!(ok, "{said}");
            let probed: Remote =
                serde_json::from_str(said.lines().rev().find(|l| l.starts_with('{')).unwrap())
                    .unwrap();
            assert_eq!(probed.earlier, ["run-1"], "{fail:?}: the next probe names it");
            let next = Here::new(home.path(), Fail::No);
            undo_earlier(&next, "Other", "run-1").unwrap();
            let now = files_of(home.path());
            assert!(
                now == start || now == complete,
                "{fail:?}, then the next run's undo: {:?} / {:?}",
                differs(&now, &start),
                differs(&now, &complete)
            );
            assert!(!staging(home.path()).exists(), "{fail:?}: the next run removed it");
        }
    }
}
