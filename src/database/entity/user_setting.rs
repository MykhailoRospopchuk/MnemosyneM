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
