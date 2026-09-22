//! Application error type shared across the crate.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("failed to parse configuration: {0}")]
    TomlParse(#[from] toml::de::Error),

    #[error("configuration error: {0}")]
    Config(String),

    #[error("could not determine the home directory")]
    NoHomeDirectory,

    #[error("no editor found: set `editor` in the config file, install nvim, or set $VISUAL/$EDITOR")]
    EditorNotFound,

    #[error("encryption error: {0}")]
    Crypto(String),

    #[error("wrong passphrase")]
    WrongPassphrase,
}

pub type Result<T> = std::result::Result<T, AppError>;
