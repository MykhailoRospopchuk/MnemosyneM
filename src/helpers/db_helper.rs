use rusqlite::{Connection, params};

use crate::database::entity::memes_fts::MemesFts;

pub fn upsert_fts_entry(conn: &Connection, meme_fts: MemesFts) -> Result<(), String> {
    conn.execute(
        "
        DELETE FROM memes_fts WHERE rowid = ?;",
        params![meme_fts.row_id,],
    )
    .map_err(|e| e.to_string())?;

    conn.execute(
        "
        INSERT INTO memes_fts (rowid, ocr_norm, uk_norm, en_norm, title_norm)
        VALUES (?, ?, ?, ?, ?)",
        params![
            meme_fts.row_id,
            meme_fts.ocr_norm,
            meme_fts.uk_norm,
            meme_fts.en_norm,
            meme_fts.title_norm
        ],
    )
    .map_err(|e| e.to_string())?;

    Ok(())
}
