use fastembed::{EmbeddingModel, ImageEmbedding, ImageEmbeddingModel, ImageInitOptions, TextEmbedding, TextInitOptions};
use serde::{Deserialize, Serialize};

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

pub fn find_best_usage<'a>(query: &[f32], memes: &'a[Meme]) -> Option<&'a Meme> {
    if memes.is_empty() {
        return None;
    }

    let mut best_usage_idx = None;
    let mut best_usage = -1f32;
    for n in 0..memes.len() {
        if let Ok(value) = cosine_similarity(query, &memes[n].embeding) && value > best_usage {
            best_usage_idx = Some(n);
            best_usage = value;
        }
    }

    if best_usage_idx.is_none() {
        return None;
    }
    Some(&memes[best_usage_idx.unwrap()])
}

#[derive(Clone, Debug)]
#[derive(PartialEq)]
#[derive(Serialize, Deserialize)]
pub struct Meme {
    id: u64,
    title: String,
    embeding: Vec<f32>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[derive(PartialEq)]
pub struct MemeStore {
    memes: Vec<Meme>,
}

pub struct ClipEngine {
    text_model: TextEmbedding,
    image_model: ImageEmbedding,
}

impl Meme {
    pub fn new(id: u64, title: String, embeding: Vec<f32>) -> Self {
        Self {id, title, embeding }
    }
}

impl MemeStore {
    pub fn new() -> Self { Self::default() }

    pub fn add(&mut self, meme: Meme) {
        self.memes.push(meme);
    }

    pub fn find_best(&self, query: &[f32]) -> Option<&Meme> {
        find_best_usage(query, &self.memes)
    }

    pub fn save_to_file(&self, filename: &str) -> Result<(), String> {
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;

        std::fs::write(filename, json).map_err(|e| e.to_string())?;

        Ok(())
    }

    pub fn load_from_file(filename: &str) -> Result<Self, String> {
        let json = std::fs::read_to_string(filename).map_err(|e| e.to_string())?;

        let save_result  = serde_json::from_str::<MemeStore>(&json).map_err(|e| e.to_string())?;

        Ok(save_result)
    }
}

impl ClipEngine {
    pub fn new() -> Result<Self, String> {
        let text_model = TextEmbedding::try_new(TextInitOptions::new(EmbeddingModel::ClipVitB32)).map_err(|e| e.to_string())?;
        let image_model = ImageEmbedding::try_new(ImageInitOptions::new(ImageEmbeddingModel::ClipVitB32)).map_err(|e| e.to_string())?;

        Ok(Self { text_model, image_model })
    }

    pub fn embed_text(&mut self, text: &str) -> Result<Vec<f32>, String> {
        let mut embed_vec = self.text_model.embed(vec![text], None).map_err(|e| e.to_string())?;

        if embed_vec.is_empty() {
            return Err("embed text is empty".to_string());
        }

        let first_embed_vec = embed_vec.pop().unwrap();

        Ok(first_embed_vec)
    }

    pub fn embed_image(&mut self, path: &str) -> Result<Vec<f32>, String> {
        let mut embed_vec = self.image_model.embed(vec![path], None).map_err(|e| e.to_string())?;
        if embed_vec.is_empty() {
            return Err("embed image not found".to_string());
        }

        let first_embed_vec = embed_vec.pop().unwrap();
        Ok(first_embed_vec)
    }
}
