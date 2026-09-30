use MnemosyneM::{cosine_similarity, ClipEngine, MemeStore};

fn main() {
    println!("Hello, world!");
    let mut engine = ClipEngine::new().unwrap();
    let embed_text_related = engine.embed_text("A German Shepherd is looking at a monitor").unwrap();
    let embed_text_unrelated = engine.embed_text("money cards and two barrels").unwrap();
    let embed_image = engine.embed_image("samples/photo_2026-07-15_08-58-27.jpg").unwrap();

    let sim_texts_mismatch = cosine_similarity(&embed_text_related, &embed_text_unrelated).unwrap();
    let sim_match = cosine_similarity(&embed_text_related, &embed_image).unwrap();
    let sim_unrelated = cosine_similarity(&embed_text_unrelated, &embed_image).unwrap();

    println!("Compare two text: {:?}", sim_texts_mismatch);
    println!("Compare related text with image vector: {:?}", sim_match);
    println!("Compare NOT related text with image vector: {:?}", sim_unrelated);

    let mut meme_store = MemeStore::new();

    meme_store.add_from_file(1, "powerful teacher explain with big boobs".to_string(), "samples/photo_1_2026-09-30_21-36-02.jpg".to_string(), &mut engine).unwrap();
    meme_store.add_from_file(2, "chimpanzee have breakfast".to_string(), "samples/photo_2_2026-09-30_21-36-02.jpg".to_string(), &mut engine).unwrap();
    meme_store.add_from_file(3, "dog with flower".to_string(), "samples/photo_3_2026-09-30_21-36-02.jpg".to_string(), &mut engine).unwrap();
    meme_store.add_from_file(4, "lore update".to_string(), "samples/photo_4_2026-09-30_21-36-02.jpg".to_string(), &mut engine).unwrap();
    meme_store.add_from_file(5, "cow on the beach".to_string(), "samples/photo_5_2026-09-30_21-36-02.jpg".to_string(), &mut engine).unwrap();
    meme_store.add_from_file(6, "i dream of a great war of justice that will turn the American soil to ashes".to_string(), "samples/photo_2026-09-30_21-58-51.jpg".to_string(), &mut engine).unwrap();

    let best_match_meme = meme_store.search("north Korea citizen give interview", &mut engine).unwrap().unwrap();

    meme_store.save_to_file("meme_storage.json").unwrap();

    println!("ID: {:?} | Title: {:?}", best_match_meme.id, best_match_meme.title);



    println!("DONE!");
}
