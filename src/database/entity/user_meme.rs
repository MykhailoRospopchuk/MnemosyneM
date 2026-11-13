#[derive(Debug, Clone, PartialEq)]
pub struct UserMeme {
    pub user_id: i64,
    pub meme_id: u64,
    pub user_captions: String,
    pub is_public: bool,
    pub created_at: i64,
}
