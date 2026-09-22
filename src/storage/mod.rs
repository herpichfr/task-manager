use thiserror::Error;

use crate::domain::note::{Note, NoteId};
use crate::domain::task::{NewTask, Status, Task, TaskId, TaskPatch};

pub mod locked_db;
pub mod main_db;
pub mod migrations;
mod task_store_impl;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("not found")]
    NotFound,
    #[error("board already exists: {0}")]
    BoardExists(String),
    #[error("invalid enum value: {0}")]
    InvalidEnum(String),
    #[error("wrong passphrase")]
    WrongPassphrase,
}

/// Storage operations for the tasks and notes of a single board. Implemented
/// for both a plain board's table in the shared main database and a locked
/// board's own encrypted database file, so this trait must stay
/// object-safe.
pub trait TaskStore {
    fn list_tasks(&self, status: Status) -> Result<Vec<Task>, StorageError>;
    fn get_task(&self, id: TaskId) -> Result<Task, StorageError>;
    fn create_task(&self, draft: NewTask) -> Result<TaskId, StorageError>;
    fn update_task(&self, id: TaskId, patch: TaskPatch) -> Result<(), StorageError>;
    fn move_task(&self, id: TaskId, to: Status, index: i64) -> Result<(), StorageError>;
    fn reorder(&self, status: Status, ordered: &[TaskId]) -> Result<(), StorageError>;
    fn delete_task(&self, id: TaskId) -> Result<(), StorageError>;
    fn list_notes(&self) -> Result<Vec<Note>, StorageError>;
    fn create_note(&self, body: &str) -> Result<NoteId, StorageError>;
    fn update_note(&self, id: NoteId, body: &str) -> Result<(), StorageError>;
    fn delete_note(&self, id: NoteId) -> Result<(), StorageError>;
    fn promote_note(&self, id: NoteId, status: Status) -> Result<TaskId, StorageError>;
}

/// Restricts a database file to owner-only access (`0600`).
///
/// SQLite derives the permissions of the `-wal` and `-shm` sidecar files
/// from the main database file, so hardening this one path covers them
/// too. The plain `main.db` holds unlocked boards' tasks and notes in
/// cleartext, and a locked board's file, while encrypted, still reveals
/// its size and modification pattern -- neither belongs to every account
/// on the machine, which is what the default umask would grant.
#[cfg(unix)]
pub(crate) fn harden_file(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if path.exists() {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn harden_file(_path: &std::path::Path) -> std::io::Result<()> {
    Ok(())
}

/// Restricts the data directory to owner-only access (`0700`), so the
/// names of a user's boards cannot be listed by other accounts.
#[cfg(unix)]
pub(crate) fn harden_dir(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if path.is_dir() {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn harden_dir(_path: &std::path::Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod object_safety {
    use super::TaskStore;

    /// Compiles only if `TaskStore` is object-safe.
    #[allow(dead_code)]
    fn assert_object_safe(_store: &dyn TaskStore) {}
}
