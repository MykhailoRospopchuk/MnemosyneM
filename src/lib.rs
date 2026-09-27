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
pub struct Meme {
    id: u64,
    title: String,
    embeding: Vec<f32>,
}

impl Meme {
    pub fn new(id: u64, title: String, embeding: Vec<f32>) -> Self {
        Self {id, title, embeding }
    }
}