use MnemosyneM::{cosine_similarity, find_best_usage};
use MnemosyneM::Meme;

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
        Meme::new(1, String::from("bobr_secta"), Vec::from(candidate)),
        Meme::new(2, String::from("chicken_secta"), vec![8.2, 2.4, 4.2, 10f32])
    ];

    let result = find_best_usage(&candidate, &memes);
    assert_eq!(result, Some(&memes[0]))
}
