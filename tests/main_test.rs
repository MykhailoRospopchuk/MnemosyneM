use MnemosyneM::{cosine_similarity, find_best_usage};
use MnemosyneM::Meme;
use MnemosyneM::MemeStore;

#[test]
fn test_cosine_similarity() {
    let a = [1.2, 2.4, 4.2, 10f32];
    let result = cosine_similarity(&a, &a).unwrap();
    assert!((result - 1.0).abs() < 1e-6);
}

#[test]
fn test_find_best_usage() {
    let candidate = [1.2, 2.4, 4.2, 10f32];
    let memes = vec! [
        Meme::new(1, String::from("bobr_secta"), Vec::from(candidate), "some text".to_string()),
        Meme::new(2, String::from("chicken_secta"), vec![8.2, 2.4, 4.2, 10f32], "some another text".to_string()),
    ];

    let result = find_best_usage(&candidate, &memes);
    assert_eq!(result, Some(&memes[0]))
}

#[test]
fn test_meme_storage() {
    let meme = Meme::new(1, String::from("bobr_secta"), vec![8.2, 2.4, 4.2, 10f32], "there to new text".to_string());

    let mut meme_storage = MemeStore::new();

    meme_storage.add(meme);

    meme_storage.save_to_file("meme_storage.json").unwrap();

    let meme_from_storage = MemeStore::load_from_file("meme_storage.json").unwrap();

    let _ = std::fs::remove_file("meme_storage.json");

    assert_eq!(meme_from_storage, meme_storage);
}