use std::fmt;

#[derive(Debug)]
pub enum Error {
    Rejected(String),
    Internal(String),
}
impl Error {
    pub fn rejected(message: impl Into<String>) -> Self {
        Self::Rejected(message.into())
    }
    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal(message.into())
    }
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Rejected(_) => "rejected",
            Self::Internal(_) => "internal",
        }
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rejected(message) | Self::Internal(message) => f.write_str(message),
        }
    }
}
impl std::error::Error for Error {}
pub type Result<T, E = Error> = std::result::Result<T, E>;
