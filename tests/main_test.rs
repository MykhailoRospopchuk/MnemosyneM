use MnemosyneM::cosine_similarity;

#[test]
fn test_cosine_similarity() {
    let a = [1.2, 2.4, 4.2, 10f32];
    let result = cosine_similarity(&a, &a).unwrap();
    assert!((result - 1.0).abs() < 1e-6);
}
