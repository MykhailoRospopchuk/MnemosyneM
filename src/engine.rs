use std::path::PathBuf;

use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};

pub struct ClipEngine {
    text_model: TextEmbedding,
}

impl ClipEngine {
    pub fn new() -> Result<Self, String> {
        let mut options = TextInitOptions::new(EmbeddingModel::MultilingualE5Base);
        if let Ok(cache_dir) = std::env::var("FASTEMBED_CACHE_DIR")
            && !cache_dir.trim().is_empty()
        {
            options = options.with_cache_dir(PathBuf::from(cache_dir));
        }

        let text_model = TextEmbedding::try_new(options).map_err(|e| e.to_string())?;

        Ok(Self { text_model })
    }

    pub fn embed_passage(&mut self, text: &str) -> Result<Vec<f32>, String> {
        let format_passage = format!("passage: {}", text);
        let mut embed_vec = self
            .text_model
            .embed(vec![format_passage], None)
            .map_err(|e| e.to_string())?;

        embed_vec
            .pop()
            .ok_or_else(|| "embedding passage is empty".to_string())
    }

    pub fn embed_query(&mut self, text: &str) -> Result<Vec<f32>, String> {
        let format_query = format!("query: {}", text);
        let mut embed_vec = self
            .text_model
            .embed(vec![format_query], None)
            .map_err(|e| e.to_string())?;

        embed_vec
            .pop()
            .ok_or_else(|| "embedding query is empty".to_string())
    }

    pub fn embed_passages_pair(
        &mut self,
        uk: &str,
        en: &str,
    ) -> Result<(Vec<f32>, Vec<f32>), String> {
        Ok((self.embed_passage(uk)?, self.embed_passage(en)?))
    }
}
