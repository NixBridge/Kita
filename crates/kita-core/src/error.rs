use std::fmt;

/// A structured, machine-readable Kita diagnostic.
///
/// Error codes follow docs/07-rust-implementation.md §12. Codes are stable and
/// must never be reused for a different meaning.
#[derive(Debug, Clone)]
pub struct KitaError {
    pub code: &'static str,
    pub message: String,
    pub hints: Vec<String>,
}

impl KitaError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            hints: Vec::new(),
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hints.push(hint.into());
        self
    }

    pub fn io(code: &'static str, context: impl AsRef<str>, error: std::io::Error) -> Self {
        Self::new(code, format!("{}: {}", context.as_ref(), error))
    }
}

impl fmt::Display for KitaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "error[{}]: {}", self.code, self.message)?;
        for hint in &self.hints {
            write!(f, "\n  help: {hint}")?;
        }
        Ok(())
    }
}

impl std::error::Error for KitaError {}

pub type Result<T> = std::result::Result<T, KitaError>;
