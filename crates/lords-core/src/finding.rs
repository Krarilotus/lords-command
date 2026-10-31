use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    /// For the owner to decide; never resolved by the tool.
    Candidate,
    Info,
}

#[derive(Clone, Debug, Serialize)]
pub struct Finding {
    pub severity: Severity,
    pub code: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub related: Vec<String>,
}

impl Finding {
    pub fn new(severity: Severity, code: &'static str, message: impl Into<String>) -> Finding {
        Finding {
            severity,
            code,
            message: message.into(),
            file: None,
            line: None,
            entry: None,
            related: Vec::new(),
        }
    }

    pub fn error(code: &'static str, message: impl Into<String>) -> Finding {
        Finding::new(Severity::Error, code, message)
    }

    pub fn warning(code: &'static str, message: impl Into<String>) -> Finding {
        Finding::new(Severity::Warning, code, message)
    }

    pub fn candidate(code: &'static str, message: impl Into<String>) -> Finding {
        Finding::new(Severity::Candidate, code, message)
    }

    pub fn info(code: &'static str, message: impl Into<String>) -> Finding {
        Finding::new(Severity::Info, code, message)
    }

    pub fn at(mut self, file: impl Into<PathBuf>, line: Option<usize>) -> Finding {
        self.file = Some(file.into());
        self.line = line;
        self
    }

    pub fn entry(mut self, id: impl Into<String>) -> Finding {
        self.entry = Some(id.into());
        self
    }

    pub fn related(mut self, ids: impl IntoIterator<Item = String>) -> Finding {
        self.related.extend(ids);
        self
    }
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sev = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Candidate => "candidate",
            Severity::Info => "info",
        };
        write!(f, "{sev} [{}]", self.code)?;
        if let Some(file) = &self.file {
            write!(f, " {}", file.display())?;
            if let Some(line) = self.line {
                write!(f, ":{line}")?;
            }
        }
        if let Some(id) = &self.entry {
            write!(f, " ({id})")?;
        }
        write!(f, ": {}", self.message)?;
        if !self.related.is_empty() {
            write!(f, " [{}]", self.related.join(", "))?;
        }
        Ok(())
    }
}
