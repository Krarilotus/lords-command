//! `lords panel add <alias>`'s plan: from what it read on both PCs, the exact changes, or why none (design note
//! 2026-10-31-lords-panel-remote, P3 and P4). Pure: facts in, steps or a refusal out; `mod.rs` reads the facts and
//! carries the steps out. Every change is "add", never "replace": what is there and differs is refused, for its owner
//! to settle by hand.

use serde::Deserialize;
use std::path::PathBuf;

/// The panel's server on this PC, and the port its forward listens on, on the other.
pub const SERVER_PORT: u16 = lords_core::panel::PORT;
pub const LOCAL_PORT: u16 = 17879;
/// overseer's coordinator, whose alias on the other PC is the route the panel's alias copies.
const OVERSEER_PORT: u16 = 7878;
/// The scheduled task that runs `lords panel serve` at the owner's logon.
pub const TASK: &str = "Lords-Panel-Serve";

/// What the probe read on the other PC (its `.pub` files only; no private key is ever read).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Remote {
    pub home: String,
    pub local_app_data: String,
    pub app_data: String,
    pub ssh: bool,
    pub keygen: bool,
    pub ssh_config: Option<String>,
    #[serde(default)]
    pub keys: Vec<PublicKey>,
    pub app: bool,
    pub config_txt: Option<String>,
    pub running: u32,
    pub listening: u32,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct PublicKey {
    pub name: String,
    pub text: String,
}

/// What this PC holds: the bridge's files and processes, the task, whether the server answers.
#[derive(Clone, Debug, Default)]
pub struct Local {
    pub bridge: PathBuf,
    pub sshd_config: String,
    pub authorized_keys: String,
    /// The bridge's listening sshd, from its supervisor's `status.json`.
    pub listener: Option<u32>,
    /// Its live sessions: its children running the same sshd.
    pub sessions: Vec<u32>,
    pub task: bool,
    pub served: bool,
    /// The tool's home, and whether it has the panel's token yet.
    pub home: PathBuf,
    pub token: bool,
}

/// The changes, each `None` where it is there already.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// The panel's alias on the other PC, and the alias it copies.
    pub alias: String,
    pub base: String,
    /// The key's file name in the other PC's `~/.ssh`, and the comment its line carries here.
    pub key: String,
    pub comment: String,
    /// The key's `.pub` line, if the key is there already; else it is made there.
    pub public: Option<String>,
    /// The `Host` block appended to the other PC's `~/.ssh/config`.
    pub block: Option<String>,
    /// The other PC's `~/.ssh/config` as read: it is appended to only while it still holds this.
    pub config_before: String,
    /// The bridge's `sshd_config` with the panel's port permitted.
    pub sshd_config: Option<String>,
    /// The other PC's config.txt for the panel, written only where there is none.
    pub config_txt: Option<String>,
    pub create_task: bool,
    pub run_task: bool,
}

struct Block {
    names: Vec<String>,
    lines: Vec<String>,
}

/// A config line's keyword (lower case) and value, `Keyword value` or `Keyword=value`.
fn keyword(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let at = line.find(|c: char| c.is_whitespace() || c == '=')?;
    let (k, v) = line.split_at(at);
    let v = v.trim_start_matches(|c: char| c.is_whitespace() || c == '=').trim();
    Some((k.to_ascii_lowercase(), v.to_string()))
}

/// An ssh config's `Host` blocks (a `Match` block ends one and is none itself).
fn blocks(config: &str) -> Vec<Block> {
    let mut all: Vec<Block> = Vec::new();
    let mut open = false;
    for line in config.lines() {
        match keyword(line) {
            Some((k, v)) if k == "host" => {
                all.push(Block {
                    names: v.split_whitespace().map(str::to_string).collect(),
                    lines: vec![line.to_string()],
                });
                open = true;
            }
            Some((k, _)) if k == "match" => open = false,
            _ if open => all.last_mut().expect("a block is open").lines.push(line.to_string()),
            _ => {}
        }
    }
    all
}

/// The ports a block's `LocalForward`s listen on and reach: `(local port, target port)`.
fn forwards(b: &Block) -> Vec<(Option<u16>, Option<u16>)> {
    let port = |s: &str| s.rsplit(':').next().and_then(|p| p.parse::<u16>().ok());
    b.lines
        .iter()
        .filter_map(|l| keyword(l).filter(|(k, _)| k == "localforward"))
        .map(|(_, v)| {
            let parts: Vec<&str> = v.split_whitespace().collect();
            (parts.first().and_then(|p| port(p)), parts.get(1).and_then(|p| port(p)))
        })
        .collect()
}

/// The panel's block: the base block's lines with its name, key and forward the panel's own.
fn panel_block(base: &Block, alias: &str, key: &str, nl: &str) -> String {
    let indent = base
        .lines
        .iter()
        .skip(1)
        .find_map(|l| keyword(l).map(|_| l[..l.len() - l.trim_start().len()].to_string()));
    let indent = indent.unwrap_or_else(|| "    ".into());
    let mut out = format!("Host {alias}{nl}");
    for l in base.lines.iter().skip(1) {
        let line = match keyword(l).map(|(k, _)| k) {
            Some(k) if k == "identityfile" => format!("{indent}IdentityFile ~/.ssh/{key}"),
            Some(k) if k == "localforward" => {
                format!("{indent}LocalForward 127.0.0.1:{LOCAL_PORT} 127.0.0.1:{SERVER_PORT}")
            }
            _ if l.trim().is_empty() => continue,
            _ => l.clone(),
        };
        out.push_str(&line);
        out.push_str(nl);
    }
    out
}

/// The lines of a block that say anything, trimmed: two blocks alike say the same.
fn said(text: &str) -> Vec<String> {
    text.lines().filter(|l| keyword(l).is_some()).map(|l| l.trim().to_string()).collect()
}

/// The bridge's `sshd_config` with the server's port permitted (`None`: it is already), or why it can't be.
pub fn sshd_config_with(text: &str) -> Result<Option<String>, String> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let forwarding =
        lines.iter().filter_map(|l| keyword(l)).find(|(k, _)| k == "allowtcpforwarding");
    if forwarding
        .is_some_and(|(_, v)| !["yes", "local", "all"].contains(&v.to_ascii_lowercase().as_str()))
    {
        return Err("the bridge's sshd_config allows no local forwards (AllowTcpForwarding)".into());
    }
    let want = format!("127.0.0.1:{SERVER_PORT}");
    let at = lines.iter().position(|l| keyword(l).is_some_and(|(k, _)| k == "permitopen")).ok_or(
        "the bridge's sshd_config has no PermitOpen line: each forward it allows is named there",
    )?;
    let (_, value) = keyword(lines[at]).expect("found above");
    if value.split_whitespace().any(|t| t == want) {
        return Ok(None);
    }
    if value.split_whitespace().any(|t| t.eq_ignore_ascii_case("any")) {
        return Err(
            "the bridge's PermitOpen allows any forward; the panel adds its port only to a list"
                .into(),
        );
    }
    let end = if lines[at].ends_with("\r\n") {
        "\r\n"
    } else if lines[at].ends_with('\n') {
        "\n"
    } else {
        ""
    };
    let line = format!("{} {want}{end}", lines[at].trim_end_matches(['\r', '\n']));
    let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    out[at] = line;
    Ok(Some(out.concat()))
}

/// The bridge's `authorized_keys` line for the panel's key: one forward, to the server's port, nothing else.
pub fn authorized_line(public: &str, comment: &str) -> Result<String, String> {
    let parts: Vec<&str> = public.split_whitespace().collect();
    let b64 = |s: &str| {
        !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b))
    };
    match parts[..] {
        [kind, key, ..] if kind.starts_with("ssh-") && b64(key) => Ok(format!(
            "restrict,port-forwarding,permitopen=\"127.0.0.1:{SERVER_PORT}\" {kind} {key} {comment}"
        )),
        _ => Err(format!("`{}` is no SSH public key", public.trim())),
    }
}

/// `authorized_keys` with the panel's line added (`None`: it is there), or why not: the same key with other options,
/// or the same comment on another key, is the owner's to settle.
pub fn authorized_with(text: &str, line: &str) -> Result<Option<String>, String> {
    let key = line.split_whitespace().nth(2).unwrap_or_default();
    let comment = line.split_whitespace().last().unwrap_or_default();
    for l in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        if l == line {
            return Ok(None);
        }
        let words: Vec<&str> = l.split_whitespace().collect();
        if words.contains(&key) {
            return Err(format!("authorized_keys has the panel's key with other options: {l}"));
        }
        if words.last() == Some(&comment) {
            return Err(format!(
                "authorized_keys has `{comment}` on another key; remove that line by hand first"
            ));
        }
    }
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let sep = if text.is_empty() || text.ends_with('\n') { "" } else { nl };
    Ok(Some(format!("{text}{sep}{line}{nl}")))
}

/// The plan for the PC this PC reaches as `name`, or why there is none.
pub fn plan(name: &str, local: &Local, remote: &Remote) -> Result<Plan, String> {
    if !remote.ssh || !remote.keygen {
        return Err(format!(
            "{name} has no OpenSSH client (ssh.exe and ssh-keygen.exe): add Windows' OpenSSH Client feature there first"
        ));
    }
    let home_rel = format!("{}\\AppData\\Local", remote.home.trim_end_matches('\\'));
    if !remote.local_app_data.eq_ignore_ascii_case(&home_rel) {
        return Err(format!(
            "{name}'s LOCALAPPDATA ({}) is not {home_rel}: the files are copied there relative to its home",
            remote.local_app_data
        ));
    }
    let config = remote.ssh_config.clone().unwrap_or_default();
    let all = blocks(&config);
    let base = all
        .iter()
        .find(|b| forwards(b).iter().any(|(_, to)| *to == Some(OVERSEER_PORT)))
        .ok_or_else(|| {
            format!(
                "no bridge on {name}: its ~/.ssh/config has no alias forwarding to 127.0.0.1:{OVERSEER_PORT} (overseer's route to this PC), which the panel's alias copies"
            )
        })?;
    let base_name = base.names.first().cloned().unwrap_or_default();
    let suffix = base_name.strip_prefix("fleet-panel-").unwrap_or(&base_name).to_string();
    let alias = format!("lords-panel-{suffix}");
    let key = format!("lords_panel_{}", suffix.replace(['-', '.'], "_"));
    lords_core::config::check_alias(&alias)?;
    let nl = if config.contains("\r\n") { "\r\n" } else { "\n" };
    let block = panel_block(base, &alias, &key, nl);
    let ours = all.iter().find(|b| b.names.contains(&alias));
    if let Some(b) = all.iter().find(|b| {
        !b.names.contains(&alias) && forwards(b).iter().any(|(from, _)| *from == Some(LOCAL_PORT))
    }) {
        return Err(format!(
            "on {name}, `Host {}` already forwards 127.0.0.1:{LOCAL_PORT}, the panel's port",
            b.names.join(" ")
        ));
    }
    let block = match ours {
        Some(b) if said(&b.lines.join("\n")) == said(&block) => None,
        Some(b) => {
            return Err(format!(
                "on {name}, `Host {alias}` is there and says otherwise (its forwards: {:?}); settle it by hand, then run again",
                forwards(b)
            ))
        }
        None if remote.listening > 0 => {
            return Err(format!("on {name}, 127.0.0.1:{LOCAL_PORT} is in use by something else"))
        }
        None => Some(block),
    };
    if remote.running > 0 {
        return Err(format!("lords-command runs on {name}; close it first (its exe is replaced)"));
    }
    let comment = format!("lords-panel-{}", name.to_ascii_lowercase());
    let public = remote
        .keys
        .iter()
        .find(|k| k.name == format!("{key}.pub"))
        .map(|k| k.text.trim().to_string());
    if let Some(p) = &public {
        authorized_with(&local.authorized_keys, &authorized_line(p, &comment)?)?;
    } else if local
        .authorized_keys
        .lines()
        .any(|l| l.split_whitespace().last() == Some(comment.as_str()))
    {
        return Err(format!(
            "authorized_keys has `{comment}` on a key {name} no longer has; remove that line first"
        ));
    }
    if local.listener.is_none() {
        return Err("the bridge's listener isn't running (its supervisor's status.json): start the bridge first".into());
    }
    let config_txt = remote
        .config_txt
        .is_none()
        .then(|| format!("api = http://127.0.0.1:{LOCAL_PORT}\r\ntunnel = {alias}\r\n"));
    Ok(Plan {
        base: base_name,
        alias,
        key,
        comment,
        public,
        block,
        config_before: config,
        sshd_config: sshd_config_with(&local.sshd_config)?,
        config_txt,
        create_task: !local.task,
        run_task: !local.served,
    })
}

impl Plan {
    /// The exact targets on both PCs, in the order `--apply` carries them out.
    /// `run`: this run's own id, which names its staging dirs on both PCs and its backup dir (review r1 R7).
    pub fn describe(&self, name: &str, local: &Local, remote: &Remote, run: &str) -> Vec<String> {
        let b = local.bridge.display();
        let ssh = format!("{}\\.ssh", remote.home);
        let app = format!("{}\\Programs\\lords-command", remote.local_app_data);
        let mut out = vec![format!("{name}:")];
        out.push(format!(
            "  + {app}\\staging\\{run}\\ (this run's own: what it sends, removed at its end)"
        ));
        out.push(match &self.public {
            Some(_) => format!("  = {ssh}\\{} (there: its .pub is used)", self.key),
            None => format!(
                "  + {ssh}\\{} (ed25519, made there; only its .pub leaves {name})",
                self.key
            ),
        });
        out.push(match &self.block {
            Some(_) => format!(
                "  + {ssh}\\config: Host {}, a copy of {} with IdentityFile ~/.ssh/{} and LocalForward 127.0.0.1:{LOCAL_PORT} 127.0.0.1:{SERVER_PORT} (backup config.before-lords-panel)",
                self.alias, self.base, self.key
            ),
            None => format!("  = {ssh}\\config: Host {} is there", self.alias),
        });
        if remote.app {
            out.push(format!(
                "  ~ {app}\\: lords-command.exe, README.md, LICENSE, each backed up first into {app}\\backup-{run}\\"
            ));
        } else {
            out.push(format!("  + {app}\\: lords-command.exe, README.md, LICENSE"));
        }
        out.push(format!(
            "  + {app}\\panel-token: a copy of this PC's, owner-only (replaced, never backed up: a secret keeps no copies)"
        ));
        out.push(match &self.config_txt {
            Some(_) => format!(
                "  + {app}\\config.txt: api = http://127.0.0.1:{LOCAL_PORT}, tunnel = {}",
                self.alias
            ),
            None => format!("  = {app}\\config.txt (kept as it is)"),
        });
        out.push(format!(
            "  + {}\\Microsoft\\Windows\\Start Menu\\Programs\\lords-command.lnk (if not there)",
            remote.app_data
        ));
        out.push("this PC:".into());
        let home = local.home.display();
        out.push(format!(
            "  + {home}\\panel-add-{run}\\ (this run's own: the task's definition, removed at its end)"
        ));
        if !local.token {
            out.push(format!(
                "  + {home}\\{}: the panel's token (random, owner-only; its text is never shown)",
                lords_core::panel::TOKEN_FILE
            ));
        }
        out.push(match &self.public {
            Some(p) => format!(
                "  + {b}\\authorized_keys: {} (backup authorized_keys.before-lords-panel)",
                authorized_line(p, &self.comment).unwrap_or_default()
            ),
            None => format!(
                "  + {b}\\authorized_keys: restrict,port-forwarding,permitopen=\"127.0.0.1:{SERVER_PORT}\" <the new key's .pub, from {name}> {} (backup authorized_keys.before-lords-panel)",
                self.comment
            ),
        });
        match &self.sshd_config {
            Some(_) => {
                out.push(format!(
                    "  ~ {b}\\sshd_config: PermitOpen + 127.0.0.1:{SERVER_PORT} (backup sshd_config.before-lords-panel)"
                ));
                let sessions: Vec<String> = local.sessions.iter().map(u32::to_string).collect();
                out.push(format!(
                    "  ! sshd -t -f sshd_config, then the bridge's listener (PID {}) stops alone; its supervisor starts it again within 5 s",
                    local.listener.map(|p| p.to_string()).unwrap_or_default()
                ));
                out.push(format!(
                    "    kept: {} live session(s) ({}), their own processes; dropped: none; a connection tried in those seconds fails and retries",
                    sessions.len(),
                    sessions.join(", ")
                ));
            }
            None => out
                .push(format!("  = {b}\\sshd_config permits 127.0.0.1:{SERVER_PORT}; no restart")),
        }
        if self.create_task {
            out.push(format!(
                "  + scheduled task {TASK} at the owner's logon: hidden powershell runs `lords panel serve` from this lords.exe"
            ));
        } else {
            out.push(format!("  = scheduled task {TASK} is there"));
        }
        out.push(if self.run_task {
            format!("  ▸ {TASK} runs once now (127.0.0.1:{SERVER_PORT} doesn't answer yet)")
        } else {
            format!("  = 127.0.0.1:{SERVER_PORT} answers already")
        });
        out.push(format!(
            "then, on {name}: ssh -N {} for at most 30 s and one GET 127.0.0.1:{LOCAL_PORT}/status through it",
            self.alias
        ));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "Host benchmon-x\r\n    HostName 127.0.0.1\r\n    Port 22220\r\n\r\nHost fleet-panel-main\r\n    HostName 127.0.0.1\r\n    Port 22220\r\n    User owner\r\n    IdentityFile ~/.ssh/fleet_panel_main\r\n    IdentitiesOnly yes\r\n    UserKnownHostsFile ~/.ssh/bridge-known-hosts\r\n    LocalForward 127.0.0.1:17878 127.0.0.1:7878\r\n";
    const SSHD: &str = "Port 22223\r\nAllowTcpForwarding local\r\nPermitOpen 127.0.0.1:7878 127.0.0.1:8317\r\nX11Forwarding no\r\n";
    const PUB: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExample0123456789abcdefABCDEF lords-panel-other";

    fn remote() -> Remote {
        // A neutral fixture home: no user's path, so the privacy check has nothing to take for one.
        let home = "X:\\fixture-home".to_string();
        Remote {
            local_app_data: format!("{home}\\AppData\\Local"),
            app_data: format!("{home}\\AppData\\Roaming"),
            home,
            ssh: true,
            keygen: true,
            ssh_config: Some(BASE.into()),
            ..Remote::default()
        }
    }

    fn local() -> Local {
        Local {
            bridge: "C:\\bridge".into(),
            sshd_config: SSHD.into(),
            authorized_keys: "restrict,port-forwarding,permitopen=\"127.0.0.1:7878\" ssh-ed25519 AAAAother fleet-panel-other\r\n".into(),
            listener: Some(21100),
            sessions: vec![42532, 33456],
            home: "X:\\fixture-tool-home".into(),
            ..Local::default()
        }
    }

    // Spec (P3, P4: the panel's alias is a copy of overseer's with its own key and forward; the bridge permits one
    // more port; a fresh PC gets the app, a config.txt and the task): for a PC with overseer's route and nothing of
    // the panel's yet, every target is an addition: the copied block (every other option kept, the file's own line
    // ends), the key made there, PermitOpen with 7879 added, the task created and run.
    #[test]
    fn a_fresh_pc_gets_a_copy_of_overseers_route_and_every_target_is_an_addition() {
        let p = plan("Other", &local(), &remote()).unwrap();
        assert_eq!(
            (p.alias.as_str(), p.key.as_str(), p.comment.as_str()),
            ("lords-panel-main", "lords_panel_main", "lords-panel-other")
        );
        assert_eq!(
            p.block.as_deref(),
            Some("Host lords-panel-main\r\n    HostName 127.0.0.1\r\n    Port 22220\r\n    User owner\r\n    IdentityFile ~/.ssh/lords_panel_main\r\n    IdentitiesOnly yes\r\n    UserKnownHostsFile ~/.ssh/bridge-known-hosts\r\n    LocalForward 127.0.0.1:17879 127.0.0.1:7879\r\n")
        );
        assert_eq!(
            p.sshd_config.as_deref(),
            Some("Port 22223\r\nAllowTcpForwarding local\r\nPermitOpen 127.0.0.1:7878 127.0.0.1:8317 127.0.0.1:7879\r\nX11Forwarding no\r\n")
        );
        assert!(p.public.is_none() && p.create_task && p.run_task);
        assert_eq!(
            p.config_txt.as_deref(),
            Some("api = http://127.0.0.1:17879\r\ntunnel = lords-panel-main\r\n")
        );
        let said = p.describe("Other", &local(), &remote(), "20261031-0900-1").join("\n");
        assert!(
            said.contains("kept: 2 live session(s) (42532, 33456)")
                && said.contains("dropped: none"),
            "{said}"
        );
    }

    // Spec (P4: "The same command updates the exe later and keeps config.txt"): run again on a PC that has it all,
    // nothing is added but the exe, which is replaced; config.txt, the alias, the key, PermitOpen and the task stay.
    #[test]
    fn a_second_run_replaces_only_the_exe() {
        let mut r = remote();
        r.ssh_config = Some(format!("{BASE}\r\nHost lords-panel-main\r\n    HostName 127.0.0.1\r\n    Port 22220\r\n    User owner\r\n    IdentityFile ~/.ssh/lords_panel_main\r\n    IdentitiesOnly yes\r\n    UserKnownHostsFile ~/.ssh/bridge-known-hosts\r\n    LocalForward 127.0.0.1:17879 127.0.0.1:7879\r\n"));
        r.keys = vec![PublicKey { name: "lords_panel_main.pub".into(), text: PUB.into() }];
        r.app = true;
        r.config_txt = Some("api = http://127.0.0.1:17879\r\n".into());
        r.listening = 1;
        let mut l = local();
        l.sshd_config = SSHD.replace("8317", "8317 127.0.0.1:7879");
        l.authorized_keys.push_str(&authorized_line(PUB, "lords-panel-other").unwrap());
        (l.task, l.served) = (true, true);
        let p = plan("Other", &l, &r).unwrap();
        assert_eq!(
            (p.block.as_ref(), p.sshd_config.as_ref(), p.config_txt.as_ref()),
            (None, None, None)
        );
        assert!(!p.create_task && !p.run_task);
        assert_eq!(
            authorized_with(&l.authorized_keys, &authorized_line(PUB, &p.comment).unwrap()),
            Ok(None)
        );
        // Review r1 R7: the dry run names every target this run touches: its staging dirs on both PCs, the backup of
        // each installed file it replaces, the token's copy; never the token's text.
        l.token = true;
        let said = p.describe("Other", &l, &r, "20261031-0900-1").join("\n");
        for target in [
            "X:\\fixture-home\\AppData\\Local\\Programs\\lords-command\\staging\\20261031-0900-1\\",
            "X:\\fixture-home\\AppData\\Local\\Programs\\lords-command\\backup-20261031-0900-1\\",
            "X:\\fixture-home\\AppData\\Local\\Programs\\lords-command\\panel-token",
            "X:\\fixture-tool-home\\panel-add-20261031-0900-1\\",
        ] {
            assert!(said.contains(target), "{target} in {said}");
        }
        assert!(!said.contains("X:\\fixture-tool-home\\panel-token"), "made already: {said}");
    }

    // Spec (brief, inputs to attack: "`panel add` against an alias that already exists, or that has a different
    // forward; a remote PC without OpenSSH, or without the bridge"): each is refused before anything is written, and
    // says what to settle: the panel's alias with another forward, another alias on the panel's port, no OpenSSH
    // client, no overseer route to copy, the panel already open there, a bridge whose sshd allows no listed forward,
    // a key line here that the other PC's key doesn't match.
    #[test]
    fn what_is_there_and_differs_is_refused_before_anything_is_written() {
        let other_forward = format!(
            "{BASE}Host lords-panel-main\r\n    LocalForward 127.0.0.1:17879 127.0.0.1:9999\r\n"
        );
        let on_our_port =
            format!("{BASE}Host something\r\n    LocalForward 127.0.0.1:17879 127.0.0.1:80\r\n");
        type Change = Box<dyn Fn(&mut Local, &mut Remote)>;
        let cases: Vec<(&str, Change)> = vec![
            ("says otherwise", Box::new(move |_, r| r.ssh_config = Some(other_forward.clone()))),
            (
                "already forwards 127.0.0.1:17879",
                Box::new(move |_, r| r.ssh_config = Some(on_our_port.clone())),
            ),
            ("no OpenSSH client", Box::new(|_, r| r.keygen = false)),
            ("no OpenSSH client", Box::new(|_, r| r.ssh = false)),
            ("no bridge", Box::new(|_, r| r.ssh_config = None)),
            ("no bridge", Box::new(|_, r| r.ssh_config = Some(BASE.replace("7878", "7000")))),
            ("in use by something else", Box::new(|_, r| r.listening = 1)),
            ("runs on Other", Box::new(|_, r| (r.app, r.running) = (true, 1))),
            ("no local forwards", Box::new(|l, _| l.sshd_config = SSHD.replace("local", "no"))),
            ("no PermitOpen", Box::new(|l, _| l.sshd_config = "Port 22223\r\n".into())),
            ("allows any", Box::new(|l, _| l.sshd_config = "PermitOpen any\r\n".into())),
            (
                "on a key Other no longer has",
                Box::new(|l, _| {
                    l.authorized_keys.push_str("ssh-ed25519 AAAAold lords-panel-other\r\n")
                }),
            ),
            ("listener isn't running", Box::new(|l, _| l.listener = None)),
        ];
        for (why, change) in &cases {
            let (mut l, mut r) = (local(), remote());
            change(&mut l, &mut r);
            let refused = plan("Other", &l, &r);
            assert!(refused.as_ref().is_err_and(|e| e.contains(why)), "{why}: {refused:?}");
        }
        // The panel's key there already, with other options here: refused too.
        let mut r = remote();
        r.keys = vec![PublicKey { name: "lords_panel_main.pub".into(), text: PUB.into() }];
        let mut l = local();
        l.authorized_keys.push_str(&format!("{PUB}\r\n"));
        assert!(plan("Other", &l, &r).is_err_and(|e| e.contains("other options")));
    }

    // Spec (P3 step 2: the line restricts the key to the one forward): a public key becomes exactly that line;
    // anything that isn't an SSH public key is refused, so nothing else ever reaches authorized_keys.
    #[test]
    fn only_a_public_key_becomes_an_authorized_line() {
        assert_eq!(
            authorized_line(PUB, "lords-panel-other").unwrap(),
            "restrict,port-forwarding,permitopen=\"127.0.0.1:7879\" ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExample0123456789abcdefABCDEF lords-panel-other"
        );
        // A private key's first line, built at run time so the privacy check doesn't take this source for a key.
        let private = ["-----BEGIN OPENSSH", "PRIVATE KEY-----"].join(" ");
        for bad in ["", &private, "command=\"x\" ssh-ed25519 AAAA", "ssh-ed25519 AA\"AA"] {
            assert!(authorized_line(bad, "c").is_err(), "{bad}");
        }
    }
}
