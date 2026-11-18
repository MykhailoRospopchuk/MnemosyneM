use crate::database::entity::media_type::MediaType;

pub struct NewMeme<'a> {
    pub id: u64,
    pub title: String,
    pub analysis: &'a crate::vlm::MemeAnalysis,
    pub file_tg_id: String,
    pub file_tg_unique_id: String,
    pub user_id: i64,
    pub source_title: String,
    pub source_url: String,
    pub phash: u64,
    pub media_type: MediaType,
    pub thumbnail_tg_id: String,
    pub vlm_version: i64,
}
