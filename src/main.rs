mod bot;
mod config;
mod db;
mod engine;
mod vlm;

use std::sync::Arc;

use tokio::sync::Mutex;

struct AppState {
    meme_store: Mutex<db::MemeStore>,
    engine: Mutex<engine::ClipEngine>,
}

#[tokio::main]
async fn main() {
    println!("Hello, world!");

    dotenvy::dotenv().ok();
    let app_config = config::load_config();

    println!("Loading storage and models...");

    let engine = engine::ClipEngine::new().expect("Failed to create clip engine");
    let store = db::MemeStore::open(&app_config.db_path).expect("Failed to open database");

    let state = Arc::new(AppState {
        meme_store: Mutex::new(store),
        engine: Mutex::new(engine),
    });

    println!("Let's get it started");

    bot::run_bot(state, app_config).await;

    println!("DONE!");
}
