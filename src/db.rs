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
}

impl Meme {
    pub fn new(
        id: u64,
        title: String,
        embeding: Vec<f32>,
        file_tg_id: String,
        file_tg_unique_id: String,
    ) -> Self {
        Self {
            id,
            title,
            embeding,
            file_tg_id,
            file_tg_unique_id,
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
                "INSERT INTO memes (id, title, file_tg_id, file_tg_unique_id, embedding)
                VALUES (?, ?, ?, ?, ?)
                ON CONFLICT (file_tg_unique_id)
                DO UPDATE SET title = excluded.title;",
                params![
                    meme.id,
                    meme.title,
                    meme.file_tg_id,
                    meme.file_tg_unique_id,
                    blob
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

    pub fn find_top_k(&self, query: &[f32], k: usize) -> Vec<(&Meme, f32)> {
        find_top_k(query, &self.memes, k)
    }

    pub fn len(&self) -> usize {
        self.memes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn random_meme(&self) -> Option<&Meme> {
        if self.is_empty() {
            return None;
        }

        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos() as usize;

        let idx = nanos % self.memes.len();
        Some(&self.memes[idx])
    }

    pub fn add_from_descriptions(
        &mut self,
        id: u64,
        title: String,
        file_tg_id: String,
        file_tg_unique_id: String,
        descriptions: String,
        engine: &mut ClipEngine,
    ) -> Result<(), String> {
        let embed = engine
            .embed_passage(&descriptions)
            .map_err(|e| e.to_string())?;

        let meme = Meme::new(id, title, embed, file_tg_id, file_tg_unique_id);
        self.add(meme)?;
        Ok(())
    }

    pub fn search_top_k<'a>(
        &'a self,
        query_text: &str,
        engine: &mut ClipEngine,
        k: usize,
    ) -> Result<Vec<(&'a Meme, f32)>, String> {
        let embed = engine.embed_query(query_text).map_err(|e| e.to_string())?;

        let top_k = self.find_top_k(&embed, k);

        Ok(top_k)
    }

    pub fn open(db_connection: &str) -> Result<Self, String> {
        let conn = Connection::open(db_connection).map_err(|e| e.to_string())?;
        conn.execute(
            "\
        CREATE TABLE IF NOT EXISTS memes (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            title TEXT NOT NULL, \
            file_tg_id TEXT NOT NULL, \
            file_tg_unique_id TEXT NOT NULL UNIQUE, \
            embedding BLOB NOT NULL\
            );",
            [],
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

                Ok(Meme::new(
                    id,
                    title,
                    embedding,
                    file_tg_id,
                    file_tg_unique_id,
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

pub fn find_top_k<'a>(query: &[f32], memes: &'a [Meme], k: usize) -> Vec<(&'a Meme, f32)> {
    let mut top_k: Vec<(&'a Meme, f32)> = Vec::with_capacity(memes.len());

    for n in 0..memes.len() {
        if let Ok(value) = cosine_similarity(query, &memes[n].embeding) {
            top_k.push((&memes[n], value));
        }
    }

    top_k.retain(|(_, value)| value > &0.05);
    top_k.sort_by(|b, a| a.1.total_cmp(&b.1));

    top_k.truncate(k);
    top_k
}
