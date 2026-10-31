//! `lords panel add`'s remote steps, run here: the same PowerShell the other PC runs, in a local powershell with its
//! `LOCALAPPDATA` in a temp dir of the test's own. What ssh adds (the transport, the other PC's shell) is not run here.

// Tests are outside the filesystem and process boundary (fsguard.rs): their fixtures write their own temp dirs.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]
#![cfg(windows)]

use super::*;
use std::io::Write;
use std::process::{Command, Stdio};

/// Runs `step` as the other PC would (its arguments after it), with `stdin`; its status, output and time.
fn run(
    step: &str,
    deadline: u64,
    name: &str,
    home: &Path,
    stdin: &[u8],
) -> (bool, String, Duration) {
    let start = Instant::now();
    let mut child = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", step])
        .args([deadline.to_string().as_str(), "run-1", name])
        .env("LOCALAPPDATA", home)
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
    let home = tempfile::TempDir::new().unwrap();
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
    let home = tempfile::TempDir::new().unwrap();
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
    let home = tempfile::TempDir::new().unwrap();
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
