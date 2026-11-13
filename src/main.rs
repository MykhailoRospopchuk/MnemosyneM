mod bot;
mod config;
mod database;
mod engine;
mod helpers;
mod maintenance;
mod meme_store;
mod migrations;
mod vlm;
mod worker;

use std::{collections::HashMap, sync::Arc};

use teloxide::Bot;
use tokio::sync::Mutex;

use crate::{maintenance::maintance_worker, meme_store::MemeStore};

struct AppState {
    meme_store: Mutex<MemeStore>,
    engine: Mutex<engine::ClipEngine>,
    last_inline_query: Mutex<HashMap<i64, String>>,
}

#[tokio::main]
async fn main() {
    println!("Hello, world!");

    dotenvy::dotenv().ok();
    let app_config = config::load_config();

    println!("Loading storage and models...");

    let mut engine = engine::ClipEngine::new().expect("Failed to create clip engine");
    migrations::run(&app_config.db_path, &mut engine).expect("Failed to run migration");
    let store = MemeStore::open(&app_config.db_path).expect("Failed to open database");

    let state = Arc::new(AppState {
        meme_store: Mutex::new(store),
        engine: Mutex::new(engine),
        last_inline_query: Mutex::new(HashMap::new()),
    });

    let bot = Bot::from_env();

    let (sender, receiver) = tokio::sync::mpsc::channel(100);
    worker::start_worker(
        receiver,
        sender.clone(),
        Arc::clone(&state),
        bot.clone(),
        app_config,
    );

    maintance_worker::start_maintenance(Arc::clone(&state), bot.clone());

    println!("Let's get it started");

    bot::run_bot(bot, state, app_config, sender).await;

    println!("DONE!");
}
