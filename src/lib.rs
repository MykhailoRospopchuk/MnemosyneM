pub mod db;
pub mod engine;
pub mod vlm;

pub fn score_calibration(raw_score: f32) -> f32 {
    ((raw_score - 0.75) / (0.85 - 0.75) * 100.0).clamp(0.0, 100.0)
}
