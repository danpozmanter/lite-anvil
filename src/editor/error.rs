/// Errors from the regex subsystem.
#[derive(Debug, thiserror::Error)]
pub enum RegexError {
    #[error("regex compilation failed: {0}")]
    Compile(String),
    #[error("regex match error: {0}")]
    Match(String),
}
