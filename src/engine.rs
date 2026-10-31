use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};

pub struct ClipEngine {
    text_model: TextEmbedding,
}

impl ClipEngine {
    pub fn new() -> Result<Self, String> {
        let text_model =
            TextEmbedding::try_new(TextInitOptions::new(EmbeddingModel::MultilingualE5Base))
                .map_err(|e| e.to_string())?;

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
            .ok_or_else(|| "embeding passage is empty".to_string())
    }

    pub fn embed_query(&mut self, text: &str) -> Result<Vec<f32>, String> {
        let format_query = format!("query: {}", text);
        let mut embed_vec = self
            .text_model
            .embed(vec![format_query], None)
            .map_err(|e| e.to_string())?;

        embed_vec
            .pop()
            .ok_or_else(|| "embeding query is empty".to_string())
    }
}
