pub fn embedding_to_blob(embedding: &[f32]) -> Vec<u8> {
    embedding
        .iter()
        .flat_map(|&val| val.to_le_bytes())
        .collect()
}
pub fn blob_to_embedding(blob: &[u8]) -> Vec<f32> {
    blob.as_chunks::<4>()
        .0
        .iter()
        .map(|chunk| f32::from_le_bytes(*chunk))
        .collect()
}

pub fn format_embedding_passage(ocr_text: &str, description: &str) -> String {
    if ocr_text.is_empty() {
        description.to_string()
    } else {
        format!("{} | {}", ocr_text, description)
    }
}
