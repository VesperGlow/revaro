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
    let mut q = c.prepare_cached(&format!("SELECT f.* FROM (SELECT {FILE_COLUMNS} FROM files) f JOIN book_stack_items si ON si.file_id=f.id CROSS JOIN library_items l ON l.file_id=f.id WHERE si.stack_id=?1 AND f.status='ready' AND f.deleted_at IS NULL AND l.kind='book' ORDER BY si.position,f.id")).map_err(db)?;
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
    let selected = super::batch::ids(file_ids, true)?;
    let valid: i64 = tx.query_row("SELECT COUNT(*) FROM json_each(?1) selected CROSS JOIN files f ON f.id=selected.value JOIN library_items l ON l.file_id=f.id WHERE f.status='ready' AND f.deleted_at IS NULL AND l.kind='book'", [&selected], |r| r.get(0)).map_err(db)?;
    if valid != file_ids.len() as i64 {
        return Err(ApiError::bad_request("堆叠只能包含可用的书籍"));
    }
    let mut query = tx.prepare_cached("SELECT DISTINCT stack_id FROM book_stack_items WHERE file_id IN (SELECT value FROM json_each(?1)) AND stack_id<>?2").map_err(db)?;
    let previous = query
        .query_map(params![selected, id], |r| r.get::<_, String>(0))
        .map_err(db)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db)?;
    drop(query);
    // Keep destination slots, move other memberships in one statement, and
    // calculate MAX only once rather than for every inserted book.
    tx.execute("DELETE FROM book_stack_items WHERE stack_id<>?1 AND file_id IN (SELECT value FROM json_each(?2))",params![id,selected]).map_err(db)?;
    tx.execute("WITH start AS MATERIALIZED (SELECT COALESCE(MAX(position)+1,0) AS position FROM book_stack_items WHERE stack_id=?1), added AS MATERIALIZED (SELECT selected.value AS file_id,ROW_NUMBER() OVER (ORDER BY CAST(selected.key AS INTEGER))-1 AS offset FROM json_each(?2) selected WHERE NOT EXISTS(SELECT 1 FROM book_stack_items i WHERE i.file_id=selected.value)) INSERT INTO book_stack_items(stack_id,file_id,position) SELECT ?1,added.file_id,start.position+added.offset FROM added CROSS JOIN start",params![id,selected]).map_err(db)?;
    let previous = serde_json::to_string(&previous)
        .map_err(|_| ApiError::internal("could not encode stacks"))?;
    tx.execute("DELETE FROM book_stacks WHERE id IN (SELECT value FROM json_each(?1)) AND NOT EXISTS(SELECT 1 FROM book_stack_items WHERE stack_id=book_stacks.id)",[previous]).map_err(db)?;
    Ok(())
}

async fn list(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
) -> Result<Json<Vec<Stack>>, ApiError> {
    state
        .db
        .call_api(|c| {
            let tx=c.transaction().map_err(db)?;
            let mut stacks = tx.prepare_cached("SELECT id,name FROM book_stacks ORDER BY created_at,id").map_err(db)?
                .query_map([], |r| Ok(Stack{id:r.get(0)?,name:r.get(1)?,files:Vec::new()})).map_err(db)?
                .collect::<Result<Vec<_>,_>>().map_err(db)?;
            let positions = stacks.iter().enumerate().map(|(i,s)|(s.id.clone(),i)).collect::<std::collections::HashMap<_,_>>();
            let mut query = tx.prepare_cached(&format!("SELECT f.*,si.stack_id FROM (SELECT {FILE_COLUMNS} FROM files) f JOIN book_stack_items si ON si.file_id=f.id CROSS JOIN library_items l ON l.file_id=f.id WHERE f.status='ready' AND f.deleted_at IS NULL AND l.kind='book' ORDER BY si.stack_id,si.position,f.id")).map_err(db)?;
            let rows=query.query_map([],|r|Ok((r.get::<_,String>(15)?,scan_file(r)?))).map_err(db)?;
            for row in rows { let (id,file)=row.map_err(db)?; if let Some(&index)=positions.get(&id) { stacks[index].files.push(file); } }
            drop(query);
            tx.commit().map_err(db)?;
            Ok(stacks)
        }).await.map(Json)
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
        let selected = super::batch::ids(&o.file_ids,false)?;
        tx.execute("DELETE FROM book_stack_items WHERE stack_id=?1 AND file_id IN (SELECT value FROM json_each(?2))", params![id,selected]).map_err(db)?;
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
            let selected = super::batch::ids(&o.file_ids,true)?;
            tx.execute("WITH slots AS MATERIALIZED (SELECT position,ROW_NUMBER() OVER (ORDER BY position,file_id)-1 AS ordinal FROM book_stack_items WHERE stack_id=?1 AND file_id IN (SELECT value FROM json_each(?2))), ordering AS MATERIALIZED (SELECT selected.value AS file_id,slots.position FROM json_each(?2) selected JOIN slots ON slots.ordinal=CAST(selected.key AS INTEGER)) UPDATE book_stack_items SET position=(SELECT position FROM ordering WHERE ordering.file_id=book_stack_items.file_id) WHERE stack_id=?1 AND file_id IN (SELECT file_id FROM ordering)",params![id,selected]).map_err(db)?;
            tx.commit().map_err(db)?;
            Ok(http::StatusCode::NO_CONTENT)
        })
        .await
}

async fn suggestions(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
) -> Result<axum::response::Response, ApiError> {
    use axum::response::IntoResponse;
    state.maintenance.wake("book-series");
    state.db.call_api(|c| {
        let now = revaro_core::Timestamp::now().to_string();
        let pending: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM library_items l JOIN files f ON f.id=l.file_id WHERE l.kind='book' AND f.status='ready' AND f.deleted_at IS NULL AND (l.metadata_etag IS NULL OR l.metadata_etag<>COALESCE(f.etag,'')) AND NOT EXISTS(SELECT 1 FROM book_metadata_retries r WHERE r.file_id=f.id AND r.source_etag=COALESCE(f.etag,'') AND r.retry_at>?1))", [now], |row|row.get(0)).map_err(db)?;
        let mut q = c.prepare_cached("SELECT l.series,f.id FROM library_items l JOIN files f ON f.id=l.file_id WHERE l.kind='book' AND l.metadata_etag=COALESCE(f.etag,'') AND l.series IS NOT NULL AND f.status='ready' AND f.deleted_at IS NULL AND NOT EXISTS(SELECT 1 FROM book_stack_items WHERE file_id=f.id) ORDER BY l.series,l.series_index ASC NULLS LAST,f.name,f.id").map_err(db)?;
        let rows = q.query_map([], |r| Ok((r.get::<_, String>(0)?,r.get::<_, String>(1)?))).map_err(db)?;
        let mut suggestions = Vec::<StackSuggestion>::new();
        for row in rows {
            let (name, id) = row.map_err(db)?;
            if let Some(last) = suggestions.last_mut().filter(|s| s.name==name) { last.file_ids.push(id); }
            else { suggestions.push(StackSuggestion { name, file_ids: vec![id] }); }
        }
        suggestions.retain(|s| s.file_ids.len()>=2 && s.file_ids.len()<=5000);
        Ok((pending,suggestions))
    }).await.map(|(pending,suggestions)| (
        [("x-revaro-index-pending",if pending {"1"} else {"0"})],
        Json(suggestions),
    ).into_response())
}
