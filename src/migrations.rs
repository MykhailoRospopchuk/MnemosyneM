use std::time::SystemTime;

use rusqlite::{Connection, params};
use tracing::{error, info};

use crate::database::entity::memes_fts::MemesFts;
use crate::engine::ClipEngine;
use crate::helpers::db_helper;
use crate::helpers::embeding_helper;

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
    add_vlm_version_column(&mut conn)?;
    add_memes_fts_index(&mut conn)?;
    add_user_reactions_table(&mut conn)?;
    add_drip_subscription_tables(&mut conn)?;
    add_tags_tables(&mut conn)?;
    add_vlm_model_quota_table(&mut conn)?;

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

    info!(
        migration = %migratin_name,
        "Recalculating embeddings for memes from description_uk & description_en"
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

    info!(migration = %migratin_name, total, "Found memes to recalculate embeddings");

    let tx = conn.transaction().map_err(|e| e.to_string())?;
    {
        let mut update_stmt = tx
            .prepare("UPDATE memes SET embedding_uk = ?, embedding_en = ? WHERE id = ?")
            .map_err(|e| e.to_string())?;
        for (idx, (id, ocr_text, desc_uk, desc_en)) in memes_to_update.into_iter().enumerate() {
            let passage_uk = if ocr_text.is_empty() {
                desc_uk.clone()
            } else {
                embeding_helper::format_embedding_passage(&ocr_text, &desc_uk)
            };
            let passage_en = if ocr_text.is_empty() {
                desc_en.clone()
            } else {
                embeding_helper::format_embedding_passage(&ocr_text, &desc_en)
            };
            let embed_uk = engine.embed_passage(&passage_uk)?;
            let embed_en = engine.embed_passage(&passage_en)?;
            let blob_uk = embeding_helper::embedding_to_blob(&embed_uk);
            let blob_en = embeding_helper::embedding_to_blob(&embed_en);
            update_stmt
                .execute(params![blob_uk, blob_en, id])
                .map_err(|e| e.to_string())?;
            if (idx + 1) % 50 == 0 || idx + 1 == total {
                info!(migration = %migratin_name, processed = idx + 1, total, "Recalculated embeddings progress");
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
    info!(
        migration = %migratin_name,
        "Migration finished: all embeddings updated"
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

    info!(
        migration = %migration_name,
        "Adding phash column to memes table"
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

    info!(migration = %migration_name, "Migration finished successfully");
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

    info!(
        migration = %migration_name,
        "Adding media_type and thumbnail_tg_id columns to memes table"
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

    info!(migration = %migration_name, "Migration finished successfully");
    Ok(())
}

fn add_vlm_version_column(conn: &mut Connection) -> Result<(), String> {
    let migration_name = "1794937142_add_vlm_version_column".to_string();

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

    info!(
        migration = %migration_name,
        "Adding vlm_version column to memes table"
    );

    let mut stmt = conn
        .prepare("PRAGMA table_info(memes)")
        .map_err(|e| e.to_string())?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| e.to_string())?;

    let mut has_vlm_version = false;
    for column in columns.flatten() {
        if column == "vlm_version" {
            has_vlm_version = true;
        }
    }

    drop(stmt);

    if !has_vlm_version {
        conn.execute(
            "ALTER TABLE memes ADD COLUMN vlm_version BIGINT NOT NULL DEFAULT 1",
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

    info!(migration = %migration_name, "Migration finished successfully");
    Ok(())
}

fn add_memes_fts_index(conn: &mut Connection) -> Result<(), String> {
    let migration_name = "1795026302_add_memes_fts_index".to_string();

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

    info!(
        migration = %migration_name,
        "Creating memes_fts virtual table and indexing rows"
    );

    conn.execute(
        "CREATE VIRTUAL TABLE IF NOT EXISTS memes_fts USING fts5 (
            ocr_norm,
            uk_norm,
            en_norm,
            title_norm
        )",
        [],
    )
    .map_err(|e| e.to_string())?;

    let mut stmt = conn
        .prepare("SELECT id, ocr_text, description_uk, description_en, title FROM memes")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            let id: u64 = row.get(0)?;
            let ocr_text: String = row.get(1)?;
            let desc_uk: String = row.get(2)?;
            let desc_en: String = row.get(3)?;
            let title: String = row.get(4)?;
            Ok(MemesFts::from_raw(
                id, &ocr_text, &desc_uk, &desc_en, &title,
            ))
        })
        .map_err(|e| e.to_string())?;
    let memes_to_update: Vec<MemesFts> = rows.filter_map(|r| r.ok()).collect();
    drop(stmt);

    let tx = conn.transaction().map_err(|e| e.to_string())?;
    for meme in memes_to_update {
        if let Err(e) = db_helper::upsert_fts_entry(&tx, meme) {
            error!(
                migration = %migration_name,
                error = %e,
                "Failed to insert meme_fts entry during migration"
            );
        }
    }
    tx.commit().map_err(|e| e.to_string())?;

    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    conn.execute(
        "INSERT OR REPLACE INTO _migrations (name, executed_at) VALUES (?, ?)",
        params![migration_name, now],
    )
    .map_err(|e| e.to_string())?;

    info!(migration = %migration_name, "Migration finished successfully");
    Ok(())
}

fn add_user_reactions_table(conn: &mut Connection) -> Result<(), String> {
    let migration_name = "1795440900_add_user_reactions_table".to_string();

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

    info!(
        migration = %migration_name,
        "Creating user_reactions table and index"
    );

    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS user_reactions (
            user_id BIGINT NOT NULL,
            meme_id BIGINT NOT NULL,
            reaction TEXT NOT NULL CHECK (reaction IN ('like', 'dislike', 'skip')),
            created_at BIGINT NOT NULL,
            PRIMARY KEY (user_id, meme_id)
        );
        CREATE INDEX IF NOT EXISTS idx_user_reactions_lookup
            ON user_reactions(user_id, reaction);
        ",
    )
    .map_err(|e| e.to_string())?;

    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    conn.execute(
        "INSERT OR REPLACE INTO _migrations (name, executed_at) VALUES (?, ?)",
        params![migration_name, now],
    )
    .map_err(|e| e.to_string())?;

    info!(migration = %migration_name, "Migration finished successfully");
    Ok(())
}

fn add_drip_subscription_tables(conn: &mut Connection) -> Result<(), String> {
    let migration_name = "1795446000_add_drip_subscription_tables".to_string();

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

    info!(
        migration = %migration_name,
        "Creating meme_subscriptions and user_delivered_memes tables"
    );

    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS meme_subscriptions (
            user_id BIGINT PRIMARY KEY,
            chat_id BIGINT NOT NULL,
            memes_per_day INTEGER NOT NULL DEFAULT 3,
            tz_offset_hours INTEGER NOT NULL DEFAULT 2,
            quiet_start_hour INTEGER NOT NULL DEFAULT 23,
            quiet_end_hour INTEGER NOT NULL DEFAULT 9,
            next_delivery_at BIGINT NOT NULL DEFAULT 0,
            is_active BOOLEAN NOT NULL DEFAULT 1,
            created_at BIGINT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_meme_subscriptions_due
            ON meme_subscriptions(is_active, next_delivery_at);
        CREATE TABLE IF NOT EXISTS user_delivered_memes (
            user_id BIGINT NOT NULL,
            meme_id BIGINT NOT NULL,
            delivered_at BIGINT NOT NULL,
            PRIMARY KEY (user_id, meme_id)
        );
        ",
    )
    .map_err(|e| e.to_string())?;

    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    conn.execute(
        "INSERT OR REPLACE INTO _migrations (name, executed_at) VALUES (?, ?)",
        params![migration_name, now],
    )
    .map_err(|e| e.to_string())?;

    info!(migration = %migration_name, "Migration finished successfully");
    Ok(())
}

fn add_tags_tables(conn: &mut Connection) -> Result<(), String> {
    let migration_name = "1795454000_add_tags_tables".to_string();

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

    info!(
        migration = %migration_name,
        "Creating tags, meme_tags tables and memes.tags_version column"
    );

    let mut stmt = conn
        .prepare("PRAGMA table_info(memes)")
        .map_err(|e| e.to_string())?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| e.to_string())?;

    let mut has_tags_version = false;
    for column in columns.flatten() {
        if column == "tags_version" {
            has_tags_version = true;
            break;
        }
    }
    drop(stmt);

    if !has_tags_version {
        conn.execute(
            "ALTER TABLE memes ADD COLUMN tags_version INTEGER NOT NULL DEFAULT 0",
            [],
        )
        .map_err(|e| e.to_string())?;
    }

    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS tags (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL UNIQUE,
            emoji TEXT NOT NULL DEFAULT '🏷️',
            instruction TEXT NOT NULL DEFAULT '',
            is_active BOOLEAN NOT NULL DEFAULT 1
        );
        CREATE TABLE IF NOT EXISTS meme_tags (
            meme_id BIGINT NOT NULL,
            tag_id INTEGER NOT NULL,
            confidence REAL NOT NULL DEFAULT 1.0,
            PRIMARY KEY (meme_id, tag_id)
        );
        CREATE INDEX IF NOT EXISTS idx_meme_tags_tag_id
            ON meme_tags(tag_id, meme_id);
        ",
    )
    .map_err(|e| e.to_string())?;

    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    conn.execute(
        "INSERT OR REPLACE INTO _migrations (name, executed_at) VALUES (?, ?)",
        params![migration_name, now],
    )
    .map_err(|e| e.to_string())?;

    info!(migration = %migration_name, "Migration finished successfully");
    Ok(())
}

fn add_vlm_model_quota_table(conn: &mut Connection) -> Result<(), String> {
    let migration_name = "1795458000_add_vlm_model_quota_table";

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

    info!(migration = %migration_name, "Creating vlm_model_quota table");

    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS vlm_model_quota (
            model_id TEXT PRIMARY KEY,
            pt_day_key TEXT NOT NULL,
            used_today INTEGER NOT NULL DEFAULT 0,
            cooldown_until INTEGER NOT NULL DEFAULT 0,
            last_reason TEXT NOT NULL DEFAULT '',
            updated_at INTEGER NOT NULL
        );
        ",
    )
    .map_err(|e| e.to_string())?;

    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    conn.execute(
        "INSERT OR REPLACE INTO _migrations (name, executed_at) VALUES (?, ?)",
        params![migration_name, now],
    )
    .map_err(|e| e.to_string())?;

    info!(migration = %migration_name, "Migration finished successfully");
    Ok(())
}
