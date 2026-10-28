//! `lords-mcp`: Lords Command's MCP server over stdio (v2 design §2.1, §2.2). Its tools return exactly what
//! `lords_core::serve` returns, as JSON text, so the CLI and this server give the same ids and bodies. The server's
//! own working dir selects nothing: every call names the delivery id `lords brief` issued for the session.
//!
//! usage: lords-mcp [--registry DIR]   (else $LORDS_HOME, else ~/.lords)

use lords_core::process::ExitCode;
use lords_core::serve::{self, Note};
use rmcp::handler::server::{router::tool::ToolRouter, wrapper::Parameters};
use rmcp::model::{ServerCapabilities, ServerConfig};
use rmcp::{schemars, tool, tool_handler, tool_router, ServerHandler, ServiceExt};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ContextArgs {
    #[schemars(description = "your assigned role, as your brief names it")]
    role: String,
    #[schemars(description = "this session's delivery id, from its brief")]
    delivery_id: String,
    #[schemars(description = "the byte budget for duties' text; duties past it are listed by id")]
    budget: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SearchArgs {
    #[schemars(description = "this session's delivery id, from its brief")]
    delivery_id: String,
    #[schemars(description = "words to find in an entry's id, title, topics or text")]
    query: Option<String>,
    #[schemars(description = "entry ids to fetch whole")]
    ids: Option<Vec<String>>,
    #[schemars(description = "also expired entries, labelled")]
    include_expired: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RememberArgs {
    #[schemars(description = "this session's delivery id, from its brief")]
    delivery_id: String,
    #[schemars(description = "the note")]
    text: String,
    title: Option<String>,
    #[schemars(description = "memory (default) or fact")]
    kind: Option<String>,
    #[schemars(description = "YYYY-MM-DD: the note ends at the start of this UTC day")]
    until: Option<String>,
    #[schemars(description = "YYYY-MM-DD: after this day the note is shown as due for review")]
    review_by: Option<String>,
    #[schemars(description = "why the note is temporary; required with until or review_by")]
    reason: Option<String>,
}

#[derive(Debug, Clone)]
struct Lords {
    home: PathBuf,
    #[allow(dead_code)] // read by the `tool_handler` expansion
    tool_router: ToolRouter<Self>,
}

fn json(v: Result<impl serde::Serialize, String>) -> Result<String, String> {
    v.and_then(|v| serde_json::to_string_pretty(&v).map_err(|e| e.to_string()))
}

#[tool_router]
impl Lords {
    fn new(home: PathBuf) -> Self {
        Lords { home, tool_router: Self::tool_router() }
    }

    #[tool(
        description = "Your role's duties, the index of everything else for this session's project, and the date \
                          it is valid until. Call it before the first task action, and again after compaction or a \
                          role or location change."
    )]
    fn context(&self, Parameters(a): Parameters<ContextArgs>) -> Result<String, String> {
        json(serve::context(
            &self.home,
            &a.delivery_id,
            &a.role,
            a.budget,
            &lords_core::ops::today(),
        ))
    }

    #[tool(
        description = "Entries of this session's project and the core that match a query, or the ids given, \
                          with their text."
    )]
    fn search(&self, Parameters(a): Parameters<SearchArgs>) -> Result<String, String> {
        json(serve::search(
            &self.home,
            &a.delivery_id,
            a.query.as_deref().unwrap_or(""),
            &a.ids.unwrap_or_default(),
            a.include_expired.unwrap_or(false),
            &lords_core::ops::today(),
        ))
    }

    #[tool(
        description = "Write a memory or fact to this session's project store (orchestrators and coders only). \
                          A temporary note needs a reason and until or review_by."
    )]
    fn remember(&self, Parameters(a): Parameters<RememberArgs>) -> Result<String, String> {
        let note = Note {
            text: a.text,
            title: a.title,
            kind: a.kind,
            until: a.until,
            review_by: a.review_by,
            reason: a.reason,
        };
        json(serve::remember(&self.home, &a.delivery_id, &note, &lords_core::ops::today()))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Lords {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }
}

fn home() -> Result<PathBuf, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => lords_core::identity::default_home()
            .ok_or_else(|| "no home dir: pass --registry DIR".into()),
        [flag, dir] if flag == "--registry" => Ok(PathBuf::from(dir)),
        _ => Err("usage: lords-mcp [--registry DIR]".into()),
    }
}

fn main() -> ExitCode {
    let home = match home() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("lords-mcp: {e}");
            return ExitCode::from(2);
        }
    };
    let rt = match tokio::runtime::Builder::new_current_thread().enable_io().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("lords-mcp: {e}");
            return ExitCode::from(2);
        }
    };
    let ran = rt.block_on(async {
        let running =
            Lords::new(home).serve(rmcp::transport::stdio()).await.map_err(|e| e.to_string())?;
        running.waiting().await.map(|_| ()).map_err(|e| e.to_string())
    });
    match ran {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("lords-mcp: {e}");
            ExitCode::from(1)
        }
    }
}
