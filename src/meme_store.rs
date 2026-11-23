use crate::{
    config::AppConfig,
    constants::TAGS_STATE_UNDETERMINED,
    database::entity::memes_fts::MemesFts,
    helpers::{
        db_helper::upsert_fts_entry, embeding_helper::format_embedding_passage, lexical_helper,
    },
    tagger::{DiscoverySample, DynamicTagDef, OTHER_TAG_EMOJI, OTHER_TAG_SLUG, TAG_TAXONOMY, tag_emoji},
    vlm::{MemeAnalysis, PersistedModelQuota},
};
use rusqlite::{Connection, params};
use std::{
    collections::{HashMap, HashSet, hash_map::RandomState},
    hash::{BuildHasher, Hasher},
    path::Path,
    time::SystemTime,
};
use tracing::{error, warn};

use crate::{
    database::{
        entity::{
            media_type::MediaType, meme::Meme, new_meme::NewMeme, user_meme::UserMeme,
            user_setting::UserSetting,
        },
        enums::book_mark_status_enum::BookMarkStatus,
    },
    engine::ClipEngine,
    helpers::{
        embeding_helper::{blob_to_embedding, embedding_to_blob},
        hash_helper::hamming_distance,
    },
};

const BM25_MATCH_LIMIT: usize = 40;

#[derive(Debug)]
pub struct MemeStore {
    conn: Connection,
    memes: Vec<Meme>,
    user_settings: HashMap<i64, UserSetting>,
}

pub struct ReprocessCandidate {
    pub id: u64,
    pub file_tg_id: String,
    pub media_type: MediaType,
    pub title: String,
}

pub struct Stage1Candidate {
    pub meme: Meme,
    pub passage: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UserReactionCounts {
    pub likes: usize,
    pub dislikes: usize,
    pub skips: usize,
}

impl UserReactionCounts {
    pub fn total(&self) -> usize {
        self.likes + self.dislikes + self.skips
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DripSubscription {
    pub user_id: i64,
    pub chat_id: i64,
    pub memes_per_day: u8,
    pub tz_offset_hours: i8,
    pub quiet_start_hour: u8,
    pub quiet_end_hour: u8,
    pub next_delivery_at: u64,
    pub is_active: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SearchFilter<'a> {
    pub media: Option<MediaType>,
    pub tags: &'a [String],
}

#[derive(Debug, Clone)]
pub struct TagCandidate {
    pub id: u64,
    pub title: String,
    pub ocr_text: String,
    pub description_uk: String,
    pub description_en: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemeCardInfo {
    pub user_is_public: bool,
    pub effective_is_public: bool,
    pub tags: Vec<(String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteMemeOutcome {
    DeletedCompletely,
    RemovedFromCollection,
    NotFoundOrNotOwner,
}

struct FtsHit {
    bm25_score: f32,
    matched_terms: usize,
}

struct Fts35 {
    rowid: u64,
    ocr_norm: String,
    uk_norm: String,
    en_norm: String,
    title_norm: String,
    bm25_score: f32,
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

            return Ok(BookMarkStatus::AddedInstantly);
        }

        Ok(BookMarkStatus::NewMemeNeeded)
    }

    pub fn add_from_analysis(
        &mut self,
        new_meme: NewMeme<'_>,
        engine: &mut ClipEngine,
    ) -> Result<(u64, bool), String> {
        let passage_uk = format_embedding_passage(
            &new_meme.analysis.ocr_text,
            &new_meme.analysis.uk_description,
        );
        let passage_en = format_embedding_passage(
            &new_meme.analysis.ocr_text,
            &new_meme.analysis.en_description,
        );

        let embed_uk = engine
            .embed_passage(&passage_uk)
            .map_err(|e| e.to_string())?;
        let embed_en = engine
            .embed_passage(&passage_en)
            .map_err(|e| e.to_string())?;

        let is_public = self.get_user_settings(new_meme.user_id).upload_as_public;

        let blob_uk = embedding_to_blob(&embed_uk);
        let blob_en = embedding_to_blob(&embed_en);

        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let upserted_id: u64 = self.conn
            .query_row(
                "INSERT INTO memes (
                    id, title, ocr_text, description_uk, description_en,
                    embedding_uk, embedding_en,
                    file_tg_id, file_tg_unique_id, source_title, source_url, phash, media_type, thumbnail_tg_id, vlm_version
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT (file_tg_unique_id)
                DO UPDATE SET
                    title = excluded.title,
                    ocr_text = excluded.ocr_text,
                    description_uk = excluded.description_uk,
                    description_en = excluded.description_en,
                    embedding_uk = excluded.embedding_uk,
                    embedding_en = excluded.embedding_en,
                    source_title = excluded.source_title,
                    source_url = excluded.source_url,
                    phash = excluded.phash,
                    media_type = excluded.media_type,
                    thumbnail_tg_id = excluded.thumbnail_tg_id,
                    vlm_version = excluded.vlm_version
                RETURNING id;",
                params![
                    new_meme.id,
                    new_meme.title,
                    new_meme.analysis.ocr_text,
                    new_meme.analysis.uk_description,
                    new_meme.analysis.en_description,
                    blob_uk,
                    blob_en,
                    new_meme.file_tg_id,
                    new_meme.file_tg_unique_id,
                    new_meme.source_title,
                    new_meme.source_url,
                    new_meme.phash as i64,
                    new_meme.media_type.as_i32(),
                    new_meme.thumbnail_tg_id,
                    new_meme.vlm_version
                ],
                |row| row.get(0)
            )
            .map_err(|e| e.to_string())?;

        self.conn.execute("
            INSERT OR REPLACE INTO user_memes (user_id, meme_id, user_captions, is_public, created_at)
            VALUES (?, ?, ?, ?, ?)",
            params![
                new_meme.user_id, upserted_id, new_meme.title, is_public, now
            ]).map_err(|e| e.to_string())?;

        let effective_public: bool = self
            .conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM user_memes WHERE meme_id = ? AND is_public = 1)",
                params![upserted_id],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;

        let mut meme = Meme {
            id: upserted_id,
            title: new_meme.title.clone(),
            ocr_text: new_meme.analysis.ocr_text.clone(),
            embedding_uk: embed_uk,
            embedding_en: embed_en,
            file_tg_id: new_meme.file_tg_id,
            file_tg_unique_id: new_meme.file_tg_unique_id,
            source_title: new_meme.source_title,
            source_url: new_meme.source_url,
            is_public: effective_public,
            owners: vec![new_meme.user_id],
            phash: new_meme.phash,
            media_type: new_meme.media_type,
            thumbnail_tg_id: new_meme.thumbnail_tg_id,
            tags: Vec::new(),
        };

        if let Some(item) = self
            .memes
            .iter_mut()
            .find(|x| x.file_tg_unique_id == meme.file_tg_unique_id)
        {
            let mut merged_owners = std::mem::take(&mut item.owners);
            if !merged_owners.contains(&new_meme.user_id) {
                merged_owners.push(new_meme.user_id);
            }
            meme.owners = merged_owners;
            meme.tags = std::mem::take(&mut item.tags);
            *item = meme;
        } else {
            self.memes.push(meme);
        }

        upsert_fts_entry(
            &self.conn,
            MemesFts::from_raw(
                upserted_id,
                &new_meme.analysis.ocr_text,
                &new_meme.analysis.uk_description,
                &new_meme.analysis.en_description,
                &new_meme.title,
            ),
        )
        .map_err(|e| e.to_string())?;

        Ok((upserted_id, is_public))
    }

    pub fn len(&self, user_id: i64) -> (usize, usize) {
        let mut public: usize = 0;
        let mut private: usize = 0;

        for m in &self.memes {
            if m.is_public {
                public += 1;
            }

            if m.is_owned_by(user_id) && !m.is_public {
                private += 1;
            }
        }

        (public, private)
    }

    pub fn len_all(&self) -> usize {
        self.memes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len_all() == 0
    }

    pub fn random_meme(&self, user_id: i64) -> Option<&Meme> {
        self.random_meme_excluding(user_id, None)
    }

    pub fn random_meme_excluding(
        &self,
        user_id: i64,
        exclude_meme_id: Option<u64>,
    ) -> Option<&Meme> {
        if self.is_empty() {
            return None;
        }

        let settings = self.get_user_settings(user_id);
        let mut disliked_ids: HashSet<u64> = HashSet::new();
        if let Ok(mut stmt) = self.conn.prepare(
            "SELECT meme_id FROM user_reactions WHERE user_id = ? AND reaction = 'dislike'",
        ) && let Ok(rows) = stmt.query_map(params![user_id], |row| row.get::<_, u64>(0))
        {
            for id in rows.flatten() {
                disliked_ids.insert(id);
            }
        }

        let mut candidates: Vec<&Meme> = self
            .memes
            .iter()
            .filter(|m| {
                !m.file_tg_id.is_empty()
                    && m.is_visible_to(user_id, &settings.search_scope)
                    && !disliked_ids.contains(&m.id)
            })
            .collect();

        if candidates.is_empty() {
            return None;
        }

        if let Some(ex) = exclude_meme_id
            && candidates.len() > 1
        {
            candidates.retain(|m| m.id != ex);
        }

        let mut hasher = RandomState::new().build_hasher();
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        hasher.write_u64(nanos);
        let idx = (hasher.finish() as usize) % candidates.len();
        Some(candidates[idx])
    }

    pub fn get_meme_card_info(&self, user_id: i64, meme_id: u64) -> Option<MemeCardInfo> {
        let user_is_public: bool = self
            .conn
            .query_row(
                "SELECT is_public FROM user_memes WHERE user_id = ? AND meme_id = ?",
                params![user_id, meme_id],
                |row| row.get(0),
            )
            .ok()?;

        let meme = self.memes.iter().find(|m| m.id == meme_id)?;

        let mut emoji_map: HashMap<String, String> = HashMap::new();
        if let Ok(mut stmt) = self.conn.prepare("SELECT name, emoji FROM tags")
            && let Ok(rows) = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
        {
            for (name, emoji) in rows.flatten() {
                emoji_map.insert(name, emoji);
            }
        }

        let tags = meme
            .tags
            .iter()
            .map(|slug| {
                let emoji = emoji_map
                    .get(slug)
                    .cloned()
                    .unwrap_or_else(|| tag_emoji(slug).to_string());
                (slug.clone(), emoji)
            })
            .collect();

        Some(MemeCardInfo {
            user_is_public,
            effective_is_public: meme.is_public,
            tags,
        })
    }

    pub fn set_user_meme_visibility(
        &mut self,
        user_id: i64,
        meme_id: u64,
        is_public: bool,
    ) -> Result<Option<MemeCardInfo>, String> {
        let updated = self
            .conn
            .execute(
                "UPDATE user_memes SET is_public = ? WHERE user_id = ? AND meme_id = ?",
                params![is_public, user_id, meme_id],
            )
            .map_err(|e| e.to_string())?;

        if updated == 0 {
            return Ok(None);
        }

        let effective_public: bool = self
            .conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM user_memes WHERE meme_id = ? AND is_public = 1)",
                params![meme_id],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;

        if let Some(meme) = self.memes.iter_mut().find(|m| m.id == meme_id) {
            meme.is_public = effective_public;
        }

        Ok(self.get_meme_card_info(user_id, meme_id))
    }

    pub fn delete_user_meme(
        &mut self,
        user_id: i64,
        meme_id: u64,
    ) -> Result<DeleteMemeOutcome, String> {
        let tx = self.conn.transaction().map_err(|e| e.to_string())?;
        let removed = tx
            .execute(
                "DELETE FROM user_memes WHERE user_id = ? AND meme_id = ?",
                params![user_id, meme_id],
            )
            .map_err(|e| e.to_string())?;

        if removed == 0 {
            return Ok(DeleteMemeOutcome::NotFoundOrNotOwner);
        }

        let remaining: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM user_memes WHERE meme_id = ?",
                params![meme_id],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;

        if remaining == 0 {
            tx.execute("DELETE FROM memes WHERE id = ?", params![meme_id])
                .map_err(|e| e.to_string())?;
            tx.execute(
                "DELETE FROM memes_fts WHERE rowid = ?",
                params![meme_id as i64],
            )
            .map_err(|e| e.to_string())?;
            tx.execute(
                "DELETE FROM meme_tags WHERE meme_id = ?",
                params![meme_id as i64],
            )
            .map_err(|e| e.to_string())?;
            tx.execute(
                "DELETE FROM user_reactions WHERE meme_id = ?",
                params![meme_id],
            )
            .map_err(|e| e.to_string())?;
            tx.execute(
                "DELETE FROM user_delivered_memes WHERE meme_id = ?",
                params![meme_id],
            )
            .map_err(|e| e.to_string())?;

            tx.commit().map_err(|e| e.to_string())?;
            self.memes.retain(|m| m.id != meme_id);
            Ok(DeleteMemeOutcome::DeletedCompletely)
        } else {
            let effective_public: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM user_memes WHERE meme_id = ? AND is_public = 1)",
                    params![meme_id],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;

            tx.commit().map_err(|e| e.to_string())?;
            if let Some(meme) = self.memes.iter_mut().find(|m| m.id == meme_id) {
                meme.owners.retain(|&uid| uid != user_id);
                meme.is_public = effective_public;
            }
            Ok(DeleteMemeOutcome::RemovedFromCollection)
        }
    }

    pub fn is_meme_liked(&self, user_id: i64, meme_id: u64) -> bool {
        self.conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM user_reactions WHERE user_id = ? AND meme_id = ? AND reaction = 'like')",
                params![user_id, meme_id],
                |row| row.get(0),
            )
            .unwrap_or(false)
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
                source_url TEXT NOT NULL DEFAULT '',
                phash BIGINT NOT NULL DEFAULT 0,
                media_type INTEGER NOT NULL DEFAULT 0,
                thumbnail_tg_id TEXT NOT NULL DEFAULT '',
                vlm_version BIGINT NOT NULL DEFAULT 1,
                tags_version INTEGER NOT NULL DEFAULT 0
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

            CREATE VIRTUAL TABLE IF NOT EXISTS memes_fts USING fts5 (
                ocr_norm,
                uk_norm,
                en_norm,
                title_norm
            );

            CREATE TABLE IF NOT EXISTS user_reactions (
                user_id BIGINT NOT NULL,
                meme_id BIGINT NOT NULL,
                reaction TEXT NOT NULL CHECK (reaction IN ('like', 'dislike', 'skip')),
                created_at BIGINT NOT NULL,
                PRIMARY KEY (user_id, meme_id)
            );

            CREATE INDEX IF NOT EXISTS idx_user_reactions_lookup
                ON user_reactions(user_id, reaction);

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

        for def in TAG_TAXONOMY {
            conn.execute(
                "INSERT INTO tags (name, emoji, instruction, is_active)
                 VALUES (?1, ?2, ?3, 1)
                 ON CONFLICT(name) DO UPDATE SET
                     emoji = excluded.emoji,
                     instruction = excluded.instruction",
                params![def.slug, def.emoji, def.instruction],
            )
            .map_err(|e| e.to_string())?;
        }

        conn.execute(
            "INSERT INTO tags (name, emoji, instruction, is_active)
             VALUES (?1, ?2, '', 0)
             ON CONFLICT(name) DO UPDATE SET
                 emoji = excluded.emoji,
                 is_active = 0",
            params![OTHER_TAG_SLUG, OTHER_TAG_EMOJI],
        )
        .map_err(|e| e.to_string())?;

        let mut stmt = conn
                    .prepare("SELECT id, title, ocr_text, embedding_uk, embedding_en, file_tg_id, file_tg_unique_id, source_title, source_url, phash, media_type, thumbnail_tg_id FROM memes")
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
                let phash_raw: i64 = row.get(9)?;
                let phash: u64 = phash_raw as u64;
                let media_type: MediaType = row.get(10).unwrap_or(MediaType::Photo);
                let thumbnail_tg_id: String = row.get(11)?;
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
                    phash,
                    media_type,
                    thumbnail_tg_id,
                    tags: Vec::new(),
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

        let mut tag_stmt = conn
            .prepare(
                "SELECT mt.meme_id, t.name
                 FROM meme_tags mt
                 JOIN tags t ON t.id = mt.tag_id
                 ORDER BY mt.confidence DESC, t.name ASC",
            )
            .map_err(|e| e.to_string())?;
        let tag_iter = tag_stmt
            .query_map([], |row| {
                let meme_id: u64 = row.get(0)?;
                let tag_name: String = row.get(1)?;
                Ok((meme_id, tag_name))
            })
            .map_err(|e| e.to_string())?;
        for (meme_id, tag_name) in tag_iter.flatten() {
            if let Some(mem) = memes_map.get_mut(&meme_id) {
                mem.tags.push(tag_name);
            }
        }
        drop(tag_stmt);

        let mut memes: Vec<Meme> = memes_map.into_values().collect();
        memes.sort_by_key(|m| m.id);

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
        for u in user_iter.flatten() {
            useres.insert(u.user_id, u);
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

    pub fn get_recent_memes(
        &self,
        user_id: i64,
        limit: usize,
        media_filter: Option<MediaType>,
    ) -> Vec<&Meme> {
        let settings = self.get_user_settings(user_id);

        match media_filter {
            Some(filter) => self
                .memes
                .iter()
                .rev()
                .filter(|m| {
                    m.is_visible_to(user_id, &settings.search_scope) && m.media_type == filter
                })
                .take(limit)
                .collect(),
            None => self
                .memes
                .iter()
                .rev()
                .filter(|m| m.is_visible_to(user_id, &settings.search_scope))
                .take(limit)
                .collect(),
        }
    }

    pub fn stage1_candidates_from_embedding(
        &self,
        query: &[f32],
        query_text: &str,
        app_config: &AppConfig,
        user_id: i64,
        scope_visibility: &str,
        filter: SearchFilter<'_>,
        candidate_limit: usize,
    ) -> Vec<Stage1Candidate> {
        if candidate_limit == 0 {
            return Vec::new();
        }

        let fts_hits = self.query_fts_hits(query_text, BM25_MATCH_LIMIT);
        let scored = score_candidates(
            query,
            query_text,
            &self.memes,
            user_id,
            scope_visibility,
            filter,
            &fts_hits,
        );

        let deduplicated =
            deduplicate_by_phash(scored, candidate_limit, app_config.max_phash_distance);
        let ids: Vec<u64> = deduplicated.iter().map(|s| s.meme.id).collect();
        let passages = self.fetch_rerank_passages(&ids);

        deduplicated
            .into_iter()
            .map(|s| {
                let passage = passages
                    .get(&s.meme.id)
                    .cloned()
                    .unwrap_or_else(|| s.meme.ocr_text.clone());
                Stage1Candidate {
                    meme: s.meme.clone(),
                    passage,
                }
            })
            .collect()
    }

    fn fetch_rerank_passages(&self, ids: &[u64]) -> HashMap<u64, String> {
        if ids.is_empty() {
            return HashMap::new();
        }

        let placeholders = vec!["?"; ids.len()].join(", ");
        let sql = format!(
            "SELECT id, ocr_text, description_uk, description_en FROM memes WHERE id IN ({})",
            placeholders
        );

        let mut stmt = match self.conn.prepare(&sql) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "Failed to prepare rerank passage lookup");
                return HashMap::new();
            }
        };

        let rows = match stmt.query_map(rusqlite::params_from_iter(ids.iter()), |row| {
            let id: u64 = row.get(0)?;
            let ocr_text: String = row.get(1)?;
            let desc_uk: String = row.get(2)?;
            let desc_en: String = row.get(3)?;
            let desc = if desc_uk.trim().is_empty() {
                desc_en
            } else {
                desc_uk
            };
            Ok((id, format_embedding_passage(&ocr_text, &desc)))
        }) {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "Failed to fetch rerank passages");
                return HashMap::new();
            }
        };

        let mut passages = HashMap::with_capacity(ids.len());
        for row in rows {
            match row {
                Ok((id, passage)) => {
                    passages.insert(id, passage);
                }
                Err(e) => {
                    warn!(error = %e, "Failed to map rerank passage row");
                }
            }
        }
        passages
    }

    pub fn find_top_k(
        &self,
        query: &[f32],
        query_text: &str,
        app_config: &AppConfig,
        user_id: i64,
        scope_visibility: String,
        filter: SearchFilter<'_>,
    ) -> Vec<(&Meme, f32)> {
        let fts_hits = self.query_fts_hits(query_text, BM25_MATCH_LIMIT);

        find_top_k_impl(
            query,
            query_text,
            &self.memes,
            app_config,
            user_id,
            scope_visibility,
            filter,
            &fts_hits,
        )
    }

    pub fn find_duplicate_by_phash(
        &self,
        target_phash: u64,
        max_distance: u32,
    ) -> Option<(u64, u32)> {
        let mut best_match: Option<(u64, u32)> = None;

        for meme in &self.memes {
            if meme.phash == 0 {
                continue;
            }

            let distance = hamming_distance(meme.phash, target_phash);
            if distance < max_distance {
                match best_match {
                    Some((_, best_dist)) if distance < best_dist => {
                        best_match = Some((meme.id, distance));
                    }
                    None => {
                        best_match = Some((meme.id, distance));
                    }
                    _ => {}
                }
            }
        }

        best_match
    }

    pub fn link_existing_meme(
        &mut self,
        meme_id: u64,
        user_id: i64,
        user_caption: &str,
    ) -> Result<bool, String> {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let is_public = self.get_user_settings(user_id).upload_as_public;
        let mut is_new = false;

        self.conn
            .execute(
                "
            INSERT INTO user_memes (user_id, meme_id, user_captions, is_public, created_at)
            VALUES (?, ?, ?, ?, ?)
            ON CONFLICT(user_id, meme_id) DO UPDATE
            SET
                user_captions = EXCLUDED.user_captions,
                is_public = EXCLUDED.is_public
                ",
                params![user_id, meme_id, user_caption, is_public, now],
            )
            .map_err(|e| e.to_string())?;

        if let Some(meme) = self.memes.iter_mut().find(|m| m.id == meme_id) {
            if !meme.owners.contains(&user_id) {
                meme.owners.push(user_id);
                is_new = true;
            }
            if is_public {
                meme.is_public = true;
            }
        }

        Ok(is_new)
    }

    pub fn get_unhashed_memes(&self) -> Vec<(u64, String, MediaType)> {
        self.memes
            .iter()
            .filter(|m| m.phash == 0)
            .map(|m| (m.id, m.file_tg_id.clone(), m.media_type))
            .collect()
    }

    pub fn update_phash(&mut self, meme_id: u64, phash: u64) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE memes SET phash = ? WHERE id = ?",
                params![phash as i64, meme_id],
            )
            .map_err(|e| e.to_string())?;

        if let Some(meme) = self.memes.iter_mut().find(|m| m.id == meme_id) {
            meme.phash = phash;
        }

        Ok(())
    }

    pub fn get_memes_for_reprocess(
        &self,
        target_vlm_version: i64,
        limit: Option<usize>,
    ) -> Vec<ReprocessCandidate> {
        let lim = limit.unwrap_or(500);

        let Ok(mut stmt) = self.conn.prepare(
            "SELECT id, file_tg_id, media_type, title
                FROM memes
                WHERE vlm_version >= 0 AND vlm_version < ?
                ORDER BY id
                LIMIT ?;",
        ) else {
            return Vec::new();
        };

        let Ok(reprocess_candidate) = stmt.query_map(params![target_vlm_version, lim], |row| {
            Ok(ReprocessCandidate {
                id: row.get(0)?,
                file_tg_id: row.get(1)?,
                media_type: row.get(2).unwrap_or(MediaType::Photo),
                title: row.get(3)?,
            })
        }) else {
            return Vec::new();
        };

        reprocess_candidate.flatten().collect()
    }

    pub fn update_meme_reprocessed(
        &mut self,
        meme_id: u64,
        analysis: &MemeAnalysis,
        embed_uk: Vec<f32>,
        embed_en: Vec<f32>,
        vlm_version: i64,
        title: &str,
    ) -> Result<(), String> {
        let blob_uk = embedding_to_blob(&embed_uk);
        let blob_en = embedding_to_blob(&embed_en);

        self.conn
            .execute(
                "UPDATE memes
                SET ocr_text = ?,
                    description_uk = ?,
                    description_en = ?,
                    embedding_uk = ?,
                    embedding_en = ?,
                    vlm_version = ?
                WHERE id = ?",
                params![
                    analysis.ocr_text,
                    analysis.uk_description,
                    analysis.en_description,
                    blob_uk,
                    blob_en,
                    vlm_version,
                    meme_id
                ],
            )
            .map_err(|e| e.to_string())?;

        if let Some(meme) = self.memes.iter_mut().find(|m| m.id == meme_id) {
            meme.ocr_text = analysis.ocr_text.clone();
            meme.embedding_uk = embed_uk;
            meme.embedding_en = embed_en
        }

        upsert_fts_entry(
            &self.conn,
            MemesFts::from_raw(
                meme_id,
                &analysis.ocr_text,
                &analysis.uk_description,
                &analysis.en_description,
                title,
            ),
        )
        .map_err(|e| e.to_string())?;

        Ok(())
    }

    pub fn mark_meme_vlm_version(&mut self, meme_id: u64, vlm_version: i64) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE memes
                SET vlm_version = ?
                WHERE id = ?",
                params![vlm_version, meme_id],
            )
            .map_err(|e| e.to_string())?;

        Ok(())
    }

    pub fn create_pre_process_backup(&self, backup_path: String) -> Result<(), String> {
        if !Path::new(&backup_path).exists() {
            self.conn
                .execute("VACUUM INTO ?", params![backup_path])
                .map_err(|e| e.to_string())?;
        }

        Ok(())
    }

    fn query_fts_hits(&self, query_text: &str, limit: usize) -> HashMap<u64, FtsHit> {
        let query_terms = lexical_helper::extract_stemmed_terms(query_text);

        match lexical_helper::build_fts_match_query(query_text) {
            Some(match_expresion) => {
                let mut stmt = match self.conn.prepare(
                    "
                    SELECT rowid,
                           bm25(memes_fts, 2.0, 1.0, 0.6, 0.8) as raw_bm25,
                           ocr_norm,
                           uk_norm,
                           en_norm,
                           title_norm
                    FROM memes_fts
                    WHERE memes_fts MATCH ?
                    ORDER BY raw_bm25 ASC
                    LIMIT ?;",
                ) {
                    Ok(s) => s,
                    Err(e) => {
                        error!(error = %e, "Failed to prepare FTS query");
                        return HashMap::new();
                    }
                };

                let match_rows = match stmt.query_map(params![match_expresion, limit], |row| {
                    let raw_bm25: f32 = row.get(1)?;
                    Ok(Fts35 {
                        rowid: row.get(0)?,
                        ocr_norm: row.get(2)?,
                        uk_norm: row.get(3)?,
                        en_norm: row.get(4)?,
                        title_norm: row.get(5)?,
                        bm25_score: (-raw_bm25 as f32).max(0.0),
                    })
                }) {
                    Ok(r) => r,
                    Err(e) => {
                        error!(error = %e, "Failed to query FTS index");
                        return HashMap::new();
                    }
                };

                let mut result: HashMap<u64, FtsHit> = HashMap::new();
                for matched_result in match_rows {
                    match matched_result {
                        Ok(matched) => {
                            let doc_tokens: HashSet<&str> = matched
                                .ocr_norm
                                .split_whitespace()
                                .chain(matched.uk_norm.split_whitespace())
                                .chain(matched.en_norm.split_whitespace())
                                .chain(matched.title_norm.split_whitespace())
                                .collect();
                            let matched_terms = query_terms
                                .iter()
                                .filter(|q| doc_tokens.contains(q.as_str()))
                                .count();

                            result.insert(
                                matched.rowid,
                                FtsHit {
                                    bm25_score: matched.bm25_score,
                                    matched_terms,
                                },
                            );
                        }
                        Err(e) => warn!(error = %e, "Failed to map FTS row"),
                    }
                }
                drop(stmt);

                return result;
            }
            None => return HashMap::new(),
        };
    }

    pub fn record_reaction(
        &self,
        user_id: i64,
        meme_id: u64,
        reaction: &str,
    ) -> Result<(), String> {
        if !matches!(reaction, "like" | "dislike" | "skip") {
            return Err(format!("Unsupported reaction: {}", reaction));
        }

        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        self.conn
            .execute(
                "INSERT INTO user_reactions (user_id, meme_id, reaction, created_at)
                 VALUES (?, ?, ?, ?)
                 ON CONFLICT(user_id, meme_id) DO UPDATE SET
                     reaction = excluded.reaction,
                     created_at = excluded.created_at",
                params![user_id, meme_id, reaction, now],
            )
            .map_err(|e| e.to_string())?;

        Ok(())
    }

    pub fn remove_reaction(&self, user_id: i64, meme_id: u64) -> Result<(), String> {
        self.conn
            .execute(
                "DELETE FROM user_reactions WHERE user_id = ? AND meme_id = ?",
                params![user_id, meme_id],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Counts a user's Tinder reactions grouped by kind (`like`, `dislike`, `skip`).
    pub fn get_user_reaction_counts(&self, user_id: i64) -> UserReactionCounts {
        let mut counts = UserReactionCounts::default();
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT reaction, COUNT(*)
             FROM user_reactions
             WHERE user_id = ?
             GROUP BY reaction",
        ) else {
            return counts;
        };

        let Ok(rows) = stmt.query_map(params![user_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        }) else {
            return counts;
        };

        for (reaction, count) in rows.flatten() {
            let count = count.max(0) as usize;
            match reaction.as_str() {
                "like" => counts.likes = count,
                "dislike" => counts.dislikes = count,
                "skip" => counts.skips = count,
                _ => {}
            }
        }

        counts
    }

    /// Deletes a user's reactions for the specified kind (`Some("like" | "dislike" | "skip")`)
    /// or all reactions when `reaction` is `None`. Returns the number of deleted rows.
    pub fn reset_user_reactions(
        &self,
        user_id: i64,
        reaction: Option<&str>,
    ) -> Result<usize, String> {
        match reaction {
            Some(kind) => {
                if !matches!(kind, "like" | "dislike" | "skip") {
                    return Err(format!("Invalid reaction type: {}", kind));
                }
                self.conn
                    .execute(
                        "DELETE FROM user_reactions WHERE user_id = ? AND reaction = ?",
                        params![user_id, kind],
                    )
                    .map_err(|e| e.to_string())
            }
            None => self
                .conn
                .execute(
                    "DELETE FROM user_reactions WHERE user_id = ?",
                    params![user_id],
                )
                .map_err(|e| e.to_string()),
        }
    }

    pub fn reset_user_skips(&self, user_id: i64) -> Result<usize, String> {
        self.reset_user_reactions(user_id, Some("skip"))
    }

    pub fn count_user_skips(&self, user_id: i64) -> usize {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM user_reactions WHERE user_id = ? AND reaction = 'skip'",
                params![user_id],
                |row| row.get::<_, i64>(0),
            )
            .map(|c| c.max(0) as usize)
            .unwrap_or(0)
    }

    pub fn count_user_likes(&self, user_id: i64) -> usize {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM user_reactions WHERE user_id = ? AND reaction = 'like'",
                params![user_id],
                |row| row.get::<_, i64>(0),
            )
            .map(|c| c.max(0) as usize)
            .unwrap_or(0)
    }

    pub fn get_next_tinder_meme(&self, user_id: i64) -> Option<(Meme, usize)> {
        let mut reacted_ids: HashSet<u64> = HashSet::new();
        if let Ok(mut stmt) = self
            .conn
            .prepare("SELECT meme_id FROM user_reactions WHERE user_id = ?")
            && let Ok(rows) = stmt.query_map(params![user_id], |row| row.get::<_, u64>(0))
        {
            for id in rows.flatten() {
                reacted_ids.insert(id);
            }
        }

        let candidates: Vec<&Meme> = self
            .memes
            .iter()
            .filter(|m| {
                m.is_visible_to(user_id, "all")
                    && !m.file_tg_id.is_empty()
                    && !reacted_ids.contains(&m.id)
            })
            .collect();

        let remaining = candidates.len();
        if remaining == 0 {
            return None;
        }

        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos() as usize;
        let idx = nanos.wrapping_mul(2_654_435_761) % remaining;
        Some((candidates[idx].clone(), remaining))
    }

    pub fn get_liked_memes(&self, user_id: i64) -> Vec<Meme> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT meme_id
             FROM user_reactions
             WHERE user_id = ? AND reaction = 'like'
             ORDER BY created_at DESC",
        ) else {
            return Vec::new();
        };

        let Ok(rows) = stmt.query_map(params![user_id], |row| row.get::<_, u64>(0)) else {
            return Vec::new();
        };

        let meme_by_id: HashMap<u64, &Meme> = self
            .memes
            .iter()
            .filter(|m| !m.file_tg_id.is_empty())
            .map(|m| (m.id, m))
            .collect();

        rows.flatten()
            .filter_map(|id| meme_by_id.get(&id).map(|m| (*m).clone()))
            .collect()
    }

    pub fn get_drip_subscription(&self, user_id: i64) -> Option<DripSubscription> {
        self.conn
            .query_row(
                "SELECT user_id, chat_id, memes_per_day, tz_offset_hours, quiet_start_hour, quiet_end_hour, next_delivery_at, is_active
                 FROM meme_subscriptions
                 WHERE user_id = ?",
                params![user_id],
                map_drip_subscription_row,
            )
            .ok()
    }

    pub fn upsert_drip_subscription(&self, sub: &DripSubscription) -> Result<(), String> {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.conn
            .execute(
                "INSERT INTO meme_subscriptions (
                     user_id, chat_id, memes_per_day, tz_offset_hours,
                     quiet_start_hour, quiet_end_hour, next_delivery_at, is_active, created_at
                 )
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT(user_id) DO UPDATE SET
                     chat_id = excluded.chat_id,
                     memes_per_day = excluded.memes_per_day,
                     tz_offset_hours = excluded.tz_offset_hours,
                     quiet_start_hour = excluded.quiet_start_hour,
                     quiet_end_hour = excluded.quiet_end_hour,
                     next_delivery_at = excluded.next_delivery_at,
                     is_active = excluded.is_active",
                params![
                    sub.user_id,
                    sub.chat_id,
                    sub.memes_per_day,
                    sub.tz_offset_hours,
                    sub.quiet_start_hour,
                    sub.quiet_end_hour,
                    sub.next_delivery_at as i64,
                    sub.is_active,
                    now as i64,
                ],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn set_drip_active(&self, user_id: i64, is_active: bool) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE meme_subscriptions SET is_active = ? WHERE user_id = ?",
                params![is_active, user_id],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn update_drip_next_delivery(
        &self,
        user_id: i64,
        next_delivery_at: u64,
    ) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE meme_subscriptions SET next_delivery_at = ? WHERE user_id = ?",
                params![next_delivery_at as i64, user_id],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn get_due_drip_subscriptions(&self, now_secs: u64) -> Vec<DripSubscription> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT user_id, chat_id, memes_per_day, tz_offset_hours, quiet_start_hour, quiet_end_hour, next_delivery_at, is_active
             FROM meme_subscriptions
             WHERE is_active = 1 AND next_delivery_at <= ?",
        ) else {
            return Vec::new();
        };

        let Ok(rows) = stmt.query_map(params![now_secs as i64], map_drip_subscription_row) else {
            return Vec::new();
        };

        rows.flatten().collect()
    }

    pub fn count_user_delivered_memes(&self, user_id: i64) -> usize {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM user_delivered_memes WHERE user_id = ?",
                params![user_id],
                |row| row.get::<_, i64>(0),
            )
            .map(|c| c.max(0) as usize)
            .unwrap_or(0)
    }

    pub fn reset_user_delivered_memes(&self, user_id: i64) -> Result<usize, String> {
        self.conn
            .execute(
                "DELETE FROM user_delivered_memes WHERE user_id = ?",
                params![user_id],
            )
            .map_err(|e| e.to_string())
    }

    pub fn record_delivered_meme(
        &self,
        user_id: i64,
        meme_id: u64,
        delivered_at: u64,
    ) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO user_delivered_memes (user_id, meme_id, delivered_at)
                 VALUES (?, ?, ?)",
                params![user_id, meme_id, delivered_at as i64],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn pick_next_drip_meme(&self, user_id: i64) -> Option<Meme> {
        let mut excluded_ids: HashSet<u64> = HashSet::new();
        for sql in [
            "SELECT meme_id FROM user_delivered_memes WHERE user_id = ?",
            "SELECT meme_id FROM user_reactions WHERE user_id = ? AND reaction = 'dislike'",
        ] {
            if let Ok(mut stmt) = self.conn.prepare(sql)
                && let Ok(rows) = stmt.query_map(params![user_id], |row| row.get::<_, u64>(0))
            {
                for id in rows.flatten() {
                    excluded_ids.insert(id);
                }
            }
        }

        let candidates: Vec<&Meme> = self
            .memes
            .iter()
            .filter(|m| {
                m.is_visible_to(user_id, "all")
                    && !m.file_tg_id.is_empty()
                    && !excluded_ids.contains(&m.id)
            })
            .collect();

        if candidates.is_empty() {
            return None;
        }

        let mut hasher = RandomState::new().build_hasher();
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        hasher.write_u64(nanos);
        let idx = (hasher.finish() as usize) % candidates.len();
        Some(candidates[idx].clone())
    }

    pub fn set_meme_tags(
        &mut self,
        meme_id: u64,
        tags: &[(String, f32)],
        tags_version: i64,
    ) -> Result<(), String> {
        let tx = self.conn.transaction().map_err(|e| e.to_string())?;
        tx.execute(
            "DELETE FROM meme_tags WHERE meme_id = ?",
            params![meme_id as i64],
        )
        .map_err(|e| e.to_string())?;

        for (tag_name, confidence) in tags {
            tx.execute(
                "INSERT INTO tags (name) VALUES (?) ON CONFLICT(name) DO NOTHING",
                params![tag_name],
            )
            .map_err(|e| e.to_string())?;

            let tag_id: i64 = tx
                .query_row(
                    "SELECT id FROM tags WHERE name = ?",
                    params![tag_name],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;

            tx.execute(
                "INSERT INTO meme_tags (meme_id, tag_id, confidence) VALUES (?, ?, ?)",
                params![meme_id as i64, tag_id, *confidence as f64],
            )
            .map_err(|e| e.to_string())?;
        }

        tx.execute(
            "UPDATE memes SET tags_version = ? WHERE id = ?",
            params![tags_version, meme_id as i64],
        )
        .map_err(|e| e.to_string())?;

        tx.commit().map_err(|e| e.to_string())?;

        if let Some(meme) = self.memes.iter_mut().find(|m| m.id == meme_id) {
            meme.tags = tags.iter().map(|(name, _)| name.clone()).collect();
        }

        Ok(())
    }

    pub fn get_memes_for_tagging(&self, target_version: i64, limit: usize) -> Vec<TagCandidate> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT id, title, ocr_text, description_uk, description_en
             FROM memes
             WHERE tags_version < ? AND tags_version >= 0
             ORDER BY id DESC
             LIMIT ?",
        ) else {
            return Vec::new();
        };

        let Ok(rows) = stmt.query_map(params![target_version, limit as i64], |row| {
            Ok(TagCandidate {
                id: row.get(0)?,
                title: row.get(1)?,
                ocr_text: row.get(2)?,
                description_uk: row.get(3)?,
                description_en: row.get(4)?,
            })
        }) else {
            return Vec::new();
        };

        rows.flatten().collect()
    }

    pub fn get_tagging_progress(&self, target_version: i64) -> (usize, usize) {
        let tagged: usize = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM memes WHERE tags_version >= ? OR tags_version < 0",
                params![target_version],
                |row| row.get::<_, i64>(0),
            )
            .map(|c| c.max(0) as usize)
            .unwrap_or(0);

        let total: usize = self
            .conn
            .query_row("SELECT COUNT(*) FROM memes", [], |row| row.get::<_, i64>(0))
            .map(|c| c.max(0) as usize)
            .unwrap_or(0);

        (tagged, total)
    }

    pub fn get_tag_counts(&self, user_id: i64) -> Vec<(String, String, usize)> {
        let settings = self.get_user_settings(user_id);
        let mut counts: HashMap<String, usize> = HashMap::new();

        for meme in &self.memes {
            if meme.file_tg_id.is_empty()
                || !meme.is_visible_to(user_id, &settings.search_scope)
            {
                continue;
            }
            for tag in &meme.tags {
                *counts.entry(tag.clone()).or_insert(0) += 1;
            }
        }

        let mut emoji_map: HashMap<String, String> = HashMap::new();
        if let Ok(mut stmt) = self.conn.prepare("SELECT name, emoji FROM tags")
            && let Ok(rows) = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
        {
            for (name, emoji) in rows.flatten() {
                emoji_map.insert(name, emoji);
            }
        }

        let mut list: Vec<(String, String, usize)> = counts
            .into_iter()
            .map(|(name, count)| {
                let emoji = emoji_map
                    .get(&name)
                    .cloned()
                    .unwrap_or_else(|| tag_emoji(&name).to_string());
                (name, emoji, count)
            })
            .collect();

        list.sort_by(|a, b| {
            (a.0 == OTHER_TAG_SLUG)
                .cmp(&(b.0 == OTHER_TAG_SLUG))
                .then_with(|| b.2.cmp(&a.2))
                .then_with(|| a.0.cmp(&b.0))
        });
        list
    }

    pub fn get_memes_by_tags(
        &self,
        user_id: i64,
        tags: &[String],
        limit: usize,
        media_filter: Option<MediaType>,
    ) -> Vec<&Meme> {
        let settings = self.get_user_settings(user_id);

        self.memes
            .iter()
            .rev()
            .filter(|m| {
                !m.file_tg_id.is_empty()
                    && m.is_visible_to(user_id, &settings.search_scope)
                    && tags.iter().all(|tag| m.has_tag(tag))
                    && media_filter.is_none_or(|f| m.media_type == f)
            })
            .take(limit)
            .collect()
    }

    pub fn get_active_taxonomy(&self) -> Vec<DynamicTagDef> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT name, emoji, instruction
             FROM tags
             WHERE is_active = 1 AND name != ?
             ORDER BY id ASC",
        ) else {
            return crate::tagger::default_taxonomy();
        };

        let Ok(rows) = stmt.query_map(params![OTHER_TAG_SLUG], |row| {
            Ok(DynamicTagDef {
                slug: row.get(0)?,
                emoji: row.get(1)?,
                instruction: row.get(2)?,
            })
        }) else {
            return crate::tagger::default_taxonomy();
        };

        let defs: Vec<DynamicTagDef> = rows
            .flatten()
            .filter(|d| !d.instruction.trim().is_empty())
            .collect();

        if defs.is_empty() {
            crate::tagger::default_taxonomy()
        } else {
            defs
        }
    }

    pub fn add_dynamic_category(&mut self, def: &DynamicTagDef) -> Result<bool, String> {
        let inserted = self
            .conn
            .execute(
                "INSERT INTO tags (name, emoji, instruction, is_active)
                 VALUES (?1, ?2, ?3, 1)
                 ON CONFLICT(name) DO NOTHING",
                params![def.slug, def.emoji, def.instruction],
            )
            .map_err(|e| e.to_string())?;
        Ok(inserted > 0)
    }

    pub fn get_undetermined_memes_for_discovery(&self, limit: usize) -> Vec<DiscoverySample> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT id, ocr_text, description_uk, description_en, title
             FROM memes
             WHERE tags_version = ?
             ORDER BY id DESC
             LIMIT ?",
        ) else {
            return Vec::new();
        };

        let Ok(rows) = stmt.query_map(params![TAGS_STATE_UNDETERMINED, limit as i64], |row| {
            Ok(DiscoverySample {
                id: row.get(0)?,
                ocr_text: row.get(1)?,
                description_uk: row.get(2)?,
                description_en: row.get(3)?,
                title: row.get(4)?,
            })
        }) else {
            return Vec::new();
        };

        rows.flatten().collect()
    }

    pub fn mark_memes_tags_version(
        &mut self,
        meme_ids: &[u64],
        tags_version: i64,
    ) -> Result<(), String> {
        if meme_ids.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction().map_err(|e| e.to_string())?;
        for meme_id in meme_ids {
            tx.execute(
                "UPDATE memes SET tags_version = ? WHERE id = ?",
                params![tags_version, *meme_id as i64],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn load_vlm_quotas(&self) -> Vec<PersistedModelQuota> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT model_id, pt_day_key, used_today, cooldown_until, last_reason
             FROM vlm_model_quota",
        ) else {
            return Vec::new();
        };

        let Ok(rows) = stmt.query_map([], |row| {
            Ok(PersistedModelQuota {
                model_id: row.get(0)?,
                pt_day_key: row.get(1)?,
                used_today: row.get::<_, i64>(2)?.max(0) as u32,
                cooldown_until: row.get(3)?,
                last_reason: row.get(4)?,
            })
        }) else {
            return Vec::new();
        };

        rows.flatten().collect()
    }

    pub fn upsert_vlm_quota(&self, q: &PersistedModelQuota) -> Result<(), String> {
        let updated_at = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        self.conn
            .execute(
                "INSERT INTO vlm_model_quota (
                    model_id, pt_day_key, used_today, cooldown_until, last_reason, updated_at
                 )
                 VALUES (?, ?, ?, ?, ?, ?)
                 ON CONFLICT(model_id) DO UPDATE SET
                    pt_day_key = excluded.pt_day_key,
                    used_today = excluded.used_today,
                    cooldown_until = excluded.cooldown_until,
                    last_reason = excluded.last_reason,
                    updated_at = excluded.updated_at",
                params![
                    q.model_id,
                    q.pt_day_key,
                    i64::from(q.used_today),
                    q.cooldown_until,
                    q.last_reason,
                    updated_at
                ],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

fn map_drip_subscription_row(row: &rusqlite::Row) -> rusqlite::Result<DripSubscription> {
    Ok(DripSubscription {
        user_id: row.get(0)?,
        chat_id: row.get(1)?,
        memes_per_day: row.get::<_, i64>(2)?.clamp(1, 255) as u8,
        tz_offset_hours: row.get::<_, i64>(3)?.clamp(-12, 14) as i8,
        quiet_start_hour: row.get::<_, i64>(4)?.clamp(0, 23) as u8,
        quiet_end_hour: row.get::<_, i64>(5)?.clamp(0, 23) as u8,
        next_delivery_at: row.get::<_, i64>(6)?.max(0) as u64,
        is_active: row.get(7)?,
    })
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> Result<f32, String> {
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

fn score_candidates<'a>(
    query: &[f32],
    query_text: &str,
    memes: &'a [Meme],
    user_id: i64,
    scope_visible: &str,
    filter: SearchFilter<'_>,
    fts_hits: &HashMap<u64, FtsHit>,
) -> Vec<ScoredMeme<'a>> {
    let mut selected_memes: Vec<ScoredMeme<'a>> = Vec::with_capacity(memes.len());

    let query_terms = lexical_helper::extract_stemmed_terms(query_text);
    for meme in memes {
        let mut has_lexical_match = false;
        if !meme.is_visible_to(user_id, scope_visible) {
            continue;
        };

        if let Some(media) = filter.media
            && meme.media_type != media
        {
            continue;
        }

        if !filter.tags.is_empty()
            && !filter.tags.iter().all(|tag| meme.has_tag(tag))
        {
            continue;
        }

        let sim_uk = cosine_similarity(query, &meme.embedding_uk).unwrap_or(0.0);
        let sim_en = cosine_similarity(query, &meme.embedding_en).unwrap_or(0.0);
        let mut best_score = f32::max(sim_uk, sim_en);

        let fts_hit = fts_hits.get(&meme.id);
        if !query_terms.is_empty()
            && let Some(fts) = fts_hit
            && fts.matched_terms > 0
        {
            let coverage = fts.matched_terms as f32 / query_terms.len() as f32;
            let norm_bm25 = fts.bm25_score / (fts.bm25_score + 10.0);
            let boost = 0.04 * coverage + 0.02 * norm_bm25;
            best_score = (best_score + boost).min(1.0);
            has_lexical_match = if query_terms.len() >= 3 {
                fts.matched_terms >= 2
            } else {
                fts.matched_terms >= 1
            };
        }
        if best_score > 0.5 || fts_hit.is_some() {
            selected_memes.push(ScoredMeme {
                meme,
                score: best_score,
                has_lexical_match,
            });
        }
    }
    selected_memes.sort_by(|b, a| a.score.total_cmp(&b.score));
    selected_memes
}

#[allow(clippy::too_many_arguments)]
fn find_top_k_impl<'a>(
    query: &[f32],
    query_text: &str,
    memes: &'a [Meme],
    app_config: &AppConfig,
    user_id: i64,
    scope_visible: String,
    filter: SearchFilter<'_>,
    fts_hits: &HashMap<u64, FtsHit>,
) -> Vec<(&'a Meme, f32)> {
    let selected_memes = score_candidates(
        query,
        query_text,
        memes,
        user_id,
        &scope_visible,
        filter,
        fts_hits,
    );

    let weak_filtered = weak_results(selected_memes);

    let deduplicated = deduplicate_by_phash(
        weak_filtered,
        app_config.top_k,
        app_config.max_phash_distance,
    );

    select_relative(deduplicated)
        .into_iter()
        .map(|f| (f.meme, f.score))
        .collect()
}

fn deduplicate_by_phash<'a>(
    sorted_memes: Vec<ScoredMeme<'a>>,
    limit: usize,
    max_distance: u32,
) -> Vec<ScoredMeme<'a>> {
    let mut selected_memes: Vec<ScoredMeme<'a>> = Vec::with_capacity(limit);
    let mut seen_phash: Vec<u64> = Vec::with_capacity(limit);

    for scored_meme in sorted_memes {
        if scored_meme.meme.phash != 0
            && seen_phash
                .iter()
                .any(|&h| hamming_distance(h, scored_meme.meme.phash) <= max_distance)
        {
            continue;
        }

        if scored_meme.meme.phash != 0 {
            seen_phash.push(scored_meme.meme.phash);
        }

        selected_memes.push(scored_meme);

        if selected_memes.len() == limit {
            break;
        }
    }

    selected_memes
}

fn select_relative<'a>(candidates: Vec<ScoredMeme<'a>>) -> Vec<ScoredMeme<'a>> {
    if candidates.is_empty() {
        return Vec::new();
    }

    let mut selected_memes: Vec<ScoredMeme<'a>> = Vec::new();
    let top1_score = candidates[0].score;
    selected_memes.push(candidates[0].clone());

    for item in candidates.iter().skip(1) {
        if item.has_lexical_match || top1_score - item.score <= 0.032 {
            selected_memes.push(item.clone());
        }
    }

    selected_memes
}

fn weak_results<'a>(candidates: Vec<ScoredMeme<'a>>) -> Vec<ScoredMeme<'a>> {
    if candidates.is_empty() {
        return Vec::new();
    }

    if candidates.len() >= 20 {
        let mut top_three = false;
        if let Some(chunk) = candidates.first_chunk::<3>() {
            top_three = chunk.iter().any(|f| f.has_lexical_match)
        }

        if !top_three
            && candidates[0].score < 0.82
            && candidates[0].score - candidates[19].score <= 0.02
        {
            return Vec::new();
        }
    }

    candidates
}

#[derive(Clone, Debug)]
struct ScoredMeme<'a> {
    meme: &'a Meme,
    score: f32,
    has_lexical_match: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_meme(id: u64, phash: u64, ocr_text: &str, embedding_uk: Vec<f32>) -> Meme {
        Meme {
            id,
            title: String::new(),
            ocr_text: ocr_text.to_string(),
            embedding_uk,
            embedding_en: vec![0.0, 1.0],
            file_tg_id: format!("tg_{}", id),
            file_tg_unique_id: format!("uniq_{}", id),
            source_title: String::new(),
            source_url: String::new(),
            is_public: true,
            owners: vec![1],
            phash,
            media_type: MediaType::Photo,
            thumbnail_tg_id: String::new(),
            tags: Vec::new(),
        }
    }

    fn make_scored<'a>(meme: &'a Meme, score: f32, has_lexical_match: bool) -> ScoredMeme<'a> {
        ScoredMeme {
            meme,
            score,
            has_lexical_match,
        }
    }

    #[test]
    fn deduplicate_by_phash_skips_near_clones_and_keeps_top_score() {
        let m1 = make_test_meme(1, 0b1111_1111, "", vec![1.0, 0.0]);
        let m2 = make_test_meme(2, 0b1111_1100, "", vec![1.0, 0.0]); // hamming = 2 <= 5
        let m3 = make_test_meme(3, 0xFFFF_0000_0000_0000, "", vec![1.0, 0.0]);
        let m4 = make_test_meme(4, 0x0000_FFFF_0000_0000, "", vec![1.0, 0.0]);

        let candidates = vec![
            make_scored(&m1, 0.90, false),
            make_scored(&m2, 0.88, false),
            make_scored(&m3, 0.85, false),
            make_scored(&m4, 0.82, false),
        ];
        let selected = deduplicate_by_phash(candidates, 3, 5);
        let ids: Vec<u64> = selected.into_iter().map(|s| s.meme.id).collect();

        assert_eq!(ids, vec![1, 3, 4]);
    }

    #[test]
    fn deduplicate_by_phash_preserves_unhashed_memes() {
        let m1 = make_test_meme(10, 0, "", vec![1.0, 0.0]);
        let m2 = make_test_meme(20, 0, "", vec![1.0, 0.0]);

        let candidates = vec![make_scored(&m1, 0.90, false), make_scored(&m2, 0.85, false)];
        let selected = deduplicate_by_phash(candidates, 3, 5);
        let ids: Vec<u64> = selected.into_iter().map(|s| s.meme.id).collect();

        assert_eq!(ids, vec![10, 20]);
    }

    #[test]
    fn deduplicate_by_phash_keeps_memes_above_distance_threshold() {
        let m1 = make_test_meme(1, 0b0000_0000, "", vec![1.0, 0.0]);
        let m2 = make_test_meme(2, 0b0011_1111, "", vec![1.0, 0.0]); // hamming = 6 > 5

        let candidates = vec![make_scored(&m1, 0.90, false), make_scored(&m2, 0.85, false)];
        let selected = deduplicate_by_phash(candidates, 3, 5);
        let ids: Vec<u64> = selected.into_iter().map(|s| s.meme.id).collect();

        assert_eq!(ids, vec![1, 2]);
    }

    #[test]
    fn select_relative_filters_tail_beyond_gap_unless_lexically_matched() {
        let m1 = make_test_meme(1, 1, "", vec![1.0, 0.0]);
        let m2 = make_test_meme(2, 2, "", vec![1.0, 0.0]);
        let m3 = make_test_meme(3, 3, "", vec![1.0, 0.0]);
        let m4 = make_test_meme(4, 4, "", vec![1.0, 0.0]);

        let candidates = vec![
            make_scored(&m1, 0.88, false),
            make_scored(&m2, 0.86, false), // diff 0.02 <= 0.032 -> keep
            make_scored(&m3, 0.82, false), // diff 0.06 > 0.032, no lex -> drop
            make_scored(&m4, 0.82, true),  // diff 0.06 > 0.032, has lex -> keep
        ];

        let kept = select_relative(candidates);
        let ids: Vec<u64> = kept.into_iter().map(|s| s.meme.id).collect();
        assert_eq!(ids, vec![1, 2, 4]);
    }

    #[test]
    fn weak_results_rejects_flat_weak_results_and_keeps_lex_confirmed() {
        let memes: Vec<Meme> = (1..=20)
            .map(|id| make_test_meme(id, id, "", vec![1.0, 0.0]))
            .collect();

        let flat_noise: Vec<ScoredMeme<'_>> = memes
            .iter()
            .enumerate()
            .map(|(i, m)| make_scored(m, 0.81 - (i as f32) * 0.0005, false))
            .collect();
        assert!(weak_results(flat_noise).is_empty());

        let with_lex: Vec<ScoredMeme<'_>> = memes
            .iter()
            .enumerate()
            .map(|(i, m)| make_scored(m, 0.81 - (i as f32) * 0.0005, i == 0))
            .collect();
        assert_eq!(weak_results(with_lex).len(), 20);
    }

    fn make_test_config(top_k: usize) -> AppConfig {
        AppConfig {
            db_path: String::new(),
            top_k,
            gemini_api_key: String::new(),
            max_phash_distance: 5,
            auto_reprocess: false,
            enable_reranker: false,
            rerank_inline: false,
            rerank_min_score: 0.15,
            rerank_candidates: 10,
        }
    }

    #[test]
    fn find_top_k_deduplicates_clones_and_avoids_false_substring_boost() {
        let cfg = make_test_config(2);

        let clone_a = make_test_meme(
            1,
            0xAAAA_AAAA_AAAA_AAAA,
            "шоколадки на касах",
            vec![0.9, 0.1],
        );
        let clone_b = make_test_meme(
            2,
            0xAAAA_AAAA_AAAA_AAAB,
            "шоколадки на касах",
            vec![0.89, 0.11],
        );

        let svinabony = make_test_meme(3, 0x1111_2222_3333_4444, "Свінабони", vec![0.88, 0.12]);

        let memes = vec![clone_a, clone_b, svinabony];
        let query_vec = vec![0.9, 0.1];
        let mut fts_hits = HashMap::new();
        fts_hits.insert(
            1,
            FtsHit {
                bm25_score: 8.5,
                matched_terms: 2,
            },
        );
        fts_hits.insert(
            2,
            FtsHit {
                bm25_score: 8.5,
                matched_terms: 2,
            },
        );

        let results = find_top_k_impl(
            &query_vec,
            "шоколадки на касі",
            &memes,
            &cfg,
            1,
            "all".to_string(),
            SearchFilter::default(),
            &fts_hits,
        );

        let ids: Vec<u64> = results.iter().map(|(m, _)| m.id).collect();
        assert_eq!(ids, vec![1, 3]);
        assert!(
            (results[1].1 - cosine_similarity(&query_vec, &[0.88, 0.12]).unwrap()).abs() < 1e-5
        );
    }

    #[test]
    fn find_top_k_drops_distant_unmatched_tail() {
        let cfg = make_test_config(3);

        let exact = make_test_meme(1, 0x1000, "шоколадки", vec![1.0, 0.0]);
        let noise = make_test_meme(2, 0x2000, "випадковий текст", vec![0.8, 0.6]); // cos = 0.80, gap = 0.20 > 0.032

        let memes = vec![exact, noise];
        let query_vec = vec![1.0, 0.0];
        let mut fts_hits = HashMap::new();
        fts_hits.insert(
            1,
            FtsHit {
                bm25_score: 6.0,
                matched_terms: 1,
            },
        );

        let results = find_top_k_impl(
            &query_vec,
            "шоколадки",
            &memes,
            &cfg,
            1,
            "all".to_string(),
            SearchFilter::default(),
            &fts_hits,
        );

        let ids: Vec<u64> = results.iter().map(|(m, _)| m.id).collect();
        assert_eq!(ids, vec![1]);
    }

    fn seed_memory_meme(
        store: &mut MemeStore,
        id: u64,
        ocr: &str,
        desc_uk: &str,
        desc_en: &str,
        title: &str,
        embedding_uk: Vec<f32>,
    ) {
        let blob_uk = embedding_to_blob(&embedding_uk);
        let blob_en = embedding_to_blob(&[0.0, 1.0]);
        store
            .conn
            .execute(
                "INSERT INTO memes (
                    id, title, ocr_text, description_uk, description_en,
                    embedding_uk, embedding_en, file_tg_id, file_tg_unique_id,
                    source_title, source_url, phash, media_type, thumbnail_tg_id, vlm_version
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, '', '', ?, 0, '', 2)",
                params![
                    id,
                    title,
                    ocr,
                    desc_uk,
                    desc_en,
                    blob_uk,
                    blob_en,
                    format!("tg_{}", id),
                    format!("uniq_{}", id),
                    id as i64
                ],
            )
            .unwrap();

        upsert_fts_entry(
            &store.conn,
            MemesFts::from_raw(id, ocr, desc_uk, desc_en, title),
        )
        .unwrap();

        let mut meme = make_test_meme(
            id,
            id.wrapping_mul(0x0101_0101_0101_0101),
            ocr,
            embedding_uk,
        );
        meme.title = title.to_string();
        store.memes.push(meme);
    }

    #[test]
    fn fts_search_matches_description_uk_and_en_when_ocr_is_empty() {
        let mut store = MemeStore::open(":memory:").unwrap();
        let cfg = make_test_config(3);

        seed_memory_meme(
            &mut store,
            1,
            "",
            "Кіт у паніці дивиться на монітор через дедлайн",
            "Panicked cat staring at monitor",
            "Раночку @nepokazuy_mami",
            vec![0.9, 0.436],
        );
        seed_memory_meme(
            &mut store,
            2,
            "",
            "Пес спить на дивані ввечері",
            "Dog sleeping on couch",
            "",
            vec![0.91, 0.415],
        );

        let query_vec = vec![1.0, 0.0];
        let results = store.find_top_k(
            &query_vec,
            "кіт дедлайн",
            &cfg,
            1,
            "all".to_string(),
            SearchFilter::default(),
        );

        assert!(!results.is_empty());
        assert_eq!(results[0].0.id, 1);
        assert!(results[0].1 > 0.93);
    }

    #[test]
    fn fts_query_handles_special_chars_and_update_meme_reprocessed_syncs_index() {
        let mut store = MemeStore::open(":memory:").unwrap();
        seed_memory_meme(
            &mut store,
            10,
            "старий текст",
            "старий опис про чайник",
            "old kettle description",
            "",
            vec![1.0, 0.0],
        );

        let special_hits = store.query_fts_hits("що це? C++ (NOT) *чайник*", 10);
        assert!(special_hits.contains_key(&10));

        let updated = MemeAnalysis {
            is_nsfw: false,
            ocr_text: "новий текст".to_string(),
            uk_description: "свіжий опис про кавоварку".to_string(),
            en_description: "fresh coffee machine".to_string(),
        };
        store
            .update_meme_reprocessed(10, &updated, vec![1.0, 0.0], vec![0.0, 1.0], 2, "")
            .unwrap();

        assert!(!store.query_fts_hits("чайник", 10).contains_key(&10));
        assert!(store.query_fts_hits("кавоварка", 10).contains_key(&10));
    }

    #[test]
    fn find_top_k_keeps_pure_vector_matches_without_fts_hits() {
        let cfg = make_test_config(3);

        let semantic_hit = make_test_meme(1, 0x1000, "", vec![0.95, 0.31]);
        let memes = vec![semantic_hit];
        let query_vec = vec![0.95, 0.31];
        let empty_fts = HashMap::new();

        let results = find_top_k_impl(
            &query_vec,
            "абстрактний синонім",
            &memes,
            &cfg,
            1,
            "all".to_string(),
            SearchFilter::default(),
            &empty_fts,
        );

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0.id, 1);
    }

    #[test]
    fn stage1_candidates_fetches_full_passages_deduplicates_and_keeps_tail() {
        let mut store = MemeStore::open(":memory:").unwrap();
        let cfg = make_test_config(3);

        seed_memory_meme(
            &mut store,
            1,
            "мої повіки",
            "Дитина піднімає штангу, важко прокидатись зранку",
            "Kid lifting barbell",
            "",
            vec![1.0, 0.0],
        );
        // Distant vector score (gap > 0.032) that select_relative would drop,
        // but Stage-1 must retain for the Cross-Encoder to evaluate.
        seed_memory_meme(
            &mut store,
            2,
            "",
            "Кіт у паніці дивиться на годинник",
            "Panicked cat looking at clock",
            "",
            vec![0.8, 0.6],
        );

        let query_vec = vec![1.0, 0.0];
        let candidates = store.stage1_candidates_from_embedding(
            &query_vec,
            "важко вранці",
            &cfg,
            1,
            "all",
            SearchFilter::default(),
            15,
        );

        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].meme.id, 1);
        assert_eq!(
            candidates[0].passage,
            "мої повіки | Дитина піднімає штангу, важко прокидатись зранку"
        );
        assert_eq!(candidates[1].meme.id, 2);
        assert_eq!(candidates[1].passage, "Кіт у паніці дивиться на годинник");
    }

    #[test]
    fn tinder_reactions_track_unseen_separate_likes_and_reset_skips() {
        let mut store = MemeStore::open(":memory:").unwrap();
        seed_memory_meme(&mut store, 1, "m1", "опис 1", "desc 1", "", vec![1.0, 0.0]);
        seed_memory_meme(&mut store, 2, "m2", "опис 2", "desc 2", "", vec![1.0, 0.0]);
        seed_memory_meme(&mut store, 3, "m3", "опис 3", "desc 3", "", vec![1.0, 0.0]);

        let viewer_id = 2;

        // Like meme 1 -> added to /liked, NOT added to personal collection (user_memes / owners)
        store.record_reaction(viewer_id, 1, "like").unwrap();
        let liked_ids: Vec<u64> = store.get_liked_memes(viewer_id).iter().map(|m| m.id).collect();
        assert_eq!(liked_ids, vec![1]);
        assert!(!store.memes[0].is_owned_by(viewer_id));
        let (_, remaining) = store.get_next_tinder_meme(viewer_id).unwrap();
        assert_eq!(remaining, 2);

        // Dislike meme 2 -> not in /liked, only meme 3 remains unseen
        store.record_reaction(viewer_id, 2, "dislike").unwrap();
        let liked_ids: Vec<u64> = store.get_liked_memes(viewer_id).iter().map(|m| m.id).collect();
        assert_eq!(liked_ids, vec![1]);
        let (next_meme, remaining) = store.get_next_tinder_meme(viewer_id).unwrap();
        assert_eq!(next_meme.id, 3);
        assert_eq!(remaining, 1);

        // Skip meme 3 -> deck exhausted, skip count == 1
        store.record_reaction(viewer_id, 3, "skip").unwrap();
        assert!(store.get_next_tinder_meme(viewer_id).is_none());
        assert_eq!(store.count_user_skips(viewer_id), 1);

        // Reset skips -> only skipped meme 3 returns; like on 1 and dislike on 2 stay recorded
        assert_eq!(store.reset_user_skips(viewer_id).unwrap(), 1);
        let (next_meme, remaining) = store.get_next_tinder_meme(viewer_id).unwrap();
        assert_eq!(next_meme.id, 3);
        assert_eq!(remaining, 1);
        assert_eq!(store.count_user_likes(viewer_id), 1);

        // Removing like on 1 clears it from /liked
        store.remove_reaction(viewer_id, 1).unwrap();
        assert!(store.get_liked_memes(viewer_id).is_empty());
    }

    #[test]
    fn user_reaction_counts_and_selective_resets() {
        let mut store = MemeStore::open(":memory:").unwrap();
        seed_memory_meme(&mut store, 1, "m1", "опис 1", "desc 1", "", vec![1.0, 0.0]);
        seed_memory_meme(&mut store, 2, "m2", "опис 2", "desc 2", "", vec![1.0, 0.0]);
        seed_memory_meme(&mut store, 3, "m3", "опис 3", "desc 3", "", vec![1.0, 0.0]);

        let user_a = 10;
        let user_b = 20;

        store.record_reaction(user_a, 1, "like").unwrap();
        store.record_reaction(user_a, 2, "dislike").unwrap();
        store.record_reaction(user_a, 3, "skip").unwrap();
        store.record_reaction(user_b, 1, "dislike").unwrap();

        assert_eq!(
            store.get_user_reaction_counts(user_a),
            UserReactionCounts {
                likes: 1,
                dislikes: 1,
                skips: 1,
            }
        );
        assert_eq!(store.get_user_reaction_counts(user_a).total(), 3);

        // Reset dislikes for user_a -> meme 2 returns to /tinder, likes and skips remain
        assert_eq!(store.reset_user_reactions(user_a, Some("dislike")), Ok(1));
        assert_eq!(
            store.get_user_reaction_counts(user_a),
            UserReactionCounts {
                likes: 1,
                dislikes: 0,
                skips: 1,
            }
        );
        let (next_meme, remaining) = store.get_next_tinder_meme(user_a).unwrap();
        assert_eq!(next_meme.id, 2);
        assert_eq!(remaining, 1);

        // Reset likes for user_a -> /liked is cleared, skips stay
        assert_eq!(store.reset_user_reactions(user_a, Some("like")), Ok(1));
        assert_eq!(
            store.get_user_reaction_counts(user_a),
            UserReactionCounts {
                likes: 0,
                dislikes: 0,
                skips: 1,
            }
        );
        assert!(store.get_liked_memes(user_a).is_empty());

        // Re-add reactions for user_a and reset all -> user_a cleared, user_b untouched
        store.record_reaction(user_a, 1, "like").unwrap();
        store.record_reaction(user_a, 2, "dislike").unwrap();
        assert_eq!(store.reset_user_reactions(user_a, None), Ok(3));
        assert_eq!(store.get_user_reaction_counts(user_a).total(), 0);
        assert_eq!(store.get_user_reaction_counts(user_b).dislikes, 1);

        assert!(store.reset_user_reactions(user_a, Some("bogus")).is_err());
    }

    #[test]
    fn drip_subscriptions_track_due_exclude_delivered_and_disliked() {
        let mut store = MemeStore::open(":memory:").unwrap();
        seed_memory_meme(&mut store, 1, "m1", "опис 1", "desc 1", "", vec![1.0, 0.0]);
        seed_memory_meme(&mut store, 2, "m2", "опис 2", "desc 2", "", vec![1.0, 0.0]);
        seed_memory_meme(&mut store, 3, "m3", "опис 3", "desc 3", "", vec![1.0, 0.0]);

        let user_id = 42;
        let sub = DripSubscription {
            user_id,
            chat_id: user_id,
            memes_per_day: 3,
            tz_offset_hours: 2,
            quiet_start_hour: 23,
            quiet_end_hour: 9,
            next_delivery_at: 1000,
            is_active: true,
        };
        store.upsert_drip_subscription(&sub).unwrap();

        assert!(store.get_due_drip_subscriptions(999).is_empty());
        assert_eq!(store.get_due_drip_subscriptions(1000).len(), 1);
        assert_eq!(store.get_drip_subscription(user_id).unwrap(), sub);

        // Dislike meme 1 and mark meme 2 delivered -> only meme 3 remains eligible
        store.record_reaction(user_id, 1, "dislike").unwrap();
        store.record_delivered_meme(user_id, 2, 1000).unwrap();
        assert_eq!(store.pick_next_drip_meme(user_id).unwrap().id, 3);

        // Deliver meme 3 -> no unseen memes left
        store.record_delivered_meme(user_id, 3, 1005).unwrap();
        assert!(store.pick_next_drip_meme(user_id).is_none());
        assert_eq!(store.count_user_delivered_memes(user_id), 2);

        // Reset delivery history -> memes 2 and 3 return, disliked meme 1 stays excluded
        assert_eq!(store.reset_user_delivered_memes(user_id), Ok(2));
        assert_eq!(store.count_user_delivered_memes(user_id), 0);
        for _ in 0..10 {
            let picked = store.pick_next_drip_meme(user_id).unwrap().id;
            assert!(picked == 2 || picked == 3);
        }

        // Pause subscription -> excluded from due subscriptions
        store.set_drip_active(user_id, false).unwrap();
        assert!(store.get_due_drip_subscriptions(1000).is_empty());
    }

    #[test]
    fn tags_persist_count_and_filter_search_candidates() {
        let mut store = MemeStore::open(":memory:").unwrap();
        let cfg = make_test_config(3);

        let initial_taxonomy = store.get_active_taxonomy();
        assert_eq!(initial_taxonomy.len(), TAG_TAXONOMY.len());
        assert!(!initial_taxonomy.iter().any(|t| t.slug == OTHER_TAG_SLUG));

        let custom_tag = DynamicTagDef {
            slug: "авто".to_string(),
            emoji: "🚗".to_string(),
            instruction: "Does this meme relate to cars or driving?".to_string(),
        };
        assert!(store.add_dynamic_category(&custom_tag).unwrap());
        assert!(!store.add_dynamic_category(&custom_tag).unwrap());
        assert_eq!(store.get_active_taxonomy().len(), TAG_TAXONOMY.len() + 1);

        seed_memory_meme(&mut store, 1, "баг в прод", "Програміст плаче", "Dev crying", "", vec![1.0, 0.0]);
        seed_memory_meme(&mut store, 2, "мавпа кодить", "Мавпа за ноутбуком", "Monkey coding", "", vec![1.0, 0.0]);
        seed_memory_meme(&mut store, 3, "трек грає", "Незрозумілий мем", "Misc meme", "", vec![1.0, 0.0]);

        store
            .set_meme_tags(1, &[("айті".to_string(), 0.95), ("робота".to_string(), 0.85)], 1)
            .unwrap();
        store
            .set_meme_tags(2, &[("мавпи".to_string(), 0.96), ("айті".to_string(), 0.91)], 1)
            .unwrap();
        store
            .set_meme_tags(
                3,
                &[(OTHER_TAG_SLUG.to_string(), 0.78)],
                TAGS_STATE_UNDETERMINED,
            )
            .unwrap();

        assert_eq!(store.get_tagging_progress(1), (3, 3));
        assert!(store.get_memes_for_tagging(1, 10).is_empty());

        let undetermined = store.get_undetermined_memes_for_discovery(10);
        assert_eq!(undetermined.len(), 1);
        assert_eq!(undetermined[0].id, 3);

        store
            .mark_memes_tags_version(&[3], crate::constants::TAGS_STATE_REVIEWED_OTHER)
            .unwrap();
        assert!(store.get_undetermined_memes_for_discovery(10).is_empty());
        assert_eq!(store.get_tagging_progress(1), (3, 3));

        let counts = store.get_tag_counts(1);
        assert_eq!(counts.last().map(|(n, _, _)| n.as_str()), Some(OTHER_TAG_SLUG));
        assert_eq!(
            counts,
            vec![
                ("айті".to_string(), "💻".to_string(), 2),
                ("мавпи".to_string(), "🐒".to_string(), 1),
                ("робота".to_string(), "💼".to_string(), 1),
                (OTHER_TAG_SLUG.to_string(), OTHER_TAG_EMOJI.to_string(), 1),
            ]
        );

        let multi_tags = vec!["мавпи".to_string(), "айті".to_string()];
        let by_tags = store.get_memes_by_tags(1, &multi_tags, 10, None);
        assert_eq!(by_tags.len(), 1);
        assert_eq!(by_tags[0].id, 2);

        let query_vec = vec![1.0, 0.0];
        let filtered = store.find_top_k(
            &query_vec,
            "мем",
            &cfg,
            1,
            "all".to_string(),
            SearchFilter {
                media: None,
                tags: &multi_tags,
            },
        );
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].0.id, 2);
    }

    #[test]
    fn user_meme_visibility_toggle_and_ownership_checks() {
        let mut store = MemeStore::open(":memory:").unwrap();
        seed_memory_meme(&mut store, 1, "один", "Опис", "Desc", "", vec![1.0, 0.0]);
        store.memes[0].is_public = true;
        store.memes[0].owners = vec![10, 20];
        store
            .conn
            .execute(
                "INSERT INTO user_memes (user_id, meme_id, user_captions, is_public, created_at) VALUES (10, 1, '', 1, 100), (20, 1, '', 0, 101)",
                [],
            )
            .unwrap();
        store
            .set_meme_tags(1, &[("айті".to_string(), 0.9)], 1)
            .unwrap();

        // Non-owner gets None
        assert!(store.get_meme_card_info(999, 1).is_none());
        assert!(
            store
                .set_user_meme_visibility(999, 1, true)
                .unwrap()
                .is_none()
        );

        // User 20 sees their personal flag = false, effective = true (because user 10 is public)
        let card20 = store.get_meme_card_info(20, 1).unwrap();
        assert!(!card20.user_is_public);
        assert!(card20.effective_is_public);
        assert_eq!(card20.tags, vec![("айті".to_string(), "💻".to_string())]);

        // User 10 toggles private -> both personal and effective become false
        let card10 = store.set_user_meme_visibility(10, 1, false).unwrap().unwrap();
        assert!(!card10.user_is_public);
        assert!(!card10.effective_is_public);
        assert!(!store.memes[0].is_public);

        // User 20 toggles public -> user 20 and effective become true
        let card20_pub = store.set_user_meme_visibility(20, 1, true).unwrap().unwrap();
        assert!(card20_pub.user_is_public);
        assert!(card20_pub.effective_is_public);
        assert!(store.memes[0].is_public);
    }

    #[test]
    fn delete_user_meme_handles_single_and_shared_ownership() {
        let mut store = MemeStore::open(":memory:").unwrap();
        seed_memory_meme(&mut store, 42, "текст", "опис", "desc", "", vec![1.0, 0.0]);
        store.memes[0].is_public = true;
        store.memes[0].owners = vec![1, 2];
        store
            .conn
            .execute(
                "INSERT INTO user_memes (user_id, meme_id, user_captions, is_public, created_at) VALUES (1, 42, '', 1, 100), (2, 42, '', 0, 101)",
                [],
            )
            .unwrap();
        store
            .set_meme_tags(42, &[("коти".to_string(), 0.9)], 1)
            .unwrap();
        store.record_reaction(2, 42, "like").unwrap();
        store.record_delivered_meme(2, 42, 500).unwrap();

        // Stranger cannot delete
        assert_eq!(
            store.delete_user_meme(99, 42).unwrap(),
            DeleteMemeOutcome::NotFoundOrNotOwner
        );

        // Owner 1 deletes -> removed from user 1 only, effective visibility drops to false
        assert_eq!(
            store.delete_user_meme(1, 42).unwrap(),
            DeleteMemeOutcome::RemovedFromCollection
        );
        assert_eq!(store.memes.len(), 1);
        assert_eq!(store.memes[0].owners, vec![2]);
        assert!(!store.memes[0].is_public);

        // Last owner 2 deletes -> deleted from memes, FTS, meme_tags, reactions, deliveries, memory
        assert_eq!(
            store.delete_user_meme(2, 42).unwrap(),
            DeleteMemeOutcome::DeletedCompletely
        );
        assert!(store.memes.is_empty());

        for (table, col) in [
            ("memes", "id"),
            ("memes_fts", "rowid"),
            ("meme_tags", "meme_id"),
            ("user_reactions", "meme_id"),
            ("user_delivered_memes", "meme_id"),
        ] {
            let count: i64 = store
                .conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM {} WHERE {} = 42", table, col),
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 0, "expected 0 rows remaining in {}", table);
        }
    }

    #[test]
    fn random_meme_excludes_disliked_respects_scope_and_skips_excluded_id() {
        let mut store = MemeStore::open(":memory:").unwrap();
        seed_memory_meme(&mut store, 1, "a", "a", "a", "", vec![1.0, 0.0]);
        seed_memory_meme(&mut store, 2, "b", "b", "b", "", vec![1.0, 0.0]);
        seed_memory_meme(&mut store, 3, "c", "c", "c", "", vec![1.0, 0.0]);

        store.memes[0].is_public = true;
        store.memes[0].owners = vec![100];
        store.memes[1].is_public = false;
        store.memes[1].owners = vec![1];
        store.memes[2].is_public = false;
        store.memes[2].owners = vec![200];

        // User 1 in "all" sees 1 (public) and 2 (private owned by 1), not 3 (private owned by 200)
        store.record_reaction(1, 1, "dislike").unwrap();
        let picked = store.random_meme(1).unwrap();
        assert_eq!(picked.id, 2);

        // Excluding the only remaining candidate (2) still returns 2 as last fallback
        assert_eq!(store.random_meme_excluding(1, Some(2)).unwrap().id, 2);

        // When both 1 and 2 are available and 1 is excluded, always picks 2
        store.remove_reaction(1, 1).unwrap();
        for _ in 0..5 {
            assert_eq!(store.random_meme_excluding(1, Some(1)).unwrap().id, 2);
        }

        // Switching scope to private_only restricts to meme 2; disliking 2 yields None
        store
            .save_users_settings(UserSetting {
                user_id: 1,
                username: String::new(),
                first_name: String::new(),
                last_name: String::new(),
                upload_as_public: true,
                search_scope: "private_only".to_string(),
            })
            .unwrap();
        assert_eq!(store.random_meme(1).unwrap().id, 2);
        store.record_reaction(1, 2, "dislike").unwrap();
        assert!(store.random_meme(1).is_none());
    }

    #[test]
    fn vlm_model_quota_persists_roundtrip() {
        let store = MemeStore::open(":memory:").unwrap();
        let entry = PersistedModelQuota {
            model_id: "gemini-3.5-flash-lite".to_string(),
            pt_day_key: "2026-11-23".to_string(),
            used_today: 42,
            cooldown_until: 1_795_478_400,
            last_reason: "daily_quota".to_string(),
        };
        store.upsert_vlm_quota(&entry).unwrap();

        let updated = PersistedModelQuota {
            used_today: 500,
            ..entry.clone()
        };
        store.upsert_vlm_quota(&updated).unwrap();

        let loaded = store.load_vlm_quotas();
        assert_eq!(loaded, vec![updated]);
    }

    #[test]
    fn golden_search_eval_suite_tracks_hit_at_3_mrr_and_negative_rejection() {
        let mut store = MemeStore::open(":memory:").unwrap();
        let cfg = make_test_config(3);

        seed_memory_meme(
            &mut store,
            1,
            "коли прод впав о третій ночі",
            "Програміст дивиться на палаючий сервер через баг у коді",
            "Developer watching burning server after production bug",
            "Айті бекенд",
            vec![1.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        );
        seed_memory_meme(
            &mut store,
            2,
            "без кави не працюю",
            "Сонний кіт тримає велику чашку гарячої кави вранці",
            "Sleepy cat holding a large cup of hot coffee in the morning",
            "Ранкова кава",
            vec![0.0, 1.0, 0.0, 0.0, 0.0, 0.0],
        );
        seed_memory_meme(
            &mut store,
            3,
            "зарплата прийшла і пішла",
            "Порожній гаманець після оплати оренди квартири",
            "Empty wallet right after paying apartment rent",
            "Фінанси",
            vec![0.0, 0.0, 1.0, 0.0, 0.0, 0.0],
        );
        seed_memory_meme(
            &mut store,
            4,
            "спина болить після 30",
            "Чоловік тримається за поперек після підйому важкої коробки",
            "Man holding lower back in pain after lifting heavy box",
            "Здоров'я",
            vec![0.0, 0.0, 0.0, 1.0, 0.0, 0.0],
        );
        seed_memory_meme(
            &mut store,
            5,
            "пиво у п'ятницю",
            "Пес у сонцезахисних окулярах святкує кінець робочого тижня",
            "Dog wearing sunglasses celebrating Friday evening",
            "П'ятниця",
            vec![0.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        );
        seed_memory_meme(
            &mut store,
            6,
            "мавпа в Полтаві",
            "Людина в костюмі горили сидить вночі на зупинці транспорту",
            "Person in gorilla suit sitting at a bus stop at night",
            "Зупинка",
            vec![0.0, 0.0, 0.0, 0.0, 0.0, 1.0],
        );

        enum QueryKind {
            Positive {
                query: &'static str,
                query_vec: [f32; 6],
                expected_id: u64,
            },
            Negative {
                query: &'static str,
                query_vec: [f32; 6],
            },
        }

        let cases = [
            // standard (lexical + semantic)
            QueryKind::Positive {
                query: "палаючий сервер баг",
                query_vec: [0.95, 0.1, 0.0, 0.0, 0.0, 0.0],
                expected_id: 1,
            },
            QueryKind::Positive {
                query: "кіт кава вранці",
                query_vec: [0.0, 0.96, 0.0, 0.0, 0.0, 0.0],
                expected_id: 2,
            },
            // syn_zero (semantic paraphrase, zero stemmed FTS overlap)
            QueryKind::Positive {
                query: "брак грошей на житло",
                query_vec: [0.0, 0.0, 0.94, 0.0, 0.0, 0.0],
                expected_id: 3,
            },
            QueryKind::Positive {
                query: "горила чекає автобус",
                query_vec: [0.0, 0.0, 0.0, 0.0, 0.0, 0.93],
                expected_id: 6,
            },
            // NEGATIVE (unrelated / nonsense queries below 0.5 cosine & zero FTS match)
            QueryKind::Negative {
                query: "квантовий марсохід на орбіті",
                query_vec: [0.2, 0.2, 0.2, 0.2, 0.2, 0.2],
            },
            QueryKind::Negative {
                query: "абвгд xyzzy невідоме слово",
                query_vec: [0.15, -0.3, 0.25, -0.2, 0.1, -0.25],
            },
        ];

        let mut positive_total = 0usize;
        let mut hits_at_3 = 0usize;
        let mut mrr_sum = 0.0f32;
        let mut negative_total = 0usize;
        let mut negative_rejected = 0usize;

        for case in cases {
            match case {
                QueryKind::Positive {
                    query,
                    query_vec,
                    expected_id,
                } => {
                    positive_total += 1;
                    let res = store.find_top_k(
                        &query_vec,
                        query,
                        &cfg,
                        1,
                        "all".to_string(),
                        SearchFilter::default(),
                    );
                    if let Some(rank_idx) = res.iter().position(|(m, _)| m.id == expected_id) {
                        if rank_idx < 3 {
                            hits_at_3 += 1;
                        }
                        mrr_sum += 1.0 / ((rank_idx + 1) as f32);
                    }
                }
                QueryKind::Negative { query, query_vec } => {
                    negative_total += 1;
                    let res = store.find_top_k(
                        &query_vec,
                        query,
                        &cfg,
                        1,
                        "all".to_string(),
                        SearchFilter::default(),
                    );
                    if res.is_empty() {
                        negative_rejected += 1;
                    }
                }
            }
        }

        let hit_at_3_rate = hits_at_3 as f32 / positive_total as f32;
        let mrr = mrr_sum / positive_total as f32;
        let neg_rejection_rate = negative_rejected as f32 / negative_total as f32;

        assert!(
            (hit_at_3_rate - 1.0).abs() < f32::EPSILON,
            "Hit@3 dropped to {}",
            hit_at_3_rate
        );
        assert!(mrr >= 0.85, "MRR dropped to {}", mrr);
        assert!(
            (neg_rejection_rate - 1.0).abs() < f32::EPSILON,
            "Negative rejection dropped to {}",
            neg_rejection_rate
        );
    }
}
