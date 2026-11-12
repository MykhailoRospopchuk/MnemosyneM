use std::time::SystemTime;

use rusqlite::{Connection, params};

use crate::{db::embedding_to_blob, engine::ClipEngine};

pub fn run(dp_path: &str, engine: &mut ClipEngine) -> Result<(), String> {
    let mut conn = Connection::open(dp_path).map_err(|e| e.to_string())?;

    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS _migrations (
            name TEXT NOT NULL PRIMARY KEY,
            executed_at BIGINT NOT NULL
        );
        ",
    )
    .map_err(|e| e.to_string())?;

    recalculate_embeddings(&mut conn, engine)?;
    add_hash_column_to_memes(&mut conn)?;
    add_media_type_column(&mut conn)?;

    Ok(())
}

fn recalculate_embeddings(conn: &mut Connection, engine: &mut ClipEngine) -> Result<(), String> {
    let migratin_name = "1793997256_recalculate_embeddings".to_string();
    let force_recalculation = std::env::var("RECALCULATE_EMBEDDINGS").is_ok();

    let already_applied: bool = conn
        .query_row(
            "SELECT 1 FROM _migrations WHERE name = ?",
            params![migratin_name],
            |_| Ok(true),
        )
        .unwrap_or(false);

    if already_applied && !force_recalculation {
        return Ok(());
    }

    println!(
        "[MIGRATION] {} - Recalculating embeddings for memes from description_uk & description_en...",
        migratin_name
    );

    let mut stmt = conn
        .prepare("SELECT id, ocr_text, description_uk, description_en FROM memes")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            let id: u64 = row.get(0)?;
            let ocr_text: String = row.get(1)?;
            let desc_uk: String = row.get(2)?;
            let desc_en: String = row.get(3)?;
            Ok((id, ocr_text, desc_uk, desc_en))
        })
        .map_err(|e| e.to_string())?;
    let memes_to_update: Vec<(u64, String, String, String)> = rows.filter_map(|r| r.ok()).collect();
    drop(stmt);
    let total = memes_to_update.len();

    println!("[MIGRATION] Found {} memes to recalculate.", total);

    let tx = conn.transaction().map_err(|e| e.to_string())?;
    {
        let mut update_stmt = tx
            .prepare("UPDATE memes SET embedding_uk = ?, embedding_en = ? WHERE id = ?")
            .map_err(|e| e.to_string())?;
        for (idx, (id, ocr_text, desc_uk, desc_en)) in memes_to_update.into_iter().enumerate() {
            let passage_uk = if ocr_text.is_empty() {
                desc_uk.clone()
            } else {
                format!("{} | {}", ocr_text, desc_uk)
            };
            let passage_en = if ocr_text.is_empty() {
                desc_en.clone()
            } else {
                format!("{} | {}", ocr_text, desc_en)
            };
            let embed_uk = engine.embed_passage(&passage_uk)?;
            let embed_en = engine.embed_passage(&passage_en)?;
            let blob_uk = embedding_to_blob(&embed_uk);
            let blob_en = embedding_to_blob(&embed_en);
            update_stmt
                .execute(params![blob_uk, blob_en, id])
                .map_err(|e| e.to_string())?;
            if (idx + 1) % 50 == 0 || idx + 1 == total {
                println!("[MIGRATION] Recalculated {}/{} memes...", idx + 1, total);
            }
        }
    }
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    tx.execute(
        "INSERT OR REPLACE INTO _migrations (name, executed_at) VALUES (?, ?)",
        params![migratin_name, now],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    println!(
        "[MIGRATION] - {}: Successfully finished! All embeddings updated.",
        migratin_name
    );
    Ok(())
}

fn add_hash_column_to_memes(conn: &mut Connection) -> Result<(), String> {
    let migration_name = "1794117305_add_hash_to_memes".to_string();
    let force_recalculation = std::env::var("RECALCULATE_HASH").is_ok();

    let already_applied: bool = conn
        .query_row(
            "SELECT 1 FROM _migrations WHERE name = ?",
            params![migration_name],
            |_| Ok(true),
        )
        .unwrap_or(false);

    if already_applied && !force_recalculation {
        return Ok(());
    }

    println!(
        "[MIGRATION] - {}: Adding hash column to memes table for computed hash value...",
        migration_name
    );

    let mut stmt = conn
        .prepare("PRAGMA table_info(memes)")
        .map_err(|e| e.to_string())?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| e.to_string())?;

    let mut has_hash = false;
    for column in columns.flatten() {
        if column == "phash" {
            has_hash = true;
            break;
        }
    }

    drop(stmt);

    if !has_hash {
        conn.execute(
            "ALTER TABLE memes ADD COLUMN phash BIGINT NOT NULL DEFAULT 0",
            [],
        )
        .map_err(|e| e.to_string())?;
    }

    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    conn.execute(
        "INSERT OR REPLACE INTO _migrations (name, executed_at) VALUES (?, ?)",
        params![migration_name, now],
    )
    .map_err(|e| e.to_string())?;

    println!("[MIGRATION] - {}:  Successfully finished!", migration_name);
    Ok(())
}

fn add_media_type_column(conn: &mut Connection) -> Result<(), String> {
    let migration_name = "1794512263_add_media_type_column".to_string();

    let already_applied: bool = conn
        .query_row(
            "SELECT 1 FROM _migrations WHERE name = ?",
            params![migration_name],
            |_| Ok(true),
        )
        .unwrap_or(false);

    if already_applied {
        return Ok(());
    }

    println!(
        "[MIGRATION] - {}: Adding media type column to memes table...",
        migration_name
    );

    let mut stmt = conn
        .prepare("PRAGMA table_info(memes)")
        .map_err(|e| e.to_string())?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| e.to_string())?;

    let mut has_media_type = false;
    let mut has_thumbnail = false;
    for column in columns.flatten() {
        if column == "media_type" {
            has_media_type = true;
        }

        if column == "thumbnail_tg_id" {
            has_thumbnail = true;
        }
    }

    drop(stmt);

    if !has_media_type {
        conn.execute(
            "ALTER TABLE memes ADD COLUMN media_type INTEGER NOT NULL DEFAULT 0",
            [],
        )
        .map_err(|e| e.to_string())?;
    }
    if !has_thumbnail {
        conn.execute(
            "ALTER TABLE memes ADD COLUMN thumbnail_tg_id TEXT NOT NULL DEFAULT ''",
            [],
        )
        .map_err(|e| e.to_string())?;
    }

    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    conn.execute(
        "INSERT OR REPLACE INTO _migrations (name, executed_at) VALUES (?, ?)",
        params![migration_name, now],
    )
    .map_err(|e| e.to_string())?;

    println!("[MIGRATION] - {}:  Successfully finished!", migration_name);
    Ok(())
}
