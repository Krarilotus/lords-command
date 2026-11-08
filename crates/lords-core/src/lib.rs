//! Lords Command's core: stacks of entries, their check, privacy and the commands that change them.
//! The CLI and the GUI both call this; neither shells out to the other.

pub mod adapters;
pub mod check;
pub mod codex_trust;
pub mod compose;
pub mod config;
pub mod core_lines;
pub mod deliver;
pub mod diff;
pub mod finding;
pub mod frontmatter;
pub mod fsguard;
pub mod git;
pub mod guard;
pub mod helper_guard;
pub mod hooks;
pub mod http;
pub mod ledger;
pub mod model;
pub mod ops;
pub mod overlap;
pub mod panel;
pub mod paths;
pub mod privacy;
pub mod process;
pub mod schema;
pub mod stack;
pub mod sync;
pub mod t3;
pub mod temporary;
pub mod time_guard;
pub mod view;

pub use finding::{Finding, Severity};
pub use stack::{Stack, Stacks};
