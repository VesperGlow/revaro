//! Candidate rows shared by ordinary listings and manual-stack pagination.

use revaro_core::{ApiError, Timestamp, library::LibraryItem};
use rusqlite::{Connection, Row, params};

use super::{Options, db};
use crate::file_routes::{FILE_COLUMNS, scan_file};

pub(super) struct Candidate {
    pub item: LibraryItem,
    pub progress: Option<String>,
    pub bucket: String,
    pub stack: Option<(String, String)>,
}

fn scan_candidate(row: &Row<'_>) -> rusqlite::Result<Candidate> {
    let mut file = scan_file(row)?;
    file.has_cover = row.get("has_cover")?;
    Ok(Candidate {
        item: LibraryItem {
            file,
            duration_ms: row.get("duration_ms")?,
            kind: row.get("library_kind")?,
            favorite: row.get("favorite")?,
            last_opened: row
                .get::<_, Option<String>>("last_opened")?
                .and_then(|s| Timestamp::parse(&s).ok()),
            reading_progress: None,
            series: row.get("series")?,
            series_index: row.get("series_index")?,
            stack: None,
        },
        progress: row.get("progress")?,
        bucket: row.get("bucket")?,
        stack: row
            .get::<_, Option<String>>("stack_id")?
            .map(|id| row.get("stack_name").map(|name| (id, name)))
            .transpose()?,
    })
}

pub(super) fn load_listing(
    connection: &Connection,
    options: Options,
    limit: i64,
    grouped: bool,
) -> Result<(i64, Vec<Candidate>), ApiError> {
    let (sort, aggregate, direction) = if options.stack.is_some() {
        ("stack_position", "MIN", "ASC")
    } else if options.collection.is_some() {
        ("collection_position", "MIN", "ASC")
    } else if options.recent || options.opened_only {
        ("last_opened", "MAX", "DESC")
    } else {
        ("created_at", "MAX", "DESC")
    };
    // Select common library/home pages before enriching them. Joining progress,
    // covers and stacks for every file made a first page cost the entire library.
    let fast_page = !grouped
        && options.stack.is_none()
        && options.collection.is_none()
        && options.q.is_empty()
        && !options.favorite
        && (!options.recent || options.opened_only);
    let kind_filter = if options.kind.is_some() {
        "l.kind=?1"
    } else {
        "?1 IS NULL"
    };
    let simple_tables = if options.opened_only {
        "library_state opened INDEXED BY library_opened CROSS JOIN files f ON f.id=opened.file_id CROSS JOIN library_items l ON l.file_id=f.id"
    } else {
        "files f INDEXED BY files_content_order CROSS JOIN library_items l ON l.file_id=f.id"
    };
    let opened_filter = if options.opened_only {
        "AND opened.last_opened IS NOT NULL"
    } else {
        ""
    };
    let page_order = if options.opened_only {
        "revaro_timestamp(opened.last_opened) DESC,f.id"
    } else {
        "f.created_at DESC,f.id"
    };
    let simple_source = format!(
        "FROM {simple_tables} WHERE f.kind='file' AND f.status='ready' AND f.deleted_at IS NULL AND {kind_filter} {opened_filter}"
    );
    let page_prefix = if fast_page {
        format!(
            "page_ids AS (SELECT f.id {simple_source} ORDER BY {page_order} LIMIT ?9 OFFSET ?10),"
        )
    } else {
        String::new()
    };
    let page_join = if fast_page {
        "JOIN page_ids ON page_ids.id=f.id"
    } else {
        ""
    };
    // The derived file projection fixes scan_file's column order before named library fields.
    let candidates = format!(
        "WITH {page_prefix} classified AS (
            SELECT f.*,
                l.kind AS library_kind,
                COALESCE(s.favorite,0) AS favorite,
                CASE WHEN ?8 THEN revaro_timestamp(s.last_opened) ELSE NULLIF(MAX(
                    COALESCE(revaro_timestamp(s.last_opened),''),
                    COALESCE(revaro_timestamp(p.updated_at),''),
                    COALESCE(revaro_timestamp(mp.updated_at),'')
                ),'') END AS last_opened,
                COALESCE(m.video_codec<>'',0) AS has_cover,
                m.duration_ms AS duration_ms,
                CASE WHEN l.kind='book' AND l.metadata_etag=COALESCE(f.etag,'')
                    THEN l.series END AS series,
                CASE WHEN l.metadata_etag=COALESCE(f.etag,'')
                    THEN l.series_index END AS series_index,
                bs.id AS stack_id,
                bs.name AS stack_name,
                si.position AS stack_position,
                p.value AS progress,
                cm.position AS collection_position
            FROM (SELECT {FILE_COLUMNS} FROM files) f
            {page_join}
            JOIN library_items l ON l.file_id=f.id
            LEFT JOIN library_state s ON s.file_id=f.id
            LEFT JOIN settings p ON p.key='book_progress/'||f.id
            LEFT JOIN media_progress mp ON mp.file_id=f.id
            LEFT JOIN media_metadata m ON m.file_id=f.id AND m.source_etag=f.etag
            LEFT JOIN library_collection_items cm
                ON cm.file_id=f.id AND cm.collection_id=?4
            LEFT JOIN book_stack_items si ON si.file_id=f.id AND l.kind='book'
            LEFT JOIN book_stacks bs ON bs.id=si.stack_id
            WHERE f.kind='file' AND f.status='ready' AND f.deleted_at IS NULL
                AND (?1 IS NULL OR l.kind=?1)
                AND (instr(lower(f.name),lower(?2))>0 OR instr(lower(bs.name),lower(?2))>0)
                AND (?3=0 OR s.favorite=1)
                AND (?4 IS NULL OR cm.collection_id IS NOT NULL)
                AND (?5=0 OR s.last_opened IS NOT NULL
                    OR p.key IS NOT NULL OR mp.file_id IS NOT NULL)
                AND (?6 IS NULL OR bs.id=?6)
                AND (?8=0 OR s.last_opened IS NOT NULL)
        ), candidates AS (
            SELECT classified.*,
                CASE WHEN ?7 AND stack_id IS NOT NULL
                    THEN 'stack:'||stack_id ELSE 'file:'||id END AS bucket,
                {sort} AS sort_key
            FROM classified
        )"
    );
    let filters = params![
        options.kind,
        options.q,
        options.favorite,
        options.collection,
        options.recent,
        options.stack,
        grouped,
        options.opened_only
    ];
    let count = if grouped {
        "COUNT(DISTINCT bucket)"
    } else {
        "COUNT(*)"
    };
    let total = if fast_page {
        connection
            .query_row(
                &format!("SELECT COUNT(*) {simple_source}"),
                params![options.kind],
                |row| row.get(0),
            )
            .map_err(db)?
    } else {
        connection
            .query_row(
                &format!("{candidates} SELECT {count} FROM candidates"),
                filters,
                |row| row.get(0),
            )
            .map_err(db)?
    };
    let sql = if grouped {
        format!(
            "{candidates}, groups AS (
                SELECT bucket,{aggregate}(sort_key) AS sort_key
                FROM candidates GROUP BY bucket
                ORDER BY sort_key {direction},bucket LIMIT ?9 OFFSET ?10
            )
            SELECT candidates.* FROM candidates JOIN groups USING(bucket)
            ORDER BY groups.sort_key {direction},groups.bucket,
                candidates.stack_position ASC NULLS LAST,candidates.name,candidates.id"
        )
    } else {
        let final_offset = if fast_page { "0" } else { "?10" };
        format!(
            "{candidates} SELECT * FROM candidates
            ORDER BY sort_key {direction},CASE WHEN ?6 IS NOT NULL THEN name END,id
            LIMIT ?9 OFFSET {final_offset}"
        )
    };
    let mut query = connection.prepare(&sql).map_err(db)?;
    let rows = query
        .query_map(
            params![
                options.kind,
                options.q,
                options.favorite,
                options.collection,
                options.recent,
                options.stack,
                grouped,
                options.opened_only,
                limit,
                options.offset
            ],
            scan_candidate,
        )
        .map_err(db)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db)?;
    Ok((total, rows))
}
