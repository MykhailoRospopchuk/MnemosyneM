use fastembed::{RerankInitOptions, RerankerModel, TextRerank};

pub struct RerankerEngine {
    model: TextRerank,
}

impl RerankerEngine {
    pub fn new() -> Result<Self, String> {
        let options = RerankInitOptions::new(RerankerModel::BGERerankerV2M3)
            .with_max_length(256)
            .with_show_download_progress(true);
        let model = TextRerank::try_new(options).map_err(|e| e.to_string())?;
        Ok(Self { model })
    }

    pub fn rerank(
        &mut self,
        query: &str,
        passages: &[String],
    ) -> Result<Vec<(usize, f32)>, String> {
        if passages.is_empty() {
            return Ok(Vec::new());
        }

        let docs: Vec<&str> = passages.iter().map(String::as_str).collect();
        let results = self
            .model
            .rerank(query, docs, false, None)
            .map_err(|e| e.to_string())?;

        Ok(results
            .into_iter()
            .map(|r| (r.index, sigmoid(r.score)))
            .collect())
    }
}

pub fn sigmoid(logit: f32) -> f32 {
    1.0 / (1.0 + (-logit).exp())
}

pub fn filter_reranked(
    scored: Vec<(usize, f32)>,
    min_score: f32,
    top_k: usize,
) -> Vec<(usize, f32)> {
    if scored.is_empty() || top_k == 0 {
        return Vec::new();
    }

    let top1 = scored[0].1;
    if top1 < min_score {
        return Vec::new();
    }

    let relative_floor = if top1 >= 0.70 {
        (top1 * 0.10).max(min_score)
    } else {
        min_score
    };

    scored
        .into_iter()
        .filter(|&(_, score)| score >= relative_floor)
        .take(top_k)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sigmoid_maps_logits_to_calibrated_probabilities() {
        assert!((sigmoid(0.0) - 0.5).abs() < 1e-6);
        assert!(sigmoid(-5.0) < 0.01);
        assert!(sigmoid(5.0) > 0.99);
    }

    #[test]
    fn filter_reranked_rejects_noise_and_trims_weak_tail() {
        // Pure noise below BGE threshold (e.g. ~1e-5) -> empty
        assert!(filter_reranked(vec![(0, 0.0002), (1, 0.00002)], 0.005, 3).is_empty());

        // Strong top1 (>= 0.70) drops weak tail (< 0.90 * 0.10 = 0.09)
        let kept = filter_reranked(vec![(2, 0.90), (0, 0.20), (1, 0.05)], 0.005, 3);
        assert_eq!(kept, vec![(2, 0.90), (0, 0.20)]);

        // Short relevant query (e.g. 0.017) passes 0.005 floor while noise (0.00002) is dropped
        let short_query = filter_reranked(vec![(0, 0.017), (1, 0.00002)], 0.005, 3);
        assert_eq!(short_query, vec![(0, 0.017)]);
    }
}
