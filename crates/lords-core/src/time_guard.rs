//! The time guard (`lords guard time`): stub for the tests written first.

use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};

/// What a clock time in added text is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Placeholder,
    Future,
}

/// One clock time the guard objects to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub kind: Kind,
    pub token: String,
}

pub fn findings(_new: &str, _old: &str, _now: Option<&Zoned>) -> Vec<Finding> {
    Vec::new()
}

pub fn time_hook(_input: &str, _now: Timestamp, _zone: Option<&TimeZone>) -> String {
    String::new()
}
