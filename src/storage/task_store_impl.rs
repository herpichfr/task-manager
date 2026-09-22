//! Shared SQL and business logic behind `TaskStore`, used by both
//! `main_db::PlainBoardStore` (rows filtered by `board_id` in the shared
//! main database) and `locked_db::LockedBoardStore` (a locked board's own
//! file, whose tables have no `board_id` column at all). Every function
//! here takes `board_id: Option<BoardId>`: `Some` adds `AND board_id = ?`
//! to the relevant WHERE clause and binds it; `None` omits it entirely.
//! Neither schema is ever queried *for* its `board_id` column -- the
//! caller already knows the value (or knows there isn't one) -- so one
//! column list serves both, and this is the one place the position/dense-
//! repacking arithmetic and promote-note splitting logic exist.

use rusqlite::Connection;

use crate::domain::board::BoardId;
use crate::domain::note::{Note, NoteId};
use crate::domain::task::{NewTask, Priority, Status, Task, TaskId, TaskPatch};
use crate::storage::StorageError;

type RawTaskRow = (TaskId, String, String, String, String, i64, i64, i64);

fn row_to_raw_task(row: &rusqlite::Row) -> rusqlite::Result<RawTaskRow> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?))
}

fn task_from_parts(board_id: Option<BoardId>, raw: RawTaskRow) -> Result<Task, StorageError> {
    let (id, title, body, status, priority, position, created_at, updated_at) = raw;
    let status = Status::from_str(&status).ok_or_else(|| StorageError::InvalidEnum(status.clone()))?;
    let priority = Priority::from_str(&priority).ok_or_else(|| StorageError::InvalidEnum(priority.clone()))?;
    Ok(Task { id, board_id, title, body, status, priority, position, created_at, updated_at })
}

type RawNoteRow = (NoteId, String, Option<TaskId>, i64, i64);

fn row_to_raw_note(row: &rusqlite::Row) -> rusqlite::Result<RawNoteRow> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
}

fn note_from_parts(board_id: Option<BoardId>, raw: RawNoteRow) -> Note {
    let (id, body, promoted_task_id, created_at, updated_at) = raw;
    Note { id, board_id, body, promoted_task_id, created_at, updated_at }
}

pub(crate) fn list_tasks(conn: &Connection, board_id: Option<BoardId>, status: Status) -> Result<Vec<Task>, StorageError> {
    let sql = match board_id {
        Some(_) => "SELECT id, title, body, status, priority, position, created_at, updated_at
                     FROM tasks WHERE board_id = ?1 AND status = ?2 ORDER BY position",
        None => "SELECT id, title, body, status, priority, position, created_at, updated_at
                  FROM tasks WHERE status = ?1 ORDER BY position",
    };
    let mut stmt = conn.prepare(sql)?;
    let rows: Vec<RawTaskRow> = if let Some(bid) = board_id {
        stmt.query_map(rusqlite::params![bid, status.as_str()], row_to_raw_task)?.collect::<Result<_, _>>()?
    } else {
        stmt.query_map(rusqlite::params![status.as_str()], row_to_raw_task)?.collect::<Result<_, _>>()?
    };
    rows.into_iter().map(|r| task_from_parts(board_id, r)).collect()
}

pub(crate) fn get_task(conn: &Connection, board_id: Option<BoardId>, id: TaskId) -> Result<Task, StorageError> {
    let sql = match board_id {
        Some(_) => "SELECT id, title, body, status, priority, position, created_at, updated_at
                     FROM tasks WHERE id = ?1 AND board_id = ?2",
        None => "SELECT id, title, body, status, priority, position, created_at, updated_at
                  FROM tasks WHERE id = ?1",
    };
    let raw: Result<RawTaskRow, rusqlite::Error> = if let Some(bid) = board_id {
        conn.query_row(sql, rusqlite::params![id, bid], row_to_raw_task)
    } else {
        conn.query_row(sql, rusqlite::params![id], row_to_raw_task)
    };
    match raw {
        Ok(r) => task_from_parts(board_id, r),
        Err(rusqlite::Error::QueryReturnedNoRows) => Err(StorageError::NotFound),
        Err(e) => Err(e.into()),
    }
}

pub(crate) fn create_task(conn: &Connection, board_id: Option<BoardId>, draft: NewTask) -> Result<TaskId, StorageError> {
    let now = chrono::Utc::now().timestamp();
    let next_position: i64 = match board_id {
        Some(bid) => conn.query_row(
            "SELECT COALESCE(MAX(position), -1) + 1 FROM tasks WHERE board_id = ?1 AND status = ?2",
            rusqlite::params![bid, draft.status.as_str()],
            |row| row.get(0),
        )?,
        None => conn.query_row(
            "SELECT COALESCE(MAX(position), -1) + 1 FROM tasks WHERE status = ?1",
            rusqlite::params![draft.status.as_str()],
            |row| row.get(0),
        )?,
    };
    match board_id {
        Some(bid) => conn.execute(
            "INSERT INTO tasks (board_id, title, body, status, priority, position, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            rusqlite::params![bid, draft.title, draft.body, draft.status.as_str(), draft.priority.as_str(), next_position, now],
        )?,
        None => conn.execute(
            "INSERT INTO tasks (title, body, status, priority, position, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            rusqlite::params![draft.title, draft.body, draft.status.as_str(), draft.priority.as_str(), next_position, now],
        )?,
    };
    Ok(conn.last_insert_rowid())
}

pub(crate) fn update_task(conn: &Connection, board_id: Option<BoardId>, id: TaskId, patch: TaskPatch) -> Result<(), StorageError> {
    let mut set_clauses: Vec<&str> = Vec::new();
    let mut values: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(title) = patch.title {
        set_clauses.push("title = ?");
        values.push(Box::new(title));
    }
    if let Some(body) = patch.body {
        set_clauses.push("body = ?");
        values.push(Box::new(body));
    }
    if let Some(status) = patch.status {
        set_clauses.push("status = ?");
        values.push(Box::new(status.as_str()));
    }
    if let Some(priority) = patch.priority {
        set_clauses.push("priority = ?");
        values.push(Box::new(priority.as_str()));
    }
    set_clauses.push("updated_at = ?");
    values.push(Box::new(chrono::Utc::now().timestamp()));

    values.push(Box::new(id));
    let where_clause = match board_id {
        Some(bid) => {
            values.push(Box::new(bid));
            "WHERE id = ? AND board_id = ?"
        }
        None => "WHERE id = ?",
    };

    let sql = format!("UPDATE tasks SET {} {}", set_clauses.join(", "), where_clause);
    let params: Vec<&dyn rusqlite::ToSql> = values.iter().map(|v| v.as_ref()).collect();
    let changed = conn.execute(&sql, params.as_slice())?;
    if changed == 0 {
        return Err(StorageError::NotFound);
    }
    Ok(())
}

pub(crate) fn move_task(conn: &Connection, board_id: Option<BoardId>, id: TaskId, to: Status, index: i64) -> Result<(), StorageError> {
    let tx = conn.unchecked_transaction()?;

    let select_sql = match board_id {
        Some(_) => "SELECT status, position FROM tasks WHERE id = ?1 AND board_id = ?2",
        None => "SELECT status, position FROM tasks WHERE id = ?1",
    };
    let (from_status_s, from_position): (String, i64) = if let Some(bid) = board_id {
        tx.query_row(select_sql, rusqlite::params![id, bid], |row| Ok((row.get(0)?, row.get(1)?)))
    } else {
        tx.query_row(select_sql, rusqlite::params![id], |row| Ok((row.get(0)?, row.get(1)?)))
    }
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => StorageError::NotFound,
        other => StorageError::from(other),
    })?;
    let from_status = Status::from_str(&from_status_s).ok_or_else(|| StorageError::InvalidEnum(from_status_s.clone()))?;

    let close_gap_sql = match board_id {
        Some(_) => "UPDATE tasks SET position = position - 1 WHERE board_id = ?1 AND status = ?2 AND position > ?3",
        None => "UPDATE tasks SET position = position - 1 WHERE status = ?1 AND position > ?2",
    };
    if let Some(bid) = board_id {
        tx.execute(close_gap_sql, rusqlite::params![bid, from_status.as_str(), from_position])?;
    } else {
        tx.execute(close_gap_sql, rusqlite::params![from_status.as_str(), from_position])?;
    }

    let count_sql = match board_id {
        Some(_) => "SELECT COUNT(*) FROM tasks WHERE board_id = ?1 AND status = ?2 AND id != ?3",
        None => "SELECT COUNT(*) FROM tasks WHERE status = ?1 AND id != ?2",
    };
    let target_count: i64 = if let Some(bid) = board_id {
        tx.query_row(count_sql, rusqlite::params![bid, to.as_str(), id], |row| row.get(0))?
    } else {
        tx.query_row(count_sql, rusqlite::params![to.as_str(), id], |row| row.get(0))?
    };
    let clamped_index = index.clamp(0, target_count);

    let open_slot_sql = match board_id {
        Some(_) => "UPDATE tasks SET position = position + 1 WHERE board_id = ?1 AND status = ?2 AND position >= ?3 AND id != ?4",
        None => "UPDATE tasks SET position = position + 1 WHERE status = ?1 AND position >= ?2 AND id != ?3",
    };
    if let Some(bid) = board_id {
        tx.execute(open_slot_sql, rusqlite::params![bid, to.as_str(), clamped_index, id])?;
    } else {
        tx.execute(open_slot_sql, rusqlite::params![to.as_str(), clamped_index, id])?;
    }

    let now = chrono::Utc::now().timestamp();
    tx.execute(
        "UPDATE tasks SET status = ?1, position = ?2, updated_at = ?3 WHERE id = ?4",
        rusqlite::params![to.as_str(), clamped_index, now, id],
    )?;

    tx.commit()?;
    Ok(())
}

pub(crate) fn reorder(conn: &Connection, board_id: Option<BoardId>, status: Status, ordered: &[TaskId]) -> Result<(), StorageError> {
    let tx = conn.unchecked_transaction()?;
    let sql = match board_id {
        Some(_) => "UPDATE tasks SET position = ?1 WHERE id = ?2 AND board_id = ?3 AND status = ?4",
        None => "UPDATE tasks SET position = ?1 WHERE id = ?2 AND status = ?3",
    };
    for (idx, task_id) in ordered.iter().enumerate() {
        let changed = if let Some(bid) = board_id {
            tx.execute(sql, rusqlite::params![idx as i64, task_id, bid, status.as_str()])?
        } else {
            tx.execute(sql, rusqlite::params![idx as i64, task_id, status.as_str()])?
        };
        if changed == 0 {
            return Err(StorageError::NotFound);
        }
    }
    tx.commit()?;
    Ok(())
}

pub(crate) fn delete_task(conn: &Connection, board_id: Option<BoardId>, id: TaskId) -> Result<(), StorageError> {
    let tx = conn.unchecked_transaction()?;
    let select_sql = match board_id {
        Some(_) => "SELECT status, position FROM tasks WHERE id = ?1 AND board_id = ?2",
        None => "SELECT status, position FROM tasks WHERE id = ?1",
    };
    let (status_s, position): (String, i64) = if let Some(bid) = board_id {
        tx.query_row(select_sql, rusqlite::params![id, bid], |row| Ok((row.get(0)?, row.get(1)?)))
    } else {
        tx.query_row(select_sql, rusqlite::params![id], |row| Ok((row.get(0)?, row.get(1)?)))
    }
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => StorageError::NotFound,
        other => StorageError::from(other),
    })?;
    tx.execute("DELETE FROM tasks WHERE id = ?1", rusqlite::params![id])?;
    let gap_sql = match board_id {
        Some(_) => "UPDATE tasks SET position = position - 1 WHERE board_id = ?1 AND status = ?2 AND position > ?3",
        None => "UPDATE tasks SET position = position - 1 WHERE status = ?1 AND position > ?2",
    };
    if let Some(bid) = board_id {
        tx.execute(gap_sql, rusqlite::params![bid, status_s, position])?;
    } else {
        tx.execute(gap_sql, rusqlite::params![status_s, position])?;
    }
    tx.commit()?;
    Ok(())
}

pub(crate) fn list_notes(conn: &Connection, board_id: Option<BoardId>) -> Result<Vec<Note>, StorageError> {
    let sql = match board_id {
        Some(_) => "SELECT id, body, promoted_task_id, created_at, updated_at FROM notes WHERE board_id = ?1 ORDER BY created_at",
        None => "SELECT id, body, promoted_task_id, created_at, updated_at FROM notes ORDER BY created_at",
    };
    let mut stmt = conn.prepare(sql)?;
    let rows: Vec<RawNoteRow> = if let Some(bid) = board_id {
        stmt.query_map(rusqlite::params![bid], row_to_raw_note)?.collect::<Result<_, _>>()?
    } else {
        stmt.query_map([], row_to_raw_note)?.collect::<Result<_, _>>()?
    };
    Ok(rows.into_iter().map(|r| note_from_parts(board_id, r)).collect())
}

pub(crate) fn create_note(conn: &Connection, board_id: Option<BoardId>, body: &str) -> Result<NoteId, StorageError> {
    let now = chrono::Utc::now().timestamp();
    match board_id {
        Some(bid) => conn.execute(
            "INSERT INTO notes (board_id, body, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
            rusqlite::params![bid, body, now],
        )?,
        None => conn.execute(
            "INSERT INTO notes (body, created_at, updated_at) VALUES (?1, ?2, ?2)",
            rusqlite::params![body, now],
        )?,
    };
    Ok(conn.last_insert_rowid())
}

pub(crate) fn update_note(conn: &Connection, board_id: Option<BoardId>, id: NoteId, body: &str) -> Result<(), StorageError> {
    let now = chrono::Utc::now().timestamp();
    let changed = match board_id {
        Some(bid) => conn.execute(
            "UPDATE notes SET body = ?1, updated_at = ?2 WHERE id = ?3 AND board_id = ?4",
            rusqlite::params![body, now, id, bid],
        )?,
        None => conn.execute(
            "UPDATE notes SET body = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![body, now, id],
        )?,
    };
    if changed == 0 {
        return Err(StorageError::NotFound);
    }
    Ok(())
}

pub(crate) fn delete_note(conn: &Connection, board_id: Option<BoardId>, id: NoteId) -> Result<(), StorageError> {
    let changed = match board_id {
        Some(bid) => conn.execute("DELETE FROM notes WHERE id = ?1 AND board_id = ?2", rusqlite::params![id, bid])?,
        None => conn.execute("DELETE FROM notes WHERE id = ?1", rusqlite::params![id])?,
    };
    if changed == 0 {
        return Err(StorageError::NotFound);
    }
    Ok(())
}

pub(crate) fn promote_note(conn: &Connection, board_id: Option<BoardId>, id: NoteId, status: Status) -> Result<TaskId, StorageError> {
    let tx = conn.unchecked_transaction()?;

    let select_sql = match board_id {
        Some(_) => "SELECT body FROM notes WHERE id = ?1 AND board_id = ?2",
        None => "SELECT body FROM notes WHERE id = ?1",
    };
    let body: String = if let Some(bid) = board_id {
        tx.query_row(select_sql, rusqlite::params![id, bid], |row| row.get(0))
    } else {
        tx.query_row(select_sql, rusqlite::params![id], |row| row.get(0))
    }
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => StorageError::NotFound,
        other => StorageError::from(other),
    })?;

    let mut lines = body.splitn(2, '\n');
    let first_line = lines.next().unwrap_or("").trim();
    let title: String = first_line.chars().take(120).collect();
    let rest = lines.next().unwrap_or("").trim().to_string();

    let now = chrono::Utc::now().timestamp();
    let next_position: i64 = match board_id {
        Some(bid) => tx.query_row(
            "SELECT COALESCE(MAX(position), -1) + 1 FROM tasks WHERE board_id = ?1 AND status = ?2",
            rusqlite::params![bid, status.as_str()],
            |row| row.get(0),
        )?,
        None => tx.query_row(
            "SELECT COALESCE(MAX(position), -1) + 1 FROM tasks WHERE status = ?1",
            rusqlite::params![status.as_str()],
            |row| row.get(0),
        )?,
    };
    match board_id {
        Some(bid) => tx.execute(
            "INSERT INTO tasks (board_id, title, body, status, priority, position, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            rusqlite::params![bid, title, rest, status.as_str(), Priority::default().as_str(), next_position, now],
        )?,
        None => tx.execute(
            "INSERT INTO tasks (title, body, status, priority, position, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            rusqlite::params![title, rest, status.as_str(), Priority::default().as_str(), next_position, now],
        )?,
    };
    let task_id = tx.last_insert_rowid();

    tx.execute(
        "UPDATE notes SET promoted_task_id = ?1, updated_at = ?2 WHERE id = ?3",
        rusqlite::params![task_id, now, id],
    )?;

    tx.commit()?;
    Ok(task_id)
}
