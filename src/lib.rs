pub fn cosine_similarity(a: &[f32], b: &[f32]) -> Result<f32, String> {
    if a.len() != b.len() {
        return Err(format!("{} != {}", a.len(), b.len()));
    }

    let mut dot_product = 0f32;
    let mut sqrot_sq_a = 0f32;
    let mut sqrot_sq_b = 0f32;

    for (&x, &y) in a.iter().zip(b.iter()) {
        dot_product += x * y;
        sqrot_sq_a += x * x;
        sqrot_sq_b += y * y;
    }

    Ok(dot_product / (sqrot_sq_a.sqrt() * sqrot_sq_b.sqrt()))
}