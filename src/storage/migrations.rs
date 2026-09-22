use rusqlite::Connection;

use crate::storage::StorageError;

const MAIN_V1: &str = "
CREATE TABLE boards (
  id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE,
  kind TEXT NOT NULL CHECK (kind IN ('plain','locked')),
  db_path TEXT, kdf_salt BLOB, kdf_m_cost INTEGER, kdf_t_cost INTEGER, kdf_p_cost INTEGER,
  position INTEGER NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
CREATE TABLE tasks (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  board_id INTEGER NOT NULL REFERENCES boards(id) ON DELETE CASCADE,
  title TEXT NOT NULL, body TEXT NOT NULL DEFAULT '',
  status TEXT NOT NULL DEFAULT 'todo' CHECK (status IN ('todo','doing','done')),
  priority TEXT NOT NULL DEFAULT 'normal' CHECK (priority IN ('low','normal','high','urgent')),
  position INTEGER NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
CREATE INDEX idx_tasks_board_status ON tasks(board_id, status, position);
CREATE TABLE tags (id INTEGER PRIMARY KEY AUTOINCREMENT,
  board_id INTEGER NOT NULL REFERENCES boards(id) ON DELETE CASCADE,
  name TEXT NOT NULL, color TEXT, UNIQUE(board_id, name));
CREATE TABLE task_tags (task_id INTEGER NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
  tag_id INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE, PRIMARY KEY (task_id, tag_id));
CREATE TABLE notes (id INTEGER PRIMARY KEY AUTOINCREMENT,
  board_id INTEGER NOT NULL REFERENCES boards(id) ON DELETE CASCADE, body TEXT NOT NULL,
  promoted_task_id INTEGER REFERENCES tasks(id) ON DELETE SET NULL,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
CREATE INDEX idx_notes_board ON notes(board_id, created_at);
";

const BOARD_V1: &str = "
CREATE TABLE board_meta (id INTEGER PRIMARY KEY CHECK (id = 1), name TEXT NOT NULL, created_at INTEGER NOT NULL);
CREATE TABLE tasks (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  title TEXT NOT NULL, body TEXT NOT NULL DEFAULT '',
  status TEXT NOT NULL DEFAULT 'todo' CHECK (status IN ('todo','doing','done')),
  priority TEXT NOT NULL DEFAULT 'normal' CHECK (priority IN ('low','normal','high','urgent')),
  position INTEGER NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
CREATE INDEX idx_tasks_status ON tasks(status, position);
CREATE TABLE tags (id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT NOT NULL UNIQUE, color TEXT);
CREATE TABLE task_tags (task_id INTEGER NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
  tag_id INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE, PRIMARY KEY (task_id, tag_id));
CREATE TABLE notes (id INTEGER PRIMARY KEY AUTOINCREMENT, body TEXT NOT NULL,
  promoted_task_id INTEGER REFERENCES tasks(id) ON DELETE SET NULL,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
CREATE INDEX idx_notes_created ON notes(created_at);
";

pub const MIGRATIONS_MAIN: &[(i64, &str)] = &[(1, MAIN_V1)];
pub const MIGRATIONS_BOARD: &[(i64, &str)] = &[(1, BOARD_V1)];

const CREATE_VERSION_TABLE: &str = "CREATE TABLE IF NOT EXISTS schema_version (
    version INTEGER PRIMARY KEY,
    applied_at INTEGER NOT NULL
);";

/// Applies every migration in `set` whose version is greater than the
/// database's current schema version, in ascending order. Each migration
/// runs inside its own transaction alongside the `schema_version` row that
/// records it, so a failure leaves the database at the previous version.
/// Applying the same set twice is a no-op.
pub fn apply(conn: &Connection, set: &[(i64, &str)]) -> Result<(), StorageError> {
    conn.execute_batch(CREATE_VERSION_TABLE)?;
    let current = current_version(conn)?;

    let mut pending: Vec<&(i64, &str)> = set.iter().filter(|(version, _)| *version > current).collect();
    pending.sort_by_key(|(version, _)| *version);

    for (version, sql) in pending {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(sql)?;
        tx.execute(
            "INSERT INTO schema_version (version, applied_at) VALUES (?1, ?2)",
            rusqlite::params![version, chrono::Utc::now().timestamp()],
        )?;
        tx.commit()?;
    }

    Ok(())
}

/// Returns the database's current schema version (0 if no migration has
/// ever been applied).
pub fn current_version(conn: &Connection) -> Result<i64, StorageError> {
    conn.execute_batch(CREATE_VERSION_TABLE)?;
    let version: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_version",
        [],
        |row| row.get(0),
    )?;
    Ok(version)
}
