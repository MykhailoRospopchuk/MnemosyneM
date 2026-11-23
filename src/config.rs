use std::env;
use std::sync::OnceLock;
use tracing::{error, info};

const TOP_K: usize = 3;
const MAX_PHASH_DISTANCE: u32 = 5;
const RERANK_MIN_SCORE: f32 = 0.15;
const RERANK_CANDIDATES: usize = 5;

pub struct AppConfig {
    pub db_path: String,
    pub top_k: usize,
    pub gemini_api_key: String,
    pub max_phash_distance: u32,
    pub auto_reprocess: bool,
    pub enable_reranker: bool,
    pub rerank_inline: bool,
    pub rerank_min_score: f32,
    pub rerank_candidates: usize,
}

static CONFIG: OnceLock<AppConfig> = OnceLock::new();

pub fn load_config() -> &'static AppConfig {
    match env::var("TELOXIDE_TOKEN") {
        Ok(_) => info!("TELOXIDE_TOKEN: [CONFIGURED]"),
        Err(e) => error!(error = %e, "Couldn't read TELOXIDE_TOKEN"),
    }

    match env::var("DB_PATH") {
        Ok(url) => info!(db_path = %url, "DB_PATH configured"),
        Err(e) => error!(error = %e, "Couldn't read DB_PATH"),
    }

    match env::var("GEMINI_API_KEY") {
        Ok(_) => info!("GEMINI_API_KEY: [CONFIGURED]"),
        Err(e) => error!(error = %e, "Couldn't read GEMINI_API_KEY"),
    }

    CONFIG.get_or_init(|| AppConfig {
        db_path: env::var("DB_PATH").expect("Couldn't read DB_PATH"),
        top_k: env::var("TOP_K")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(TOP_K),

        gemini_api_key: env::var("GEMINI_API_KEY").expect("Couldn't read GEMINI_API_KEY"),
        max_phash_distance: env::var("MAX_PHASH_DISTANCE")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(MAX_PHASH_DISTANCE),

        auto_reprocess: env::var("AUTO_REPROCESS")
            .ok()
            .and_then(|v| v.parse::<bool>().ok())
            .unwrap_or(true),

        enable_reranker: env::var("ENABLE_RERANKER")
            .ok()
            .and_then(|v| v.parse::<bool>().ok())
            .unwrap_or(true),

        rerank_inline: env::var("RERANK_INLINE")
            .ok()
            .and_then(|v| v.parse::<bool>().ok())
            .unwrap_or(false),

        rerank_min_score: env::var("RERANK_MIN_SCORE")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .unwrap_or(RERANK_MIN_SCORE),

        rerank_candidates: env::var("RERANK_CANDIDATES")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(RERANK_CANDIDATES),
    })
}
