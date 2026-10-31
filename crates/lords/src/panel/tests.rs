//! `lords panel add`'s remote steps, run here: the same PowerShell the other PC runs, in a local powershell with its
//! `LOCALAPPDATA` and `USERPROFILE` in a temp dir of the test's own, and the OS's profiles directory (HKLM, which a
//! test can't change) answered by the test. What ssh adds (the transport, the other PC's shell) is not run here.

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
    run_with(step, deadline, name, [home, home], home.parent(), stdin)
}

/// `run`, with `LOCALAPPDATA` and `USERPROFILE` as given, and the OS's profiles directory answered as `profiles`. The
/// step reads it with `Get-ItemProperty` from HKLM; here a function of that name, which PowerShell finds before the
/// cmdlet, answers instead (`-NoProfile` keeps any other out on the other PC). `None`: the registry answers.
fn run_with(
    step: &str,
    deadline: u64,
    name: &str,
    [local_app_data, profile]: [&Path; 2],
    profiles: Option<&Path>,
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
    let mut child = command
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command"])
        .arg(format!("{os}{step}"))
        .args([deadline.to_string().as_str(), "run-1", name])
        .env("LOCALAPPDATA", local_app_data)
        .env("USERPROFILE", profile)
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
    let (ok, said, _) = run_with(PROBE, 30, "none", [&local, &owner], Some(&current), b"");
    assert!(ok && said.contains("Current is a link"), "{said}");
    let (ok, said, _) = run_with(STAGE, 30, "staging", [&local, &owner], Some(&current), b"");
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
    let (ok, said, _) = run_with(PROBE, 30, "none", [&local, &owner], Some(&profiles), b"");
    assert!(ok && said.contains("not a profile in the OS profiles directory"), "{said}");
    let (ok, said, _) = run_with(STAGE, 30, "staging", [&local, &owner], Some(&profiles), b"");
    assert!(!ok && said.contains("not a profile in the OS profiles directory"), "{said}");
    assert!(!app.join("staging").exists(), "nothing made");
    let (ok, said, _) = run_with(PROBE, 30, "none", [&local, &owner], None, b"");
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
