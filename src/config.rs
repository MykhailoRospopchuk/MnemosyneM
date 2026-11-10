use std::env;
use std::sync::OnceLock;

const TOP_K: usize = 3;
const MAX_PHASH_DISTANCE: u32 = 5;

pub struct AppConfig {
    pub db_path: String,
    pub top_k: usize,
    pub gemini_api_key: String,
    pub max_phash_distance: u32,
}

static CONFIG: OnceLock<AppConfig> = OnceLock::new();

pub fn load_config() -> &'static AppConfig {
    match env::var("TELOXIDE_TOKEN") {
        Ok(url) => println!("TELOXIDE_TOKEN: {}", url),
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
            .expect("Couldn't read TOP_K")
            .parse::<usize>()
            .unwrap_or(TOP_K),
        gemini_api_key: env::var("GEMINI_API_KEY").expect("Couldn't read GEMINI_API_KEY"),
        max_phash_distance: env::var("MAX_PHASH_DISTANCE")
            .expect("Couldn't read MAX_PHASH_DISTANCE")
            .parse::<u32>()
            .unwrap_or(MAX_PHASH_DISTANCE),
    })
}
