use std::env;
use std::sync::OnceLock;

const TOP_K: usize = 3;
const MAX_PHASH_DISTANCE: u32 = 5;
const RERANK_MIN_SCORE: f32 = 0.005;
const RERANK_CANDIDATES: usize = 10;

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
        Ok(_) => println!("TELOXIDE_TOKEN: [CONFIGURED]"),
        Err(e) => eprintln!("Couldn't read TELOXIDE_TOKEN: {}", e),
    }

    match env::var("DB_PATH") {
        Ok(url) => println!("DB_PATH: {}", url),
        Err(e) => eprintln!("Couldn't read DB_PATH: {}", e),
    }

    match env::var("GEMINI_API_KEY") {
        Ok(_) => println!("GEMINI_API_KEY: [CONFIGURED]"),
        Err(e) => eprintln!("Couldn't read GEMINI_API_KEY: {}", e),
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
