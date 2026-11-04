use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, time::SystemTime};

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

    pub fn find_top_k(
        &self,
        query: &[f32],
        k: usize,
        user_id: i64,
        scope_visibility: String,
    ) -> Vec<(&Meme, f32)> {
        find_top_k(query, &self.memes, k, user_id, scope_visibility)
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

        let is_public = self.get_user_settings(user_id).upload_as_public;

        let meme = Meme::new(
            id,
            title,
            embed,
            file_tg_id,
            file_tg_unique_id,
            user_id,
            is_public,
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
        let settings = self.get_user_settings(user_id);
        let top_k = self.find_top_k(&embed, k, user_id, settings.search_scope);

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
            first_name TEXT,                          -- Telegram First Name
            last_name TEXT,                           -- Telegram Last Name
            search_scope TEXT NOT NULL DEFAULT 'all', -- 'all', 'public_only', 'private_only'
            created_at INTEGER NOT NULL,
            upload_as_public BOOLEAN NOT NULL DEFAULT 1
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
            .filter(|m| match settings.search_scope.as_str() {
                "private_only" => m.user_id == user_id,
                "public_only" => m.is_public,
                _ => m.user_id == user_id || m.is_public,
            })
            .take(limit)
            .collect()
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
    scope_visible: String,
) -> Vec<(&'a Meme, f32)> {
    let mut top_k: Vec<(&'a Meme, f32)> = Vec::with_capacity(memes.len());

    for n in 0..memes.len() {
        let is_visible = match scope_visible.as_str() {
            "private_only" => memes[n].user_id == user_id,
            "public_only" => memes[n].is_public,
            _ => memes[n].user_id == user_id || memes[n].is_public, // "all"
        };
        if is_visible {
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
