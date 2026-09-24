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
//!
//! `tags`/`task_tags` have no `board_id` filtering concern once a `task_id`
//! or `tag_id` is in hand -- a plain board's tags row still carries its own
//! `board_id` column (used by `list_tags`/`upsert_tag`/`rename_tag`/
//! `delete_tag`, which enumerate or address tags directly), but the
//! `task_tags` junction just relates a `task_id` to a `tag_id`, both of
//! which are already scoped correctly by whoever looked them up.

use std::collections::HashMap;

use rusqlite::Connection;

use crate::domain::board::BoardId;
use crate::domain::note::{Note, NoteId};
use crate::domain::task::{NewTask, Priority, Status, Tag, TagId, Task, TaskId, TaskPatch};
use crate::storage::StorageError;

type RawTaskRow = (
    TaskId,
    String,
    String,
    String,
    String,
    i64,
    i64,
    i64,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
);

fn row_to_raw_task(row: &rusqlite::Row) -> rusqlite::Result<RawTaskRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
        row.get(12)?,
    ))
}

fn task_from_parts(board_id: Option<BoardId>, raw: RawTaskRow) -> Result<Task, StorageError> {
    let (id, title, body, status, priority, position, created_at, updated_at, start_date, deadline, completed_at, time_expected, deadline_notified_at) = raw;
    let status = Status::from_str(&status).ok_or_else(|| StorageError::InvalidEnum(status.clone()))?;
    let priority = Priority::from_str(&priority).ok_or_else(|| StorageError::InvalidEnum(priority.clone()))?;
    Ok(Task {
        id,
        board_id,
        title,
        body,
        status,
        priority,
        position,
        created_at,
        updated_at,
        start_date,
        time_expected,
        deadline,
        deadline_notified_at,
        completed_at,
        tags: Vec::new(),
    })
}

type RawNoteRow = (NoteId, String, Option<TaskId>, i64, i64);

fn row_to_raw_note(row: &rusqlite::Row) -> rusqlite::Result<RawNoteRow> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
}

fn note_from_parts(board_id: Option<BoardId>, raw: RawNoteRow) -> Note {
    let (id, body, promoted_task_id, created_at, updated_at) = raw;
    Note { id, board_id, body, promoted_task_id, created_at, updated_at }
}

const TASK_COLUMNS: &str =
    "id, title, body, status, priority, position, created_at, updated_at, start_date, deadline, completed_at, time_expected, deadline_notified_at";

/// Loads the tags for several tasks in one query (`task_id IN (...)`),
/// grouped by `task_id`. Called once per `list_tasks`/`get_task` call
/// regardless of how many tasks are involved -- never once per task.
fn tags_for_task_ids(conn: &Connection, task_ids: &[TaskId]) -> Result<HashMap<TaskId, Vec<Tag>>, StorageError> {
    let mut map: HashMap<TaskId, Vec<Tag>> = HashMap::new();
    if task_ids.is_empty() {
        return Ok(map);
    }
    let placeholders = vec!["?"; task_ids.len()].join(",");
    let sql = format!(
        "SELECT tt.task_id, t.id, t.name, t.color FROM task_tags tt
         JOIN tags t ON t.id = tt.tag_id
         WHERE tt.task_id IN ({placeholders}) ORDER BY t.name"
    );
    let mut stmt = conn.prepare(&sql)?;
    let params: Vec<&dyn rusqlite::ToSql> = task_ids.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
    let rows = stmt.query_map(params.as_slice(), |row| {
        let task_id: TaskId = row.get(0)?;
        let tag = Tag { id: row.get(1)?, name: row.get(2)?, color: row.get(3)? };
        Ok((task_id, tag))
    })?;
    for row in rows {
        let (task_id, tag) = row?;
        map.entry(task_id).or_default().push(tag);
    }
    Ok(map)
}

pub(crate) fn list_tasks(conn: &Connection, board_id: Option<BoardId>, status: Status) -> Result<Vec<Task>, StorageError> {
    let sql = match board_id {
        Some(_) => format!("SELECT {TASK_COLUMNS} FROM tasks WHERE board_id = ?1 AND status = ?2 ORDER BY position"),
        None => format!("SELECT {TASK_COLUMNS} FROM tasks WHERE status = ?1 ORDER BY position"),
    };
    let mut stmt = conn.prepare(&sql)?;
    let rows: Vec<RawTaskRow> = if let Some(bid) = board_id {
        stmt.query_map(rusqlite::params![bid, status.as_str()], row_to_raw_task)?.collect::<Result<_, _>>()?
    } else {
        stmt.query_map(rusqlite::params![status.as_str()], row_to_raw_task)?.collect::<Result<_, _>>()?
    };

    let task_ids: Vec<TaskId> = rows.iter().map(|r| r.0).collect();
    let mut tags_map = tags_for_task_ids(conn, &task_ids)?;

    rows.into_iter()
        .map(|raw| {
            let id = raw.0;
            let mut task = task_from_parts(board_id, raw)?;
            task.tags = tags_map.remove(&id).unwrap_or_default();
            Ok(task)
        })
        .collect()
}

pub(crate) fn get_task(conn: &Connection, board_id: Option<BoardId>, id: TaskId) -> Result<Task, StorageError> {
    let sql = match board_id {
        Some(_) => format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id = ?1 AND board_id = ?2"),
        None => format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id = ?1"),
    };
    let raw: Result<RawTaskRow, rusqlite::Error> = if let Some(bid) = board_id {
        conn.query_row(&sql, rusqlite::params![id, bid], row_to_raw_task)
    } else {
        conn.query_row(&sql, rusqlite::params![id], row_to_raw_task)
    };
    match raw {
        Ok(r) => {
            let mut task = task_from_parts(board_id, r)?;
            task.tags = tags_for_task_ids(conn, &[id])?.remove(&id).unwrap_or_default();
            Ok(task)
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => Err(StorageError::NotFound),
        Err(e) => Err(e.into()),
    }
}

pub(crate) fn create_task(conn: &Connection, board_id: Option<BoardId>, draft: NewTask) -> Result<TaskId, StorageError> {
    let now = chrono::Utc::now().timestamp();
    // A task created directly into Done (e.g. `a` while the Done column is
    // focused, or a note promoted straight there) has entered Done at
    // creation time, same as `move_task` stamps it on every other route.
    let completed_at: Option<i64> = if draft.status == Status::Done { Some(now) } else { None };
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
            "INSERT INTO tasks (board_id, title, body, status, priority, position, created_at, updated_at, start_date, time_expected, deadline, completed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                bid,
                draft.title,
                draft.body,
                draft.status.as_str(),
                draft.priority.as_str(),
                next_position,
                now,
                draft.start_date,
                draft.time_expected,
                draft.deadline,
                completed_at
            ],
        )?,
        None => conn.execute(
            "INSERT INTO tasks (title, body, status, priority, position, created_at, updated_at, start_date, time_expected, deadline, completed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                draft.title,
                draft.body,
                draft.status.as_str(),
                draft.priority.as_str(),
                next_position,
                now,
                draft.start_date,
                draft.time_expected,
                draft.deadline,
                completed_at
            ],
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
    if let Some(start_date) = patch.start_date {
        set_clauses.push("start_date = ?");
        values.push(Box::new(start_date));
    }
    if let Some(time_expected) = patch.time_expected {
        set_clauses.push("time_expected = ?");
        values.push(Box::new(time_expected));
    }
    if let Some(deadline) = patch.deadline {
        set_clauses.push("deadline = ?");
        values.push(Box::new(deadline));
        set_clauses.push("deadline_notified_at = NULL");
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

pub(crate) fn mark_deadline_notified(
    conn: &Connection,
    board_id: Option<BoardId>,
    id: TaskId,
) -> Result<(), StorageError> {
    let sql = match board_id {
        Some(_) => "UPDATE tasks SET deadline_notified_at = ?1 WHERE id = ?2 AND board_id = ?3",
        None => "UPDATE tasks SET deadline_notified_at = ?1 WHERE id = ?2",
    };
    let now = chrono::Utc::now().timestamp();
    let changed = match board_id {
        Some(bid) => conn.execute(sql, rusqlite::params![now, id, bid])?,
        None => conn.execute(sql, rusqlite::params![now, id])?,
    };
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
    // The single choke point every route that changes a task's status goes
    // through (H/L, the m/S dropdown, the edit form's status change, and
    // undo/redo of any of those, which all re-invoke this same function) --
    // so stamping `completed_at` here, and nowhere else, is what keeps it
    // correct everywhere at once.
    let completed_at: Option<i64> = if to == Status::Done { Some(now) } else { None };
    tx.execute(
        "UPDATE tasks SET status = ?1, position = ?2, updated_at = ?3, completed_at = ?4 WHERE id = ?5",
        rusqlite::params![to.as_str(), clamped_index, now, completed_at, id],
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
    // Same as `create_task`: a note promoted straight to Done has entered
    // Done at creation time.
    let completed_at: Option<i64> = if status == Status::Done { Some(now) } else { None };
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
            "INSERT INTO tasks (board_id, title, body, status, priority, position, created_at, updated_at, completed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8)",
            rusqlite::params![
                bid,
                title,
                rest,
                status.as_str(),
                Priority::default().as_str(),
                next_position,
                now,
                completed_at
            ],
        )?,
        None => tx.execute(
            "INSERT INTO tasks (title, body, status, priority, position, created_at, updated_at, completed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7)",
            rusqlite::params![title, rest, status.as_str(), Priority::default().as_str(), next_position, now, completed_at],
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

fn row_to_tag(row: &rusqlite::Row) -> rusqlite::Result<Tag> {
    Ok(Tag { id: row.get(0)?, name: row.get(1)?, color: row.get(2)? })
}

pub(crate) fn list_tags(conn: &Connection, board_id: Option<BoardId>) -> Result<Vec<Tag>, StorageError> {
    let sql = match board_id {
        Some(_) => "SELECT id, name, color FROM tags WHERE board_id = ?1 ORDER BY name",
        None => "SELECT id, name, color FROM tags ORDER BY name",
    };
    let mut stmt = conn.prepare(sql)?;
    let rows: Vec<Tag> = if let Some(bid) = board_id {
        stmt.query_map(rusqlite::params![bid], row_to_tag)?.collect::<Result<_, _>>()?
    } else {
        stmt.query_map([], row_to_tag)?.collect::<Result<_, _>>()?
    };
    Ok(rows)
}

/// Idempotent per board: a duplicate `name` returns the existing tag's id
/// (updating its color) rather than erroring or duplicating the row.
/// Rejects an empty or whitespace-only name.
pub(crate) fn upsert_tag(conn: &Connection, board_id: Option<BoardId>, name: &str, color: Option<&str>) -> Result<TagId, StorageError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(StorageError::EmptyTagName);
    }
    match board_id {
        Some(bid) => {
            conn.execute(
                "INSERT INTO tags (board_id, name, color) VALUES (?1, ?2, ?3)
                 ON CONFLICT(board_id, name) DO UPDATE SET color = excluded.color",
                rusqlite::params![bid, trimmed, color],
            )?;
            let id: TagId = conn.query_row(
                "SELECT id FROM tags WHERE board_id = ?1 AND name = ?2",
                rusqlite::params![bid, trimmed],
                |row| row.get(0),
            )?;
            Ok(id)
        }
        None => {
            conn.execute(
                "INSERT INTO tags (name, color) VALUES (?1, ?2)
                 ON CONFLICT(name) DO UPDATE SET color = excluded.color",
                rusqlite::params![trimmed, color],
            )?;
            let id: TagId =
                conn.query_row("SELECT id FROM tags WHERE name = ?1", rusqlite::params![trimmed], |row| row.get(0))?;
            Ok(id)
        }
    }
}

pub(crate) fn rename_tag(conn: &Connection, board_id: Option<BoardId>, id: TagId, new_name: &str) -> Result<(), StorageError> {
    let trimmed = new_name.trim();
    if trimmed.is_empty() {
        return Err(StorageError::EmptyTagName);
    }
    let sql = match board_id {
        Some(_) => "UPDATE tags SET name = ?1 WHERE id = ?2 AND board_id = ?3",
        None => "UPDATE tags SET name = ?1 WHERE id = ?2",
    };
    let changed = match board_id {
        Some(bid) => conn.execute(sql, rusqlite::params![trimmed, id, bid])?,
        None => conn.execute(sql, rusqlite::params![trimmed, id])?,
    };
    if changed == 0 {
        return Err(StorageError::NotFound);
    }
    Ok(())
}

pub(crate) fn delete_tag(conn: &Connection, board_id: Option<BoardId>, id: TagId) -> Result<(), StorageError> {
    let sql = match board_id {
        Some(_) => "DELETE FROM tags WHERE id = ?1 AND board_id = ?2",
        None => "DELETE FROM tags WHERE id = ?1",
    };
    let changed = match board_id {
        Some(bid) => conn.execute(sql, rusqlite::params![id, bid])?,
        None => conn.execute(sql, rusqlite::params![id])?,
    };
    if changed == 0 {
        return Err(StorageError::NotFound);
    }
    Ok(())
}

pub(crate) fn tags_for_task(conn: &Connection, task_id: TaskId) -> Result<Vec<Tag>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT t.id, t.name, t.color FROM tags t
         JOIN task_tags tt ON tt.tag_id = t.id
         WHERE tt.task_id = ?1 ORDER BY t.name",
    )?;
    let rows: Vec<Tag> = stmt.query_map(rusqlite::params![task_id], row_to_tag)?.collect::<Result<_, _>>()?;
    Ok(rows)
}

/// Replaces the whole tag set for `task_id` with exactly `tag_ids`.
pub(crate) fn set_task_tags(conn: &Connection, task_id: TaskId, tag_ids: &[TagId]) -> Result<(), StorageError> {
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM task_tags WHERE task_id = ?1", rusqlite::params![task_id])?;
    for tag_id in tag_ids {
        tx.execute(
            "INSERT OR IGNORE INTO task_tags (task_id, tag_id) VALUES (?1, ?2)",
            rusqlite::params![task_id, tag_id],
        )?;
    }
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod shared_tests {
    //! Assertions run against a `&dyn TaskStore` so `main_db` and
    //! `locked_db` can each set up their own store and share one test body,
    //! per the acceptance bar's "share the test bodies if practical".
    use crate::domain::task::{NewTask, Priority, Status, TaskPatch};
    use crate::storage::TaskStore;

    fn new_task(title: &str) -> NewTask {
        NewTask {
            title: title.into(),
            body: String::new(),
            status: Status::ToDo,
            priority: Priority::Normal,
            start_date: None,
            time_expected: None,
            deadline: None,
        }
    }

    pub(crate) fn create_read_task_with_and_without_dates(store: &dyn TaskStore) {
        let plain_id = store.create_task(new_task("no dates")).unwrap();
        let plain = store.get_task(plain_id).unwrap();
        assert_eq!(plain.start_date, None);
        assert_eq!(plain.deadline, None);

        let dated_id = store
            .create_task(NewTask {
                start_date: Some(1_000),
                time_expected: Some(600),
                deadline: Some(2_000),
                ..new_task("with dates")
            })
            .unwrap();
        let dated = store.get_task(dated_id).unwrap();
        assert_eq!(dated.start_date, Some(1_000));
        assert_eq!(dated.time_expected, Some(600));
        assert_eq!(dated.deadline, Some(2_000));
    }

    pub(crate) fn clear_deadline_via_patch_leaves_title_untouched(store: &dyn TaskStore) {
        let id = store
            .create_task(NewTask { deadline: Some(5_000), ..new_task("keep my title") })
            .unwrap();

        store
            .update_task(id, TaskPatch { deadline: Some(None), ..Default::default() })
            .unwrap();

        let after = store.get_task(id).unwrap();
        assert_eq!(after.deadline, None);
        assert_eq!(after.title, "keep my title");
    }

    pub(crate) fn tag_upsert_is_idempotent_and_rejects_empty_name(store: &dyn TaskStore) {
        let id1 = store.upsert_tag("urgent", Some("#ff0000")).unwrap();
        let id2 = store.upsert_tag("urgent", Some("#00ff00")).unwrap();
        assert_eq!(id1, id2, "upserting a duplicate name must return the same tag id");

        let tags = store.list_tags().unwrap();
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].color.as_deref(), Some("#00ff00"));

        assert!(matches!(store.upsert_tag("", None), Err(crate::storage::StorageError::EmptyTagName)));
        assert!(matches!(store.upsert_tag("   ", None), Err(crate::storage::StorageError::EmptyTagName)));
    }

    pub(crate) fn tag_rename_and_delete(store: &dyn TaskStore) {
        let id = store.upsert_tag("temp", None).unwrap();
        store.rename_tag(id, "renamed").unwrap();
        let tags = store.list_tags().unwrap();
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].name, "renamed");

        store.delete_tag(id).unwrap();
        assert!(store.list_tags().unwrap().is_empty());
    }

    pub(crate) fn set_task_tags_replaces_whole_set_and_reads_back(store: &dyn TaskStore) {
        let task_id = store.create_task(new_task("tagged task")).unwrap();
        let a = store.upsert_tag("a", None).unwrap();
        let b = store.upsert_tag("b", None).unwrap();
        let c = store.upsert_tag("c", None).unwrap();

        store.set_task_tags(task_id, &[a, b]).unwrap();
        let mut names: Vec<String> = store.tags_for_task(task_id).unwrap().into_iter().map(|t| t.name).collect();
        names.sort();
        assert_eq!(names, vec!["a".to_string(), "b".to_string()]);

        store.set_task_tags(task_id, &[c]).unwrap();
        let names: Vec<String> = store.tags_for_task(task_id).unwrap().into_iter().map(|t| t.name).collect();
        assert_eq!(names, vec!["c".to_string()]);

        store.set_task_tags(task_id, &[]).unwrap();
        assert!(store.tags_for_task(task_id).unwrap().is_empty());
    }

    pub(crate) fn deleting_task_cascades_its_tags(store: &dyn TaskStore) {
        let task_id = store.create_task(new_task("will be deleted")).unwrap();
        let tag_id = store.upsert_tag("keep-me", None).unwrap();
        store.set_task_tags(task_id, &[tag_id]).unwrap();
        assert_eq!(store.tags_for_task(task_id).unwrap().len(), 1);

        store.delete_task(task_id).unwrap();

        // The tag itself survives; only the junction row for this task is gone.
        assert_eq!(store.list_tags().unwrap().len(), 1);
        // The task is gone, so re-querying its tags must not resurrect it or panic.
        assert!(store.tags_for_task(task_id).unwrap().is_empty());
    }

    /// `move_task` is the single choke point every route that changes a
    /// task's status goes through, so pinning its `completed_at` behaviour
    /// here covers H/L, the m/S dropdown, the edit form's status change,
    /// and undo/redo of any of them at once.
    pub(crate) fn move_task_sets_and_clears_completed_at(store: &dyn TaskStore) {
        let id = store.create_task(new_task("t")).unwrap();
        assert_eq!(store.get_task(id).unwrap().completed_at, None);

        store.move_task(id, Status::Done, 0).unwrap();
        assert!(store.get_task(id).unwrap().completed_at.is_some());

        store.move_task(id, Status::Doing, 0).unwrap();
        assert_eq!(store.get_task(id).unwrap().completed_at, None);
    }

    pub(crate) fn create_task_directly_in_done_sets_completed_at(store: &dyn TaskStore) {
        let id = store.create_task(NewTask { status: Status::Done, ..new_task("born done") }).unwrap();
        assert!(store.get_task(id).unwrap().completed_at.is_some());

        let id2 = store.create_task(new_task("born todo")).unwrap();
        assert_eq!(store.get_task(id2).unwrap().completed_at, None);
    }

    pub(crate) fn tags_load_with_list_tasks_for_many_tasks(store: &dyn TaskStore) {
        let shared_tag = store.upsert_tag("shared", None).unwrap();
        let mut expected_own_tag_names = Vec::new();
        for i in 0..20 {
            let id = store.create_task(new_task(&format!("task {i}"))).unwrap();
            let own_tag_name = format!("own-{i}");
            let own_tag = store.upsert_tag(&own_tag_name, None).unwrap();
            store.set_task_tags(id, &[shared_tag, own_tag]).unwrap();
            expected_own_tag_names.push(own_tag_name);
        }

        let tasks = store.list_tasks(Status::ToDo).unwrap();
        assert_eq!(tasks.len(), 20);
        for task in &tasks {
            let mut names: Vec<&str> = task.tags.iter().map(|t| t.name.as_str()).collect();
            names.sort();
            assert_eq!(names.len(), 2);
            assert!(names.contains(&"shared"));
            let expected_own = format!("own-{}", task.title.trim_start_matches("task "));
            assert!(names.contains(&expected_own.as_str()), "task {} missing its own tag", task.title);
        }
    }
}
