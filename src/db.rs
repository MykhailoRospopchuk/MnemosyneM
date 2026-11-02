use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::time::SystemTime;

use crate::engine::ClipEngine;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Meme {
    pub id: u64,
    pub title: String,
    pub embeding: Vec<f32>,
    pub file_tg_id: String,
    pub file_tg_unique_id: String,
    pub user_id: i64,
    pub is_public: bool,
}

impl Meme {
    pub fn new(
        id: u64,
        title: String,
        embeding: Vec<f32>,
        file_tg_id: String,
        file_tg_unique_id: String,
        user_id: i64,
        is_public: bool,
    ) -> Self {
        Self {
            id,
            title,
            embeding,
            file_tg_id,
            file_tg_unique_id,
            user_id,
            is_public,
        }
    }
}

#[derive(Debug)]
pub struct MemeStore {
    conn: Connection,
    memes: Vec<Meme>,
}

impl MemeStore {
    pub fn add(&mut self, meme: Meme) -> Result<(), String> {
        let blob = self::embedding_to_blob(&meme.embeding);
        self.conn
            .execute(
                "INSERT INTO memes (id, title, file_tg_id, file_tg_unique_id, embedding, user_id, is_public)
                VALUES (?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT (file_tg_unique_id)
                DO UPDATE SET title = excluded.title;",
                params![
                    meme.id,
                    meme.title,
                    meme.file_tg_id,
                    meme.file_tg_unique_id,
                    blob,
                    meme.user_id,
                    meme.is_public
                ],
            )
            .map_err(|e| e.to_string())?;

        if let Some(item) = self
            .memes
            .iter_mut()
            .find(|x| x.file_tg_unique_id == meme.file_tg_unique_id)
        {
            item.title = meme.title;
            item.embeding = meme.embeding;
            item.file_tg_id = meme.file_tg_id;
        } else {
            self.memes.push(meme);
        }

        Ok(())
    }

    pub fn conains_meme(&self, unique_tg_id: &str) -> bool {
        self.memes
            .iter()
            .any(|meme| meme.file_tg_unique_id == unique_tg_id)
    }

    pub fn find_top_k(&self, query: &[f32], k: usize, user_id: i64) -> Vec<(&Meme, f32)> {
        find_top_k(query, &self.memes, k, user_id)
    }

    pub fn len(&self, user_id: i64) -> (usize, usize) {
        let mut public: usize = 0;
        let mut private: usize = 0;

        for m in &self.memes {
            if m.is_public {
                public += 1;
            }

            if m.user_id == user_id {
                private += 1;
            }
        }

        return (public, private);
    }

    pub fn len_all(&self) -> usize {
        self.memes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len_all() == 0
    }

    pub fn random_meme(&self, user_id: i64) -> Option<&Meme> {
        if self.is_empty() {
            return None;
        }
        let matching_memes = self
            .memes
            .iter()
            .filter(|m| m.user_id == user_id || m.is_public);

        let count_matched = matching_memes.clone().count();

        if count_matched == 0 {
            return None;
        }

        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos() as usize;

        let idx = nanos % count_matched;

        return self
            .memes
            .iter()
            .filter(|i| i.user_id == user_id || i.is_public)
            .nth(idx);
    }

    pub fn add_from_descriptions(
        &mut self,
        id: u64,
        title: String,
        file_tg_id: String,
        file_tg_unique_id: String,
        user_id: i64,
        descriptions: String,
        engine: &mut ClipEngine,
    ) -> Result<(), String> {
        let embed = engine
            .embed_passage(&descriptions)
            .map_err(|e| e.to_string())?;

        let meme = Meme::new(
            id,
            title,
            embed,
            file_tg_id,
            file_tg_unique_id,
            user_id,
            true,
        );
        self.add(meme)?;
        Ok(())
    }

    pub fn search_top_k<'a>(
        &'a self,
        query_text: &str,
        engine: &mut ClipEngine,
        k: usize,
        user_id: i64,
    ) -> Result<Vec<(&'a Meme, f32)>, String> {
        let embed = engine.embed_query(query_text).map_err(|e| e.to_string())?;

        let top_k = self.find_top_k(&embed, k, user_id);

        Ok(top_k)
    }

    pub fn open(db_connection: &str) -> Result<Self, String> {
        let conn = Connection::open(db_connection).map_err(|e| e.to_string())?;
        conn.execute_batch(
            "\
        CREATE TABLE IF NOT EXISTS memes (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            title TEXT NOT NULL, \
            file_tg_id TEXT NOT NULL, \
            file_tg_unique_id TEXT NOT NULL UNIQUE, \
            embedding BLOB NOT NULL,\
            user_id BIGINT NOT NULL DEFAULT 0,\
            is_public BOOLEAN NOT NULL DEFAULT 0
            );

        CREATE TABLE IF NOT EXISTS users (
            user_id BIGINT PRIMARY KEY,               -- Telegram User ID
            username TEXT,                            -- Telegram @username
            search_scope TEXT NOT NULL DEFAULT 'all', -- 'all', 'public_only', 'private_only'
            created_at INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_memes_user_is_public ON memes (user_id, is_public);
            ",
        )
        .map_err(|e| e.to_string())?;

        let mut stmt = conn
            .prepare("SELECT * FROM memes")
            .map_err(|e| e.to_string())?;

        let meme_iter = stmt
            .query_map([], |row| {
                let id: u64 = row.get(0)?;
                let title: String = row.get(1)?;
                let file_tg_id: String = row.get(2)?;
                let file_tg_unique_id: String = row.get(3)?;
                let blob: Vec<u8> = row.get(4)?;
                let embedding = blob_to_embedding(&blob);
                let user_id = row.get(5)?;
                let is_public = row.get(6)?;

                Ok(Meme::new(
                    id,
                    title,
                    embedding,
                    file_tg_id,
                    file_tg_unique_id,
                    user_id,
                    is_public,
                ))
            })
            .map_err(|e| e.to_string())?;

        let mut memes: Vec<Meme> = Vec::new();
        for meme in meme_iter {
            let item = meme.map_err(|e| e.to_string());
            match item {
                Ok(item) => memes.push(item),
                Err(e) => return Err(e.to_string()),
            }
        }
        drop(stmt);
        Ok(Self { conn, memes })
    }
}

pub fn embedding_to_blob(embedding: &[f32]) -> Vec<u8> {
    embedding
        .iter()
        .flat_map(|&val| val.to_le_bytes())
        .collect()
}
pub fn blob_to_embedding(blob: &[u8]) -> Vec<f32> {
    blob.chunks_exact(4)
        .map(|chunk| f32::from_le_bytes(chunk.try_into().unwrap()))
        .collect()
}

pub fn cosine_similarity(a: &[f32], b: &[f32]) -> Result<f32, String> {
    if a.len() != b.len() {
        return Err(format!("{} != {}", a.len(), b.len()));
    }

    let mut dot_product = 0f32;
    let mut sqrt_sq_a = 0f32;
    let mut sqrt_sq_b = 0f32;

    for (&x, &y) in a.iter().zip(b.iter()) {
        dot_product += x * y;
        sqrt_sq_a += x * x;
        sqrt_sq_b += y * y;
    }

    Ok(dot_product / (sqrt_sq_a.sqrt() * sqrt_sq_b.sqrt()))
}

pub fn find_top_k<'a>(
    query: &[f32],
    memes: &'a [Meme],
    k: usize,
    user_id: i64,
) -> Vec<(&'a Meme, f32)> {
    let mut top_k: Vec<(&'a Meme, f32)> = Vec::with_capacity(memes.len());

    for n in 0..memes.len() {
        if memes[n].user_id == user_id || memes[n].is_public {
            if let Ok(value) = cosine_similarity(query, &memes[n].embeding) {
                top_k.push((&memes[n], value));
            }
        }
    }

    top_k.retain(|(_, value)| value > &0.05);
    top_k.sort_by(|b, a| a.1.total_cmp(&b.1));

    top_k.truncate(k);
    top_k
}
