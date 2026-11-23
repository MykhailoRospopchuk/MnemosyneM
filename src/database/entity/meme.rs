use serde::{Deserialize, Serialize};

use crate::database::entity::media_type::MediaType;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Meme {
    pub id: u64,
    pub title: String,
    pub ocr_text: String,
    pub embedding_uk: Vec<f32>,
    pub embedding_en: Vec<f32>,
    pub file_tg_id: String,
    pub file_tg_unique_id: String,
    pub source_title: String,
    pub source_url: String,
    pub is_public: bool,
    pub owners: Vec<i64>,
    pub phash: u64,
    pub media_type: MediaType,
    pub thumbnail_tg_id: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

impl Meme {
    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|t| t == tag)
    }

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
