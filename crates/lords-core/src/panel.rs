//! The panel's calls (remote-panel design P1): what the window reads and writes, the same whether it reads the stacks
//! on this PC (`Source::Local`) or through `lords panel serve` on the PC that holds them (`Source::Remote`), whose
//! `handle` answers each request with these same local calls. Either way the window gets `view`'s structs.
//!
//! A write goes through `ops`, carries the version of the entry as it was read, and is checked and committed through
//! the stack repo's hooks under this PC's git identity; writes run one at a time in this process. A push goes through
//! each repo's pre-push privacy check. Nothing is sent but these views: an entry's files are found from the entry,
//! never from a path in a request.

use crate::http::{self, CallError, Client, Reply, Request};
use crate::model::Visibility;
use crate::ops::{self, Edit, Retire};
use crate::privacy::Rules;
use crate::view::{self, AheadView, ComposedView, FindingsView, PlanView, StacksView, StatusView};
use crate::{adapters, check, git, sync, Finding, Stacks};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

/// The port `lords panel serve` listens on, on 127.0.0.1.
pub const PORT: u16 = 7879;

/// How long the window waits for a read, for one that runs `check` or a sync plan, for a write (its commit runs the
/// stack repo's hooks) and for a push of both stack repos.
const READ: Duration = Duration::from_secs(60);
const SLOW_READ: Duration = Duration::from_secs(180);
const WRITE: Duration = Duration::from_secs(300);
/// Each repo's push, here; the window waits for both.
const PUSH_EACH: Duration = Duration::from_secs(120);

/// The stacks, where this PC holds them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    pub public: PathBuf,
    pub private: Option<PathBuf>,
}

/// Where the window reads the stacks from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Local(Paths),
    /// `lords panel serve` on the PC that holds them, through an SSH forward.
    Remote(Client),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// Refused or failed where it ran (a check, a stale version, git, another write running): its words.
    Refused(String),
    /// The panel's server wasn't reached, or a read broke off: nothing changed.
    Unreachable(String),
    /// A write's request went out and its reply didn't come back: it may have landed.
    Lost(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Refused(m) | Failure::Unreachable(m) => f.write_str(m),
            Failure::Lost(m) => write!(f, "{m}; the write may have landed"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EditRequest {
    pub id: String,
    /// Its `base` is the version the change was made from; a panel edit without one is refused.
    pub edit: Edit,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetireRequest {
    pub id: String,
    pub base: String,
    pub superseded_by: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Saved {
    pub id: String,
    pub commit: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pushed {
    pub lines: Vec<String>,
}

impl Source {
    pub fn is_remote(&self) -> bool {
        matches!(self, Source::Remote(_))
    }

    /// Both stacks' entries, what they declare, and where composed text and plans can be shown (fast: no check).
    pub fn stacks(&self) -> Result<StacksView, Failure> {
        match self {
            Source::Local(p) => Ok(view::stacks(&load(p)?)),
            Source::Remote(c) => get(c, "/stacks", &[], READ),
        }
    }

    /// `check`, with drift.
    pub fn findings(&self) -> Result<FindingsView, Failure> {
        match self {
            Source::Local(p) => Ok(local_findings(p)),
            Source::Remote(c) => get(c, "/findings", &[], SLOW_READ),
        }
    }

    /// An entry's history, newest first.
    pub fn history(&self, id: &str) -> Result<Vec<git::Commit>, Failure> {
        match self {
            Source::Local(p) => {
                let (root, file) = entry_file(&load(p)?, id)?;
                git::log(&root, &file).map_err(Failure::Refused)
            }
            Source::Remote(c) => get(c, "/history", &[("id", id)], READ),
        }
    }

    /// What one commit of its history changed in an entry's file.
    pub fn patch(&self, id: &str, commit: &str) -> Result<String, Failure> {
        match self {
            Source::Local(p) => {
                // Only a hash reaches git here: anything else (`--output=…`) is refused.
                if !(4..=64).contains(&commit.len())
                    || !commit.bytes().all(|b| b.is_ascii_hexdigit())
                {
                    return Err(Failure::Refused(format!("`{commit}` is no commit hash")));
                }
                let (root, file) = entry_file(&load(p)?, id)?;
                git::show(&root, commit, &file).map_err(Failure::Refused)
            }
            Source::Remote(c) => get(c, "/patch", &[("id", id), ("commit", commit)], READ),
        }
    }

    /// What `adapter` reads at the user level (`place` none) or in a declared repo or workspace.
    pub fn compose(&self, adapter: &str, place: Option<&str>) -> Result<ComposedView, Failure> {
        match self {
            Source::Local(p) => local_compose(&load(p)?, adapter, place),
            Source::Remote(c) => {
                let mut q = vec![("adapter", adapter)];
                q.extend(place.map(|p| ("place", p)));
                get(c, "/compose", &q, READ)
            }
        }
    }

    /// A dry run of sync, at the user level (`place` none) or for one declared repo or workspace.
    pub fn plan(&self, place: Option<&str>) -> Result<PlanView, Failure> {
        match self {
            Source::Local(p) => {
                let stacks = load(p)?;
                let opts = sync::Options {
                    repo: place.map(str::to_string),
                    user_only: place.is_none(),
                    home: sync::default_home().map_err(Failure::Refused)?,
                    ..sync::Options::default()
                };
                sync::plan(&stacks, &opts).map(|p| view::plan(&p)).map_err(Failure::Refused)
            }
            Source::Remote(c) => {
                let q: Vec<(&str, &str)> = place.map(|p| ("place", p)).into_iter().collect();
                get(c, "/plan", &q, SLOW_READ)
            }
        }
    }

    /// Each stack repo's commits its upstream doesn't have yet.
    pub fn status(&self) -> Result<StatusView, Failure> {
        match self {
            Source::Local(p) => Ok(local_status(p)),
            Source::Remote(c) => get(c, "/status", &[], READ),
        }
    }

    pub fn edit(&self, request: &EditRequest) -> Result<Saved, Failure> {
        match self {
            Source::Local(p) => local_edit(p, request),
            Source::Remote(c) => post(c, "/edit", request, WRITE),
        }
    }

    pub fn retire(&self, request: &RetireRequest) -> Result<Saved, Failure> {
        match self {
            Source::Local(p) => local_retire(p, request),
            Source::Remote(c) => post(c, "/retire", request, WRITE),
        }
    }

    /// Pushes each stack repo that is ahead of its upstream, through its pre-push check.
    pub fn push(&self) -> Result<Pushed, Failure> {
        match self {
            Source::Local(p) => local_push(p),
            Source::Remote(c) => post(c, "/push", &serde_json::json!({}), PUSH_EACH * 2 + READ),
        }
    }
}

// ---------------------------------------------------------------- the server's answers

/// The answer of `lords panel serve` to one request that passed http.rs's checks: the local call it names, as JSON.
pub fn handle(paths: &Paths, request: &Request) -> Reply {
    let local = Source::Local(paths.clone());
    let need = |name: &str| {
        request.param(name).ok_or_else(|| Reply::error(400, format!("`{name}` is missing")))
    };
    let reply = |r: Result<Reply, Reply>| r.unwrap_or_else(|refused| refused);
    let answer = |r: Result<Reply, Failure>| r.unwrap_or_else(|f| Reply::error(409, f.to_string()));
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/stacks") => answer(local.stacks().map(|v| Reply::json(&v))),
        ("GET", "/findings") => answer(local.findings().map(|v| Reply::json(&v))),
        ("GET", "/status") => answer(local.status().map(|v| Reply::json(&v))),
        ("GET", "/history") => {
            reply(need("id").map(|id| answer(local.history(id).map(|v| Reply::json(&v)))))
        }
        ("GET", "/patch") => reply(need("id").and_then(|id| {
            let commit = need("commit")?;
            Ok(answer(local.patch(id, commit).map(|v| Reply::json(&v))))
        })),
        ("GET", "/compose") => reply(need("adapter").map(|adapter| {
            answer(local.compose(adapter, request.param("place")).map(|v| Reply::json(&v)))
        })),
        ("GET", "/plan") => answer(local.plan(request.param("place")).map(|v| Reply::json(&v))),
        ("POST", "/edit") => reply(
            body(request).map(|r: EditRequest| answer(local.edit(&r).map(|v| Reply::json(&v)))),
        ),
        ("POST", "/retire") => reply(
            body(request).map(|r: RetireRequest| answer(local.retire(&r).map(|v| Reply::json(&v)))),
        ),
        ("POST", "/push") => answer(local.push().map(|v| Reply::json(&v))),
        (m, p) => Reply::error(404, format!("no {m} {p} here")),
    }
}

fn body<T: DeserializeOwned>(request: &Request) -> Result<T, Reply> {
    serde_json::from_slice(&request.body).map_err(|e| Reply::error(400, format!("a bad body: {e}")))
}

// ---------------------------------------------------------------- the remote side

fn reply_of<T: DeserializeOwned>(
    result: Result<(u16, Vec<u8>), CallError>,
    write: bool,
) -> Result<T, Failure> {
    match result {
        Ok((200, body)) => serde_json::from_slice(&body)
            .map_err(|e| Failure::Refused(format!("a bad reply from the panel's server: {e}"))),
        Ok((status, body)) => {
            let said: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
            Err(Failure::Refused(
                said["error"].as_str().map_or(format!("HTTP {status}"), str::to_string),
            ))
        }
        Err(CallError::TooLarge(m)) => Err(Failure::Refused(m)),
        Err(CallError::Unreachable(m)) => Err(Failure::Unreachable(m)),
        Err(CallError::Broken(m)) if write => Err(Failure::Lost(m)),
        Err(CallError::Broken(m)) => Err(Failure::Unreachable(m)),
    }
}

fn get<T: DeserializeOwned>(
    c: &Client,
    path: &str,
    query: &[(&str, &str)],
    deadline: Duration,
) -> Result<T, Failure> {
    reply_of(c.call("GET", &http::target(path, query), None, deadline), false)
}

fn post<T: DeserializeOwned>(
    c: &Client,
    path: &str,
    body: &impl Serialize,
    deadline: Duration,
) -> Result<T, Failure> {
    let bytes = serde_json::to_vec(body).map_err(|e| Failure::Refused(e.to_string()))?;
    reply_of(c.call("POST", path, Some(&bytes), deadline), true)
}

// ---------------------------------------------------------------- the local side

fn load(p: &Paths) -> Result<Stacks, Failure> {
    Stacks::load(&p.public, p.private.as_deref()).map_err(Failure::Refused)
}

fn local_findings(p: &Paths) -> FindingsView {
    let found = match Stacks::load(&p.public, p.private.as_deref()) {
        Err(e) => vec![Finding::error("load", e)],
        Ok(stacks) => {
            match Rules::public().with_allow_from(&p.public).with_private(stacks.private.as_ref()) {
                Err(e) => vec![Finding::error("privacy-scope", e)],
                Ok(rules) => {
                    let mut found = check::check(&stacks, &rules);
                    found.extend(sync::drift_findings(
                        &stacks,
                        &sync::default_home().unwrap_or_default(),
                    ));
                    found
                }
            }
        }
    };
    view::findings(&found)
}

/// An entry's stack root and its metadata file below it.
fn entry_file(stacks: &Stacks, id: &str) -> Result<(PathBuf, PathBuf), Failure> {
    let e = stacks.find(id).ok_or_else(|| Failure::Refused(format!("no entry `{id}`")))?;
    let root = stacks.stack(e.stack).map(|s| s.root.clone()).unwrap_or_default();
    let file = e.meta_file.strip_prefix(&root).unwrap_or(&e.meta_file).to_path_buf();
    Ok((root, file))
}

fn local_compose(
    stacks: &Stacks,
    adapter: &str,
    place: Option<&str>,
) -> Result<ComposedView, Failure> {
    let a = adapters::get(adapter)
        .ok_or_else(|| Failure::Refused(format!("no adapter `{adapter}`")))?;
    let Some(place) = place else {
        return Ok(view::composed(stacks, &sync::user_composed(stacks, a)));
    };
    let private = stacks.private.as_ref();
    let declared = private.is_some_and(|p| {
        p.manifest.repos.contains_key(place) || p.manifest.workspaces.contains_key(place)
    });
    if !declared {
        return Err(Failure::Refused(format!(
            "no repo or workspace `{place}` in the private stack.toml"
        )));
    }
    let mut v = view::composed(stacks, &sync::repo_composed(stacks, place, a));
    v.preamble = private
        .and_then(|p| p.manifest.repos.get(place))
        .and_then(|r| r.checkouts.first())
        .map(|c| crate::config::expand_home(c))
        .and_then(|c| a.repo_preamble(&c));
    Ok(v)
}

/// Each stack's repo, once (both stacks may share one).
fn stack_repos(p: &Paths) -> Vec<(Visibility, Result<PathBuf, String>)> {
    let mut repos: Vec<(Visibility, Result<PathBuf, String>)> = Vec::new();
    let stacks = [(Visibility::Public, Some(&p.public)), (Visibility::Private, p.private.as_ref())];
    for (v, root) in stacks {
        let Some(root) = root else { continue };
        let top =
            git::toplevel(root).ok_or_else(|| format!("{}: not in a git repo", root.display()));
        if !repos.iter().any(|(_, t)| t.is_ok() && t.as_ref().ok() == top.as_ref().ok()) {
            repos.push((v, top));
        }
    }
    repos
}

fn local_status(p: &Paths) -> StatusView {
    let repos = stack_repos(p)
        .into_iter()
        .map(|(stack, top)| {
            let read = top.and_then(|t| git::ahead(&t));
            match read {
                Ok((branch, upstream, commits)) => {
                    AheadView { stack, branch, upstream, commits, error: None }
                }
                Err(e) => AheadView {
                    stack,
                    branch: String::new(),
                    upstream: None,
                    commits: Vec::new(),
                    error: Some(e),
                },
            }
        })
        .collect();
    StatusView { repos }
}

/// One write at a time in this process: a second one is refused while the first runs, never queued behind it.
static WRITING: Mutex<()> = Mutex::new(());

fn one_writer() -> Result<std::sync::MutexGuard<'static, ()>, Failure> {
    match WRITING.try_lock() {
        Ok(g) => Ok(g),
        Err(std::sync::TryLockError::Poisoned(p)) => Ok(p.into_inner()),
        Err(std::sync::TryLockError::WouldBlock) => {
            Err(Failure::Refused("another write is running here; try again when it is done".into()))
        }
    }
}

fn local_edit(p: &Paths, r: &EditRequest) -> Result<Saved, Failure> {
    if r.edit.base.is_none() {
        return Err(Failure::Refused("an edit carries the version it was made from".into()));
    }
    let _one = one_writer()?;
    let stacks = load(p)?;
    let commit = ops::edit(&stacks, &r.id, &r.edit, true).map_err(Failure::Refused)?;
    Ok(Saved { id: r.id.clone(), commit })
}

fn local_retire(p: &Paths, r: &RetireRequest) -> Result<Saved, Failure> {
    let _one = one_writer()?;
    let stacks = load(p)?;
    let retired = ops::retire(
        &stacks,
        &Retire {
            id: &r.id,
            reason: "retired by the owner in Lords Command",
            superseded_by: r.superseded_by.as_deref(),
            date: ops::today(),
            commit: true,
            base: Some(&r.base),
        },
    )
    .map_err(Failure::Refused)?;
    Ok(Saved { id: r.id.clone(), commit: retired.commit })
}

/// Each stack repo's privacy pre-push hook, probed: whether a push from it is checked, and why not (review r1 R4).
/// `lords panel serve` says it at its start.
pub fn push_ready(p: &Paths) -> Vec<String> {
    let home = match sync::default_home() {
        Ok(h) => h,
        Err(e) => return vec![format!("push: no tool home ({e})")],
    };
    stack_repos(p)
        .into_iter()
        .map(|(stack, top)| match top.and_then(|t| crate::hooks::push_dispatch(&t, &home)) {
            Ok(()) => format!("{stack}: push checked by its privacy pre-push hook"),
            Err(e) => format!("{stack}: push refused until its privacy pre-push hook answers: {e}"),
        })
        .collect()
}

fn local_push(p: &Paths) -> Result<Pushed, Failure> {
    let _one = one_writer()?;
    let home = sync::default_home().map_err(Failure::Refused)?;
    let (mut lines, mut refused) = (Vec::new(), false);
    for (stack, top) in stack_repos(p) {
        let ahead = top.and_then(|t| git::ahead(&t).map(|a| (t, a)));
        match ahead {
            Err(e) => {
                lines.push(format!("{stack}: {e}"));
                refused = true;
            }
            Ok((_, (_, _, commits))) if commits.is_empty() => {}
            Ok((_, (branch, None, _))) => {
                lines.push(format!("{stack}: {branch} has no upstream; nothing pushed"));
                refused = true;
            }
            // Only through a privacy pre-push hook that answers its probe: a plain push that succeeds is no sign the
            // check ran (review r1 R4).
            Ok((top, (branch, Some(up), commits))) => {
                match crate::hooks::push_dispatch(&top, &home)
                    .and_then(|()| git::push(&top, PUSH_EACH))
                {
                    Ok(_) => lines
                        .push(format!("{stack}: {} commit(s) of {branch} to {up}", commits.len())),
                    Err(e) => {
                        lines.push(format!("{stack}: refused: {e}"));
                        refused = true;
                    }
                }
            }
        }
    }
    if refused {
        Err(Failure::Refused(lines.join("\n")))
    } else {
        Ok(Pushed { lines })
    }
}
