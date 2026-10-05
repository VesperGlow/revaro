//! Transactional operations that only change manual book grouping.
use std::collections::HashSet;

use revaro_core::stacks::{CreateStack, RenameStack, Stack, StackMembers, StackSuggestion};
use rusqlite::{Connection, Transaction, params};

use super::*;

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/library/stacks", get(list).post(create))
        .route(
            "/library/stacks/{id}",
            axum::routing::patch(rename).delete(dissolve),
        )
        .route(
            "/library/stacks/{id}/items",
            axum::routing::post(add).delete(remove),
        )
        .route("/library/stacks/{id}/order", axum::routing::put(reorder))
        .route("/library/stack-suggestions", get(suggestions))
}

fn name(value: String) -> Result<String, ApiError> {
    let value = value.trim().to_owned();
    if value.is_empty() || value.chars().count() > 80 {
        return Err(ApiError::bad_request("堆叠名称应为 1–80 个字符"));
    }
    Ok(value)
}

fn members(ids: &[String], minimum: usize) -> Result<(), ApiError> {
    if ids.len() < minimum
        || ids.len() > 5000
        || ids.iter().any(|id| id.is_empty() || id.len() > 128)
        || ids.iter().collect::<HashSet<_>>().len() != ids.len()
    {
        return Err(ApiError::bad_request("请选择不重复的书籍，最多 5000 本"));
    }
    Ok(())
}

fn require_stack(c: &Connection, id: &str) -> Result<(), ApiError> {
    if !c
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM book_stacks WHERE id=?1)",
            [id],
            |r| r.get::<_, bool>(0),
        )
        .map_err(db)?
    {
        return Err(ApiError::not_found("堆叠不存在"));
    }
    Ok(())
}

fn load_stack(c: &Connection, id: &str) -> Result<Stack, ApiError> {
    let name = c
        .query_row("SELECT name FROM book_stacks WHERE id=?1", [id], |r| {
            r.get(0)
        })
        .optional()
        .map_err(db)?
        .ok_or_else(|| ApiError::not_found("堆叠不存在"))?;
    let mut q = c.prepare(&format!("SELECT {FILE_COLUMNS} FROM files WHERE status='ready' AND deleted_at IS NULL AND id IN (SELECT si.file_id FROM book_stack_items si JOIN library_items l ON l.file_id=si.file_id WHERE si.stack_id=?1 AND l.kind='book') ORDER BY (SELECT position FROM book_stack_items WHERE file_id=files.id),id")).map_err(db)?;
    let files = q
        .query_map([id], scan_file)
        .map_err(db)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db)?;
    Ok(Stack {
        id: id.to_owned(),
        name,
        files,
    })
}

fn add_books(tx: &Transaction<'_>, id: &str, file_ids: &[String]) -> Result<(), ApiError> {
    // Validate the entire request before moving any membership. A failed batch rolls back.
    for file_id in file_ids {
        let valid = tx.query_row("SELECT EXISTS(SELECT 1 FROM files f JOIN library_items l ON l.file_id=f.id WHERE f.id=?1 AND f.status='ready' AND f.deleted_at IS NULL AND l.kind='book')", [file_id], |r| r.get::<_, bool>(0)).map_err(db)?;
        if !valid {
            return Err(ApiError::bad_request("堆叠只能包含可用的书籍"));
        }
    }
    for file_id in file_ids {
        let previous: Option<String> = tx
            .query_row(
                "SELECT stack_id FROM book_stack_items WHERE file_id=?1",
                [file_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(db)?;
        if previous.as_deref() == Some(id) {
            continue;
        }
        tx.execute("DELETE FROM book_stack_items WHERE file_id=?1", [file_id])
            .map_err(db)?;
        tx.execute("INSERT INTO book_stack_items(stack_id,file_id,position) VALUES(?1,?2,COALESCE((SELECT MAX(position)+1 FROM book_stack_items WHERE stack_id=?1),0))", params![id,file_id]).map_err(db)?;
        if let Some(previous) = previous {
            tx.execute("DELETE FROM book_stacks WHERE id=?1 AND NOT EXISTS(SELECT 1 FROM book_stack_items WHERE stack_id=?1)", [previous]).map_err(db)?;
        }
    }
    Ok(())
}

async fn list(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
) -> Result<Json<Vec<Stack>>, ApiError> {
    state
        .db
        .call_api(|c| {
            let ids = c
                .prepare("SELECT id FROM book_stacks ORDER BY created_at,id")
                .map_err(db)?
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(db)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(db)?;
            ids.iter().map(|id| load_stack(c, id)).collect()
        })
        .await
        .map(Json)
}

async fn create(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    JsonBody(o): JsonBody<CreateStack>,
) -> Result<Json<Stack>, ApiError> {
    let name = name(o.name)?;
    members(&o.file_ids, 2)?;
    state
        .db
        .call_api(move |c| {
            let tx = c
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(db)?;
            let id = crate::ids::new_id();
            tx.execute(
                "INSERT INTO book_stacks(id,name,created_at) VALUES(?1,?2,?3)",
                params![id, name, Timestamp::now().to_rfc3339()],
            )
            .map_err(db)?;
            add_books(&tx, &id, &o.file_ids)?;
            let stack = load_stack(&tx, &id)?;
            tx.commit().map_err(db)?;
            Ok(stack)
        })
        .await
        .map(Json)
}

async fn rename(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    Path(id): Path<String>,
    JsonBody(o): JsonBody<RenameStack>,
) -> Result<http::StatusCode, ApiError> {
    let name = name(o.name)?;
    state
        .db
        .call_api(move |c| {
            require_stack(c, &id)?;
            c.execute(
                "UPDATE book_stacks SET name=?2 WHERE id=?1",
                params![id, name],
            )
            .map_err(db)?;
            Ok(http::StatusCode::NO_CONTENT)
        })
        .await
}

async fn dissolve(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    Path(id): Path<String>,
) -> Result<http::StatusCode, ApiError> {
    state
        .db
        .call_api(move |c| {
            require_stack(c, &id)?;
            c.execute("DELETE FROM book_stacks WHERE id=?1", [id])
                .map_err(db)?;
            Ok(http::StatusCode::NO_CONTENT)
        })
        .await
}

async fn add(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    Path(id): Path<String>,
    JsonBody(o): JsonBody<StackMembers>,
) -> Result<http::StatusCode, ApiError> {
    members(&o.file_ids, 1)?;
    state
        .db
        .call_api(move |c| {
            let tx = c
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(db)?;
            require_stack(&tx, &id)?;
            add_books(&tx, &id, &o.file_ids)?;
            tx.commit().map_err(db)?;
            Ok(http::StatusCode::NO_CONTENT)
        })
        .await
}

async fn remove(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    Path(id): Path<String>,
    JsonBody(o): JsonBody<StackMembers>,
) -> Result<http::StatusCode, ApiError> {
    members(&o.file_ids, 1)?;
    state.db.call_api(move |c| {
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(db)?;
        require_stack(&tx, &id)?;
        for file_id in o.file_ids {
            tx.execute("DELETE FROM book_stack_items WHERE stack_id=?1 AND file_id=?2", params![id,file_id]).map_err(db)?;
        }
        tx.execute("DELETE FROM book_stacks WHERE id=?1 AND NOT EXISTS(SELECT 1 FROM book_stack_items WHERE stack_id=?1)", [id]).map_err(db)?;
        tx.commit().map_err(db)?;
        Ok(http::StatusCode::NO_CONTENT)
    }).await
}

async fn reorder(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    Path(id): Path<String>,
    JsonBody(o): JsonBody<StackMembers>,
) -> Result<http::StatusCode, ApiError> {
    members(&o.file_ids, 0)?;
    state
        .db
        .call_api(move |c| {
            let tx = c
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(db)?;
            let stack = load_stack(&tx, &id)?;
            let active = stack.files.iter().map(|f| &f.id).collect::<HashSet<_>>();
            if active != o.file_ids.iter().collect::<HashSet<_>>() {
                return Err(ApiError::bad_request("书籍已变化，请刷新后重新排序"));
            }
            // Reuse active members' slots, leaving temporarily unavailable books in place.
            let positions = stack
                .files
                .iter()
                .map(|file| {
                    tx.query_row(
                        "SELECT position FROM book_stack_items WHERE stack_id=?1 AND file_id=?2",
                        params![id, file.id],
                        |row| row.get::<_, i64>(0),
                    )
                })
                .collect::<Result<Vec<_>, _>>()
                .map_err(db)?;
            for (position, file_id) in positions.iter().zip(&o.file_ids) {
                tx.execute(
                    "UPDATE book_stack_items SET position=?3 WHERE stack_id=?1 AND file_id=?2",
                    params![id, file_id, position],
                )
                .map_err(db)?;
            }
            tx.commit().map_err(db)?;
            Ok(http::StatusCode::NO_CONTENT)
        })
        .await
}

async fn suggestions(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
) -> Result<Json<Vec<StackSuggestion>>, ApiError> {
    index_book_series(&state).await?;
    state.db.call_api(|c| {
        let mut q = c.prepare("SELECT l.series,f.id FROM library_items l JOIN files f ON f.id=l.file_id WHERE l.kind='book' AND l.metadata_etag=COALESCE(f.etag,'') AND l.series IS NOT NULL AND f.status='ready' AND f.deleted_at IS NULL AND NOT EXISTS(SELECT 1 FROM book_stack_items WHERE file_id=f.id) ORDER BY l.series,l.series_index ASC NULLS LAST,f.name,f.id").map_err(db)?;
        let rows = q.query_map([], |r| Ok((r.get::<_, String>(0)?,r.get::<_, String>(1)?))).map_err(db)?;
        let mut suggestions = Vec::<StackSuggestion>::new();
        for row in rows {
            let (name, id) = row.map_err(db)?;
            if let Some(last) = suggestions.last_mut().filter(|s| s.name==name) { last.file_ids.push(id); }
            else { suggestions.push(StackSuggestion { name, file_ids: vec![id] }); }
        }
        suggestions.retain(|s| s.file_ids.len()>=2 && s.file_ids.len()<=5000);
        Ok(suggestions)
    }).await.map(Json)
}
