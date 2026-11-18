use crate::helpers::lexical_helper;

pub struct MemesFts {
    pub row_id: u64,
    pub ocr_norm: String,
    pub uk_norm: String,
    pub en_norm: String,
    pub title_norm: String,
}

impl MemesFts {
    pub fn from_raw(
        row_id: u64,
        ocr_raw: &str,
        uk_raw: &str,
        en_raw: &str,
        title_raw: &str,
    ) -> Self {
        Self {
            row_id,
            ocr_norm: lexical_helper::normalize_for_fts(ocr_raw),
            uk_norm: lexical_helper::normalize_for_fts(uk_raw),
            en_norm: lexical_helper::normalize_for_fts(en_raw),
            title_norm: lexical_helper::clean_title_for_fts(title_raw),
        }
    }
}
