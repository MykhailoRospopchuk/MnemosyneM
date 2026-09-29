use MnemosyneM::{cosine_similarity, ClipEngine};

fn main() {
    println!("Hello, world!");
    let mut engine = ClipEngine::new().unwrap();
    let embed_text_related = engine.embed_text("A German Shepherd is looking at a monitor").unwrap();
    let embed_text_unrelated = engine.embed_text("money cards and two barrels").unwrap();
    let embed_image = engine.embed_image("photo_2026-07-15_08-58-27.jpg").unwrap();

    let sim_texts_mismatch = cosine_similarity(&embed_text_related, &embed_text_unrelated).unwrap();
    let sim_match = cosine_similarity(&embed_text_related, &embed_image).unwrap();
    let sim_unrelated = cosine_similarity(&embed_text_unrelated, &embed_image).unwrap();

    println!("Compare two text: {:?}", sim_texts_mismatch);
    println!("Compare related text with image vector: {:?}", sim_match);
    println!("Compare NOT related text with image vector: {:?}", sim_unrelated);

    println!("DONE!");
}
