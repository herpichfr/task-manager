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

const MAIN_V2: &str = "
ALTER TABLE tasks ADD COLUMN start_date INTEGER;
ALTER TABLE tasks ADD COLUMN deadline INTEGER;
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

const BOARD_V2: &str = "
ALTER TABLE tasks ADD COLUMN start_date INTEGER;
ALTER TABLE tasks ADD COLUMN deadline INTEGER;
";

// v3 adds `tasks.completed_at` (nullable unix seconds, stamped whenever a
// task enters `Status::Done` and cleared when it leaves) to both schemas.
// The backfill gives every pre-existing Done row its `updated_at` as a
// reasonable completion time rather than leaving it `NULL` -- a `NULL`
// `completed_at` on a Done row would otherwise never be auto-archived and
// would sort last in the Done column instead of by recency.
const MAIN_V3: &str = "
ALTER TABLE tasks ADD COLUMN completed_at INTEGER;
UPDATE tasks SET completed_at = updated_at WHERE status = 'done';
";

const BOARD_V3: &str = "
ALTER TABLE tasks ADD COLUMN completed_at INTEGER;
UPDATE tasks SET completed_at = updated_at WHERE status = 'done';
";

pub const MIGRATIONS_MAIN: &[(i64, &str)] = &[(1, MAIN_V1), (2, MAIN_V2), (3, MAIN_V3)];
pub const MIGRATIONS_BOARD: &[(i64, &str)] = &[(1, BOARD_V1), (2, BOARD_V2), (3, BOARD_V3)];

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

#[cfg(test)]
mod tests {
    use super::*;

    /// A v1 database (main and board schemas both) with existing rows must
    /// upgrade to v2 in place: the rows survive, and the two new columns
    /// come back NULL for them.
    #[test]
    fn v1_to_v2_migration_preserves_existing_rows_main() {
        let conn = Connection::open_in_memory().unwrap();
        apply(&conn, &MIGRATIONS_MAIN[0..1]).unwrap();
        assert_eq!(current_version(&conn).unwrap(), 1);

        conn.execute(
            "INSERT INTO boards (name, kind, position, created_at, updated_at)
             VALUES ('alpha', 'plain', 0, 0, 0)",
            [],
        )
        .unwrap();
        let board_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO tasks (board_id, title, body, status, priority, position, created_at, updated_at)
             VALUES (?1, 'pre-existing task', 'body text', 'todo', 'normal', 0, 0, 0)",
            rusqlite::params![board_id],
        )
        .unwrap();
        let task_id = conn.last_insert_rowid();

        apply(&conn, MIGRATIONS_MAIN).unwrap();
        assert_eq!(current_version(&conn).unwrap(), 3);

        let (title, start_date, deadline): (String, Option<i64>, Option<i64>) = conn
            .query_row(
                "SELECT title, start_date, deadline FROM tasks WHERE id = ?1",
                rusqlite::params![task_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(title, "pre-existing task");
        assert_eq!(start_date, None);
        assert_eq!(deadline, None);

        let board_name: String = conn
            .query_row("SELECT name FROM boards WHERE id = ?1", rusqlite::params![board_id], |row| row.get(0))
            .unwrap();
        assert_eq!(board_name, "alpha");
    }

    #[test]
    fn v1_to_v2_migration_preserves_existing_rows_board() {
        let conn = Connection::open_in_memory().unwrap();
        apply(&conn, &MIGRATIONS_BOARD[0..1]).unwrap();
        assert_eq!(current_version(&conn).unwrap(), 1);

        conn.execute(
            "INSERT INTO tasks (title, body, status, priority, position, created_at, updated_at)
             VALUES ('pre-existing task', 'body text', 'todo', 'normal', 0, 0, 0)",
            [],
        )
        .unwrap();
        let task_id = conn.last_insert_rowid();

        apply(&conn, MIGRATIONS_BOARD).unwrap();
        assert_eq!(current_version(&conn).unwrap(), 3);

        let (title, start_date, deadline): (String, Option<i64>, Option<i64>) = conn
            .query_row(
                "SELECT title, start_date, deadline FROM tasks WHERE id = ?1",
                rusqlite::params![task_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(title, "pre-existing task");
        assert_eq!(start_date, None);
        assert_eq!(deadline, None);
    }

    #[test]
    fn migrations_are_idempotent_and_versioned() {
        let conn = Connection::open_in_memory().unwrap();
        assert_eq!(current_version(&conn).unwrap(), 0);
        apply(&conn, MIGRATIONS_MAIN).unwrap();
        assert_eq!(current_version(&conn).unwrap(), 3);
        apply(&conn, MIGRATIONS_MAIN).unwrap();
        assert_eq!(current_version(&conn).unwrap(), 3);
    }

    /// A v2 database with an existing Done row must upgrade to v3 in
    /// place: the row survives, and its `completed_at` is backfilled from
    /// its `updated_at` -- a non-Done row's `completed_at` stays `NULL`.
    #[test]
    fn v2_to_v3_migration_backfills_completed_at_for_done_rows_main() {
        let conn = Connection::open_in_memory().unwrap();
        apply(&conn, &MIGRATIONS_MAIN[0..2]).unwrap();
        assert_eq!(current_version(&conn).unwrap(), 2);

        conn.execute(
            "INSERT INTO boards (name, kind, position, created_at, updated_at)
             VALUES ('alpha', 'plain', 0, 0, 0)",
            [],
        )
        .unwrap();
        let board_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO tasks (board_id, title, body, status, priority, position, created_at, updated_at)
             VALUES (?1, 'done task', '', 'done', 'normal', 0, 100, 500)",
            rusqlite::params![board_id],
        )
        .unwrap();
        let done_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO tasks (board_id, title, body, status, priority, position, created_at, updated_at)
             VALUES (?1, 'todo task', '', 'todo', 'normal', 1, 100, 500)",
            rusqlite::params![board_id],
        )
        .unwrap();
        let todo_id = conn.last_insert_rowid();

        apply(&conn, MIGRATIONS_MAIN).unwrap();
        assert_eq!(current_version(&conn).unwrap(), 3);

        let done_completed: Option<i64> = conn
            .query_row(
                "SELECT completed_at FROM tasks WHERE id = ?1",
                rusqlite::params![done_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(done_completed, Some(500));

        let todo_completed: Option<i64> = conn
            .query_row(
                "SELECT completed_at FROM tasks WHERE id = ?1",
                rusqlite::params![todo_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(todo_completed, None);

        let title: String = conn
            .query_row("SELECT title FROM tasks WHERE id = ?1", rusqlite::params![done_id], |row| row.get(0))
            .unwrap();
        assert_eq!(title, "done task");
    }

    #[test]
    fn v2_to_v3_migration_backfills_completed_at_for_done_rows_board() {
        let conn = Connection::open_in_memory().unwrap();
        apply(&conn, &MIGRATIONS_BOARD[0..2]).unwrap();
        assert_eq!(current_version(&conn).unwrap(), 2);

        conn.execute(
            "INSERT INTO tasks (title, body, status, priority, position, created_at, updated_at)
             VALUES ('done task', '', 'done', 'normal', 0, 100, 500)",
            [],
        )
        .unwrap();
        let done_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO tasks (title, body, status, priority, position, created_at, updated_at)
             VALUES ('todo task', '', 'todo', 'normal', 1, 100, 500)",
            [],
        )
        .unwrap();
        let todo_id = conn.last_insert_rowid();

        apply(&conn, MIGRATIONS_BOARD).unwrap();
        assert_eq!(current_version(&conn).unwrap(), 3);

        let done_completed: Option<i64> = conn
            .query_row(
                "SELECT completed_at FROM tasks WHERE id = ?1",
                rusqlite::params![done_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(done_completed, Some(500));

        let todo_completed: Option<i64> = conn
            .query_row(
                "SELECT completed_at FROM tasks WHERE id = ?1",
                rusqlite::params![todo_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(todo_completed, None);
    }
}
