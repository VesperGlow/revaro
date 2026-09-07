package database

import (
	"database/sql"
	"fmt"
	"path/filepath"
	"strings"
	"testing"
)

func TestWebMediaCopyMigration(t *testing.T) {
	for _, injectFailure := range []bool{false, true} {
		t.Run(fmt.Sprintf("migration_failure_%t", injectFailure), func(t *testing.T) {
			path := filepath.Join(t.TempDir(), "upgrade.db")
			db, err := sql.Open("sqlite", "file:"+path+"?_pragma=foreign_keys(1)")
			if err != nil {
				t.Fatal(err)
			}
			defer func() { db.Close() }()
			exec := func(query string, args ...any) {
				t.Helper()
				if _, err := db.Exec(query, args...); err != nil {
					t.Fatal(err)
				}
			}
			// Build the actual pre-fix schema, including migration history.
			entries, err := migrations.ReadDir("migrations")
			if err != nil {
				t.Fatal(err)
			}
			for _, entry := range entries {
				var version int
				if _, err := fmt.Sscanf(entry.Name(), "%d_", &version); err != nil {
					t.Fatal(err)
				}
				if version >= 19 {
					continue
				}
				body, err := migrations.ReadFile("migrations/" + entry.Name())
				if err != nil {
					t.Fatal(err)
				}
				exec(string(body))
				exec(`INSERT INTO schema_migrations VALUES(?,'2026-09-01')`, version)
			}
			for _, id := range []string{"source", "copy"} {
				exec(`INSERT INTO files(id,parent_id,name,kind,object_key,size,status,created_at,updated_at) VALUES(?,'00000000-0000-0000-0000-000000000000',?,'file','derived/media/video.mp4',100,'ready','created','updated')`, id, id+".mp4")
			}
			exec(`INSERT INTO web_media_playback VALUES('source','derived/media/video.mp4',100,'etag','video/mp4',12345,'hevc','aac','created')`)
			exec(`INSERT INTO web_media_subtitles VALUES('source',1000001,'derived/media/external.vtt',46,'subtitle-etag','zh-Hans','外挂字幕',1,1)`)
			if injectFailure {
				// Fail after playback's table rebuild, testing the migration runner's
				// rollback of both DDL and data, followed by a successful retry.
				exec(`CREATE TABLE web_media_subtitles_copy(blocker INTEGER)`)
				db.Close()
				failed, err := Open(path)
				if err == nil {
					failed.Close()
					t.Fatal("migration unexpectedly succeeded")
				}
				db, err = sql.Open("sqlite", "file:"+path+"?_pragma=foreign_keys(1)")
				if err != nil {
					t.Fatal(err)
				}
				var schema string
				if err := db.QueryRow(`SELECT sql FROM sqlite_master WHERE name='web_media_playback'`).Scan(&schema); err != nil || !strings.Contains(schema, "UNIQUE") {
					t.Fatalf("playback rebuild was not rolled back: %s %v", schema, err)
				}
				var count int
				if err := db.QueryRow(`SELECT COUNT(*) FROM schema_migrations WHERE version=19`).Scan(&count); err != nil || count != 0 {
					t.Fatalf("failed migration recorded: %d %v", count, err)
				}
				exec(`DROP TABLE web_media_subtitles_copy`)
			}
			db.Close()
			db, err = Open(path)
			if err != nil {
				t.Fatal(err)
			}
			// Every pre-existing field survives the upgrade; new associations can
			// share assets without weakening the per-file primary/foreign keys.
			var playback, subtitle string
			if err := db.QueryRow(`SELECT json_array(file_id,object_key,size,etag,mime_type,duration_ms,video_codec,audio_codec,created_at) FROM web_media_playback`).Scan(&playback); err != nil {
				t.Fatal(err)
			}
			if playback != `["source","derived/media/video.mp4",100,"etag","video/mp4",12345,"hevc","aac","created"]` {
				t.Fatalf("migrated playback: %s", playback)
			}
			if err := db.QueryRow(`SELECT json_array(file_id,track_index,object_key,size,etag,language,title,is_default,is_forced) FROM web_media_subtitles`).Scan(&subtitle); err != nil {
				t.Fatal(err)
			}
			if subtitle != `["source",1000001,"derived/media/external.vtt",46,"subtitle-etag","zh-Hans","外挂字幕",1,1]` {
				t.Fatalf("migrated subtitle: %s", subtitle)
			}
			exec(`INSERT INTO web_media_playback SELECT 'copy',object_key,size,etag,mime_type,duration_ms,video_codec,audio_codec,created_at FROM web_media_playback WHERE file_id='source'`)
			exec(`INSERT INTO web_media_subtitles SELECT 'copy',track_index,object_key,size,etag,language,title,is_default,is_forced FROM web_media_subtitles WHERE file_id='source'`)
			for _, statement := range []string{
				`INSERT INTO web_media_subtitles SELECT * FROM web_media_subtitles WHERE file_id='copy'`,
				`INSERT INTO web_media_playback SELECT * FROM web_media_playback WHERE file_id='copy'`,
				`UPDATE web_media_subtitles SET file_id='missing' WHERE file_id='copy'`,
				`UPDATE web_media_playback SET file_id='missing' WHERE file_id='copy'`,
				`UPDATE web_media_subtitles SET size=0 WHERE file_id='copy'`,
				`UPDATE web_media_playback SET size=0 WHERE file_id='copy'`,
			} {
				if _, err := db.Exec(statement); err == nil {
					t.Fatalf("constraint lost: %s", statement)
				}
			}
			exec(`DELETE FROM files WHERE id='source'`)
			for _, table := range []string{"web_media_playback", "web_media_subtitles"} {
				var id string
				if err := db.QueryRow(`SELECT file_id FROM ` + table).Scan(&id); err != nil || id != "copy" {
					t.Fatalf("cascade damaged surviving association: %s %v", id, err)
				}
			}
		})
	}
}
