use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, time::SystemTime};

use crate::engine::ClipEngine;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Meme {
    pub id: u64,
    pub title: String,
    ocr_text: String,
    embedding_uk: Vec<f32>,
    embedding_en: Vec<f32>,
    pub file_tg_id: String,
    pub file_tg_unique_id: String,
    pub source_title: String,
    pub source_url: String,
    pub is_public: bool,
    pub owners: Vec<i64>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct UserMeme {
    pub user_id: i64,
    pub meme_id: u64,
    pub user_captions: String,
    is_public: bool,
    created_at: i64,
}

#[derive(Debug)]
pub struct MemeStore {
    conn: Connection,
    memes: Vec<Meme>,
    user_settings: HashMap<i64, UserSetting>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UserSetting {
    pub user_id: i64,
    pub username: String,
    pub first_name: String,
    pub last_name: String,
    pub upload_as_public: bool,
    pub search_scope: String,
}

pub enum BookMarkStatus {
    AlreadyInCollection,
    AddetInstantly,
    NewMemeNeeded,
}

impl Default for UserSetting {
    fn default() -> Self {
        Self {
            user_id: 0,
            username: "".to_string(),
            first_name: "".to_string(),
            last_name: "".to_string(),
            upload_as_public: true,
            search_scope: "all".to_string(),
        }
    }
}

impl Meme {
    pub fn is_owned_by(&self, user_id: i64) -> bool {
        self.owners.contains(&user_id)
    }

    pub fn is_visible_to(&self, user_id: i64, scope: &str) -> bool {
        match scope {
            "private_only" => self.is_owned_by(user_id),
            "public_only" => self.is_public,
            _ => self.is_public || self.is_owned_by(user_id),
        }
    }
}

impl MemeStore {
    pub fn check_or_add_bookmark(
        &mut self,
        unique_id: &str,
        user_id: i64,
        caption: &str,
    ) -> Result<BookMarkStatus, String> {
        let mame_opt = self
            .memes
            .iter()
            .find(|m| m.file_tg_unique_id == unique_id)
            .cloned();

        if let Some(meme) = mame_opt {
            if meme.is_owned_by(user_id) {
                return Ok(BookMarkStatus::AlreadyInCollection);
            }

            let user_settings = self.get_user_settings(user_id);
            let now = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            self.conn.execute("
                INSERT OR REPLACE INTO user_memes (user_id, meme_id, user_captions, is_public, created_at)
                VALUES (?, ?, ?, ?, ?)",
                params![user_id, meme.id, caption, user_settings.upload_as_public, now])
            .map_err(|e| e.to_string())?;

            if let Some(m) = self.memes.iter_mut().find(|m| m.id == meme.id) {
                m.owners.push(user_id);
                if user_settings.upload_as_public {
                    m.is_public = true;
                }
            }

            return Ok(BookMarkStatus::AddetInstantly);
        }

        Ok(BookMarkStatus::NewMemeNeeded)
    }

    pub fn add_from_analysis(
        &mut self,
        id: u64,
        title: String,
        analysis: &crate::vlm::MemeAnalysis,
        file_tg_id: String,
        file_tg_unique_id: String,
        user_id: i64,
        source_title: String,
        source_url: String,
        engine: &mut ClipEngine,
    ) -> Result<(), String> {
        let passage_uk = format!("{} | {}", analysis.ocr_text, analysis.uk_description);
        let passage_en = format!("{} | {}", analysis.ocr_text, analysis.en_description);

        let embed_uk = engine
            .embed_passage(&passage_uk)
            .map_err(|e| e.to_string())?;
        let embed_en = engine
            .embed_passage(&passage_en)
            .map_err(|e| e.to_string())?;

        let is_public = self.get_user_settings(user_id).upload_as_public;

        let blob_uk = self::embedding_to_blob(&embed_uk);
        let blob_en = self::embedding_to_blob(&embed_en);

        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        self.conn
            .execute(
                "INSERT INTO memes (
                    id, title, ocr_text, description_uk, description_en,
                    embedding_uk, embedding_en,
                    file_tg_id, file_tg_unique_id, source_title, source_url
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT (file_tg_unique_id)
                DO UPDATE SET
                    title = excluded.title,
                    ocr_text = excluded.ocr_text,
                    description_uk = excluded.description_uk,
                    description_en = excluded.description_en,
                    embedding_uk = excluded.embedding_uk,
                    embedding_en = excluded.embedding_en,
                    source_title = excluded.source_title,
                    source_url = excluded.source_url ;",
                params![
                    id,
                    title,
                    analysis.ocr_text,
                    analysis.uk_description,
                    analysis.en_description,
                    blob_uk,
                    blob_en,
                    file_tg_id,
                    file_tg_unique_id,
                    source_title,
                    source_url
                ],
            )
            .map_err(|e| e.to_string())?;

        self.conn.execute("
            INSERT OR REPLACE INTO user_memes (user_id, meme_id, user_captions, is_public, created_at)
            VALUES (?, ?, ?, ?, ?)",
            params![
                user_id, id, title, is_public, now
            ]).map_err(|e| e.to_string())?;

        let meme = Meme {
            id,
            title,
            ocr_text: analysis.ocr_text.clone(),
            embedding_uk: embed_uk,
            embedding_en: embed_en,
            file_tg_id,
            file_tg_unique_id,
            source_title,
            source_url,
            is_public,
            owners: vec![user_id],
        };

        if let Some(item) = self
            .memes
            .iter_mut()
            .find(|x| x.file_tg_unique_id == meme.file_tg_unique_id)
        {
            *item = meme;
        } else {
            self.memes.push(meme);
        }

        Ok(())
    }

    pub fn len(&self, user_id: i64) -> (usize, usize) {
        let mut public: usize = 0;
        let mut private: usize = 0;

        for m in &self.memes {
            if m.is_public {
                public += 1;
            }

            if m.is_owned_by(user_id) {
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
            .filter(|m| m.is_visible_to(user_id, "all"));

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
            .filter(|i| i.is_visible_to(user_id, "all"))
            .nth(idx);
    }

    pub fn open(db_connection: &str) -> Result<Self, String> {
        let conn = Connection::open(db_connection).map_err(|e| e.to_string())?;
        conn.execute_batch(
            "\
            CREATE TABLE IF NOT EXISTS memes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                title TEXT NOT NULL,
                ocr_text TEXT NOT NULL,
                description_uk TEXT NOT NULL,
                description_en TEXT NOT NULL,
                embedding_uk BLOB NOT NULL,
                embedding_en BLOB NOT NULL,
                file_tg_id TEXT NOT NULL UNIQUE,
                file_tg_unique_id TEXT NOT NULL UNIQUE,
                source_title TEXT NOT NULL DEFAULT '',
                source_url TEXT NOT NULL DEFAULT ''
            );

            CREATE TABLE IF NOT EXISTS users (
                user_id BIGINT PRIMARY KEY,
                username TEXT,
                first_name TEXT,
                last_name TEXT,
                search_scope TEXT NOT NULL DEFAULT 'all',
                created_at BIGINT NOT NULL,
                upload_as_public BOOLEAN NOT NULL DEFAULT 1
            );

            CREATE TABLE IF NOT EXISTS user_memes (
                user_id BIGINT NOT NULL,
                meme_id INTEGER NOT NULL,
                user_captions TEXT NOT NULL DEFAULT '',
                is_public BOOLEAN NOT NULL DEFAULT 0,
                created_at BIGINT NOT NULL,
                PRIMARY KEY (user_id, meme_id)
            );
            ",
        )
        .map_err(|e| e.to_string())?;

        let mut stmt = conn
                    .prepare("SELECT id, title, ocr_text, embedding_uk, embedding_en, file_tg_id, file_tg_unique_id, source_title, source_url FROM memes")
                    .map_err(|e| e.to_string())?;
        let meme_iter = stmt
            .query_map([], |row| {
                let id: u64 = row.get(0)?;
                let title: String = row.get(1)?;
                let ocr_text: String = row.get(2)?;
                let blob_uk: Vec<u8> = row.get(3)?;
                let blob_en: Vec<u8> = row.get(4)?;
                let file_tg_id: String = row.get(5)?;
                let file_tg_unique_id: String = row.get(6)?;
                let source_title: String = row.get(7)?;
                let source_url: String = row.get(8)?;
                Ok(Meme {
                    id,
                    title,
                    ocr_text,
                    embedding_uk: blob_to_embedding(&blob_uk),
                    embedding_en: blob_to_embedding(&blob_en),
                    file_tg_id,
                    file_tg_unique_id,
                    source_title,
                    source_url,
                    is_public: false,
                    owners: vec![],
                })
            })
            .map_err(|e| e.to_string())?;

        let mut memes_map: HashMap<u64, Meme> = HashMap::new();
        for meme in meme_iter {
            let item = meme.map_err(|e| e.to_string());
            match item {
                Ok(item) => memes_map.insert(item.id, item),
                Err(e) => return Err(e.to_string()),
            };
        }
        drop(stmt);

        let mut um_stmt = conn
            .prepare(
                "SELECT user_id, meme_id, user_captions, is_public, created_at from user_memes",
            )
            .map_err(|e| e.to_string())?;

        let um_iter = um_stmt
            .query_map([], |row| {
                let user_id: i64 = row.get(0)?;
                let meme_id: u64 = row.get(1)?;
                let user_captions: String = row.get(2)?;
                let is_public: bool = row.get(3)?;
                let created_at: i64 = row.get(4)?;
                Ok(UserMeme {
                    user_id,
                    meme_id,
                    user_captions,
                    is_public,
                    created_at,
                })
            })
            .map_err(|e| e.to_string())?;

        for um in um_iter.flatten() {
            if let Some(mem) = memes_map.get_mut(&um.meme_id) {
                mem.owners.push(um.user_id);
                if um.is_public {
                    mem.is_public = true;
                }
            }
        }

        drop(um_stmt);

        let memes: Vec<Meme> = memes_map.into_values().collect();

        let mut user_smpt = conn
            .prepare("SELECT user_id, username, search_scope, upload_as_public, first_name, last_name FROM users")
            .map_err(|e| e.to_string())?;

        let user_iter = user_smpt
            .query_map([], |row| {
                let user_id: i64 = row.get(0)?;
                let username: String = row.get::<_, Option<String>>(1)?.unwrap_or_default();
                let search_scope: String = row
                    .get::<_, Option<String>>(2)?
                    .unwrap_or_else(|| "all".to_string());
                let upload_as_public: bool = row.get::<_, Option<bool>>(3)?.unwrap_or(true);
                let first_name: String = row.get::<_, Option<String>>(4)?.unwrap_or_default();
                let last_name: String = row.get::<_, Option<String>>(5)?.unwrap_or_default();

                Ok(UserSetting {
                    user_id,
                    username,
                    search_scope,
                    upload_as_public,
                    first_name,
                    last_name,
                })
            })
            .map_err(|e| e.to_string())?;

        let mut useres: HashMap<i64, UserSetting> = HashMap::new();
        for user in user_iter {
            if let Ok(u) = user {
                useres.insert(u.user_id, u);
            }
        }

        drop(user_smpt);

        Ok(Self {
            conn,
            memes,
            user_settings: useres,
        })
    }

    pub fn get_user_settings(&self, user_id: i64) -> UserSetting {
        self.user_settings
            .get(&user_id)
            .cloned()
            .unwrap_or_else(|| UserSetting {
                user_id,
                username: "".to_string(),
                first_name: "".to_string(),
                last_name: "".to_string(),
                upload_as_public: true,
                search_scope: "all".to_string(),
            })
    }

    pub fn save_users_settings(&mut self, settings: UserSetting) -> Result<(), String> {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let sql = "
            INSERT INTO users (user_id, username, search_scope, created_at, upload_as_public, first_name, last_name)
            VALUES (?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(user_id) DO UPDATE SET
                username = excluded.username,
                search_scope = excluded.search_scope,
                upload_as_public = excluded.upload_as_public,
                first_name = excluded.first_name,
                last_name = excluded.last_name
            ";
        self.conn
            .execute(
                sql,
                params![
                    settings.user_id,
                    settings.username,
                    settings.search_scope,
                    now,
                    settings.upload_as_public,
                    settings.first_name,
                    settings.last_name
                ],
            )
            .map_err(|e| e.to_string())?;

        self.user_settings.insert(settings.user_id, settings);
        Ok(())
    }

    pub fn toggle_upload_mode(&mut self, user_id: i64) -> Result<bool, String> {
        let mut user_settigns = self.get_user_settings(user_id);

        let resulted = !user_settigns.upload_as_public;
        user_settigns.upload_as_public = resulted;

        self.save_users_settings(user_settigns)?;
        Ok(resulted)
    }

    pub fn cycle_search_scope(&mut self, user_id: i64) -> Result<String, String> {
        let mut user_settings = self.get_user_settings(user_id);

        let new_search_scope = match user_settings.search_scope.as_str() {
            "all" => "private_only".to_string(),
            "private_only" => "public_only".to_string(),
            _ => "all".to_string(),
        };
        user_settings.search_scope = new_search_scope.clone();

        self.save_users_settings(user_settings)?;
        Ok(new_search_scope)
    }

    pub fn sync_user(
        &mut self,
        user_id: i64,
        username: &str,
        first_name: &str,
        last_name: &str,
    ) -> Result<(), String> {
        match self.user_settings.get_mut(&user_id) {
            Some(exiting) => {
                if (!username.is_empty() && exiting.username != username)
                    || (!first_name.is_empty() && exiting.first_name != first_name)
                    || (!last_name.is_empty() && exiting.last_name != last_name)
                {
                    exiting.username = username.to_string();
                    exiting.first_name = first_name.to_string();
                    exiting.last_name = last_name.to_string();

                    let updated = exiting.clone();
                    self.save_users_settings(updated)?;
                }
            }
            None => {
                let new_user = UserSetting {
                    user_id,
                    username: username.to_string(),
                    first_name: first_name.to_string(),
                    last_name: last_name.to_string(),
                    upload_as_public: true,
                    search_scope: "all".to_string(),
                };
                self.save_users_settings(new_user)?;
            }
        }
        Ok(())
    }

    pub fn get_recent_memes(&self, user_id: i64, limit: usize) -> Vec<&Meme> {
        let settings = self.get_user_settings(user_id);
        self.memes
            .iter()
            .rev()
            .filter(|m| m.is_visible_to(user_id, &settings.search_scope))
            .take(limit)
            .collect()
    }

    pub fn search_top_k<'a>(
        &'a self,
        query_text: &str,
        engine: &mut ClipEngine,
        k: usize,
        user_id: i64,
    ) -> Result<Vec<(&'a Meme, f32)>, String> {
        let embed = engine.embed_query(query_text).map_err(|e| e.to_string())?;
        let settings = self.get_user_settings(user_id);
        let top_k = self.find_top_k(&embed, query_text, k, user_id, settings.search_scope);
        Ok(top_k)
    }

    pub fn find_top_k(
        &self,
        query: &[f32],
        query_text: &str,
        k: usize,
        user_id: i64,
        scope_visibility: String,
    ) -> Vec<(&Meme, f32)> {
        find_top_k(query, query_text, &self.memes, k, user_id, scope_visibility)
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
    query_text: &str,
    memes: &'a [Meme],
    k: usize,
    user_id: i64,
    scope_visible: String,
) -> Vec<(&'a Meme, f32)> {
    let mut top_k: Vec<(&'a Meme, f32)> = Vec::with_capacity(memes.len());
    let query_lower = query_text.to_lowercase();
    let query_words: Vec<&str> = query_lower
        .split_whitespace()
        .filter(|w| w.len() >= 3)
        .collect();
    for meme in memes {
        if !meme.is_visible_to(user_id, &scope_visible) {
            continue;
        };

        let sim_uk = cosine_similarity(query, &meme.embedding_uk).unwrap_or(0.0);
        let sim_en = cosine_similarity(query, &meme.embedding_en).unwrap_or(0.0);
        let mut best_score = f32::max(sim_uk, sim_en);

        if !meme.ocr_text.is_empty() && !query_words.is_empty() {
            let ocr_lower = meme.ocr_text.to_lowercase();
            let matches = query_words
                .iter()
                .filter(|&&w| ocr_lower.contains(w))
                .count();
            if matches > 0 {
                let boost = 0.04 * (matches as f32 / query_words.len() as f32);
                best_score = (best_score + boost).min(1.0);
            }
        }
        if best_score > 0.05 {
            top_k.push((meme, best_score));
        }
    }
    top_k.sort_by(|b, a| a.1.total_cmp(&b.1));
    top_k.truncate(k);
    top_k
}
