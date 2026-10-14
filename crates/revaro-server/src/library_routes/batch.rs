//! Set-based library mutations: one request and one durable transaction per batch.
use super::*;
use revaro_core::library::{CollectionMembers, FavoriteBatch};
use rusqlite::{Connection, params};
use std::collections::HashSet;

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/library/collections/{id}/items",
            axum::routing::put(add).delete(remove),
        )
        .route("/library/favorites", axum::routing::patch(favorites))
}

pub(super) fn ids(ids: &[String], empty: bool) -> Result<String, ApiError> {
    if (!empty && ids.is_empty())
        || ids.len() > 5000
        || ids.iter().any(|id| id.is_empty() || id.len() > 128)
        || ids.iter().collect::<HashSet<_>>().len() != ids.len()
    {
        return Err(ApiError::bad_request("请选择不重复的文件，最多 5000 项"));
    }
    serde_json::to_string(ids).map_err(|_| ApiError::bad_request("invalid selection"))
}

pub(super) fn add_members(c: &Connection, id: &str, file_ids: &[String]) -> Result<i64, ApiError> {
    let selected = ids(file_ids, true)?;
    let valid: i64 = c.query_row("SELECT COUNT(*) FROM json_each(?2) selected CROSS JOIN files f ON f.id=selected.value JOIN library_items l ON l.file_id=f.id CROSS JOIN library_collections c ON c.id=?1 AND c.kind=l.kind WHERE f.status='ready' AND f.deleted_at IS NULL",params![id,selected],|r|r.get(0)).map_err(db)?;
    if valid != file_ids.len() as i64 {
        return Err(ApiError::bad_request("文件与集合类型不匹配，或文件已删除"));
    }
    // Materialize the starting position once. Existing members retain their slots;
    // new members follow request order even when some ids already belong here.
    c.execute("WITH start AS MATERIALIZED (SELECT COALESCE(MAX(position)+1,0) AS position FROM library_collection_items WHERE collection_id=?1), added AS MATERIALIZED (SELECT selected.value AS file_id,ROW_NUMBER() OVER (ORDER BY CAST(selected.key AS INTEGER))-1 AS offset FROM json_each(?2) selected WHERE NOT EXISTS(SELECT 1 FROM library_collection_items i WHERE i.collection_id=?1 AND i.file_id=selected.value)) INSERT INTO library_collection_items(collection_id,file_id,position) SELECT ?1,added.file_id,start.position+added.offset FROM added CROSS JOIN start",params![id,selected]).map_err(db)?;
    c.query_row(
        "SELECT COUNT(*) FROM library_collection_items WHERE collection_id=?1",
        [id],
        |r| r.get(0),
    )
    .map_err(db)
}

async fn add(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    Path(id): Path<String>,
    JsonBody(o): JsonBody<CollectionMembers>,
) -> Result<http::StatusCode, ApiError> {
    ids(&o.file_ids, false)?;
    state
        .db
        .call_api(move |c| {
            let tx = c
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(db)?;
            require_collection(&tx, &id)?;
            add_members(&tx, &id, &o.file_ids)?;
            tx.commit().map_err(db)?;
            Ok(http::StatusCode::NO_CONTENT)
        })
        .await
}
fn require_collection(c: &Connection, id: &str) -> Result<(), ApiError> {
    if !c
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM library_collections WHERE id=?1)",
            [id],
            |r| r.get::<_, bool>(0),
        )
        .map_err(db)?
    {
        return Err(ApiError::not_found("collection not found"));
    }
    Ok(())
}
async fn remove(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    Path(id): Path<String>,
    JsonBody(o): JsonBody<CollectionMembers>,
) -> Result<http::StatusCode, ApiError> {
    let selected = ids(&o.file_ids, false)?;
    state.db.call_api(move |c| {
        let tx=c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(db)?;
        require_collection(&tx,&id)?;
        tx.execute("DELETE FROM library_collection_items WHERE collection_id=?1 AND file_id IN (SELECT value FROM json_each(?2))",params![id,selected]).map_err(db)?;
        tx.commit().map_err(db)?;
        Ok(http::StatusCode::NO_CONTENT)
    }).await
}
async fn favorites(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    JsonBody(o): JsonBody<FavoriteBatch>,
) -> Result<http::StatusCode, ApiError> {
    let selected = ids(&o.file_ids, false)?;
    state.db.call_api(move |c| {
        let tx=c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(db)?;
        let valid:i64=tx.query_row("SELECT COUNT(*) FROM json_each(?1) selected CROSS JOIN files f ON f.id=selected.value WHERE f.kind='file' AND f.deleted_at IS NULL",[&selected],|r|r.get(0)).map_err(db)?;
        if valid!=o.file_ids.len() as i64 { return Err(ApiError::bad_request("文件已删除，请刷新后重试")); }
        tx.execute("INSERT INTO library_state(file_id,favorite) SELECT value,?2 FROM json_each(?1) WHERE 1 ON CONFLICT(file_id) DO UPDATE SET favorite=excluded.favorite",params![selected,o.favorite]).map_err(db)?;
        tx.commit().map_err(db)?;
        Ok(http::StatusCode::NO_CONTENT)
    }).await
}
