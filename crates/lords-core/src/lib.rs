//! Lords Command's core: stacks of entries, their check, privacy and the commands that change them.
//! The CLI and the GUI both call this; neither shells out to the other.

pub mod adapters;
pub mod check;
pub mod compose;
pub mod config;
pub mod diff;
pub mod finding;
pub mod frontmatter;
pub mod git;
pub mod hooks;
pub mod model;
pub mod ops;
pub mod overlap;
pub mod privacy;
pub mod schema;
pub mod stack;
pub mod sync;
pub mod view;

pub use finding::{Finding, Severity};
pub use stack::{Stack, Stacks};
