//! Candidate rows shared by ordinary listings and series-group pagination.

use revaro_core::{ApiError, Timestamp, library::LibraryItem};
use rusqlite::{Connection, Row, params};

use super::{Options, db};
use crate::file_routes::{FILE_COLUMNS, scan_file};

pub(super) struct Candidate {
    pub item: LibraryItem,
    pub progress: Option<String>,
    pub bucket: String,
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
            series_files: Vec::new(),
        },
        progress: row.get("progress")?,
        bucket: row.get("bucket")?,
    })
}

pub(super) fn load_listing(
    connection: &Connection,
    options: Options,
    limit: i64,
    grouped: bool,
) -> Result<(i64, Vec<Candidate>), ApiError> {
    let (sort, aggregate, direction) = if options.series.is_some() {
        ("COALESCE(order_series_index,1e308)", "MIN", "ASC")
    } else if options.collection.is_some() {
        ("collection_position", "MIN", "ASC")
    } else if options.recent {
        ("last_opened", "MAX", "DESC")
    } else {
        ("created_at", "MAX", "DESC")
    };
    // The derived file projection fixes scan_file's column order before named library fields.
    let candidates = format!(
        "WITH classified AS (
            SELECT f.*,
                l.kind AS library_kind,
                COALESCE(s.favorite,0) AS favorite,
                NULLIF(MAX(
                    COALESCE(revaro_timestamp(s.last_opened),''),
                    COALESCE(revaro_timestamp(p.updated_at),''),
                    COALESCE(revaro_timestamp(mp.updated_at),'')
                ),'') AS last_opened,
                COALESCE(m.video_codec<>'',0) AS has_cover,
                m.duration_ms AS duration_ms,
                CASE WHEN l.kind='book' AND l.metadata_etag=COALESCE(f.etag,'')
                    THEN l.series END AS series,
                CASE WHEN l.metadata_etag=COALESCE(f.etag,'')
                    THEN l.series_index END AS series_index,
                l.series_index AS order_series_index,
                p.value AS progress,
                cm.position AS collection_position
            FROM (SELECT {FILE_COLUMNS} FROM files) f
            JOIN library_items l ON l.file_id=f.id
            LEFT JOIN library_state s ON s.file_id=f.id
            LEFT JOIN settings p ON p.key='book_progress/'||f.id
            LEFT JOIN media_progress mp ON mp.file_id=f.id
            LEFT JOIN media_metadata m ON m.file_id=f.id AND m.source_etag=f.etag
            LEFT JOIN library_collection_items cm
                ON cm.file_id=f.id AND cm.collection_id=?4
            WHERE f.status='ready' AND f.deleted_at IS NULL
                AND (?1 IS NULL OR l.kind=?1)
                AND (instr(lower(f.name),lower(?2))>0 OR (
                    l.metadata_etag=COALESCE(f.etag,'')
                    AND instr(lower(l.series),lower(?2))>0
                ))
                AND (?3=0 OR s.favorite=1)
                AND (?4 IS NULL OR cm.collection_id IS NOT NULL)
                AND (?5=0 OR s.last_opened IS NOT NULL
                    OR p.key IS NOT NULL OR mp.file_id IS NOT NULL)
                AND (?6 IS NULL OR l.series=?6)
        ), candidates AS (
            SELECT classified.*,
                CASE WHEN ?7 AND series IS NOT NULL
                    THEN 'series:'||series ELSE 'file:'||id END AS bucket,
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
        options.series,
        grouped
    ];
    let count = if grouped {
        "COUNT(DISTINCT bucket)"
    } else {
        "COUNT(*)"
    };
    let total = connection
        .query_row(
            &format!("{candidates} SELECT {count} FROM candidates"),
            filters,
            |row| row.get(0),
        )
        .map_err(db)?;
    let sql = if grouped {
        format!(
            "{candidates}, groups AS (
                SELECT bucket,{aggregate}(sort_key) AS sort_key
                FROM candidates GROUP BY bucket
                ORDER BY sort_key {direction},bucket LIMIT ?8 OFFSET ?9
            )
            SELECT candidates.* FROM candidates JOIN groups USING(bucket)
            ORDER BY groups.sort_key {direction},groups.bucket,
                candidates.series_index ASC NULLS LAST,candidates.name,candidates.id"
        )
    } else {
        format!(
            "{candidates} SELECT * FROM candidates
            ORDER BY sort_key {direction},CASE WHEN ?6 IS NOT NULL THEN name END,id
            LIMIT ?8 OFFSET ?9"
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
                options.series,
                grouped,
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
