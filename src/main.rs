use std::sync::{Arc, OnceLock};
use std::env;
use tokio::sync::Mutex;
use teloxide::Bot;
use teloxide::net::Download;
use teloxide::types::{InputFile, Message};
use teloxide::prelude::*;
use MnemosyneM::{ClipEngine, MemeStore};

const TOP_K: usize = 3;

pub struct AppConfig{
    pub json_storage: String,
    pub top_k: usize,
}

static CONFIG: OnceLock<AppConfig> = OnceLock::new();

pub fn config() -> &'static AppConfig {
    CONFIG.get_or_init(|| {
        AppConfig{
            json_storage: env::var("JSON_STORAGE").expect("Couldn't read JSON_STORAGE"),
            top_k: env::var("TOP_K").expect("Couldn't read TOP_K").parse::<usize>().unwrap_or(TOP_K),
        }
    })
}

struct AppState {
    meme_store: Mutex<MemeStore>,
    engine: Mutex<ClipEngine>
}

#[tokio::main]
async fn main() {
    println!("Hello, world!");

    dotenvy::dotenv().ok();
    match env::var("TELOXIDE_TOKEN") {
        Ok(url) => println!("TELOXIDE_TOKEN: {}", url),
        Err(e) => eprintln!("Couldn't read TELOXIDE_TOKEN: {}", e),
    }

    match env::var("JSON_STORAGE") {
        Ok(url) => println!("JSON_STORAGE: {}", url),
        Err(e) => eprintln!("Couldn't read JSON_STORAGE: {}", e),
    }

    let app_config = crate::config();

    println!("Loading storage and models...");

    let engine = ClipEngine::new().expect("Failed to create clip engine");
    let store = MemeStore::load_from_file(&app_config.json_storage).unwrap_or_else(|_| MemeStore::new());

    let state = Arc::new(AppState {
        meme_store: Mutex::new(store),
        engine: Mutex::new(engine)
    });

    let bot = Bot::from_env();

    println!("Let's get it started");

    teloxide::repl(bot, move |bot: Bot, msg: Message| {
        let state = Arc::clone(&state);
        async move {
            if let Some(photos) = msg.photo(){
                if let Some(photo) = photos.last() {
                    bot.send_message(msg.chat.id, "Your meme has been confiscated...").await?;

                    //  1. load file to storage
                    let file = bot.get_file(photo.file.id.clone()).await?;
                    let file_path = format!("samples/{}.jpeg", photo.file.unique_id);
                    let mut destination = tokio::fs::File::create(&file_path).await.unwrap();
                    bot.download_file(&file.path, &mut destination).await?;

                    // 2. detect name and ID
                    let title = msg.caption()
                        .unwrap_or("no content from lazy user")
                        .to_string();

                    let id = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs();

                    let mut engine = state.engine.lock().await;
                    let mut meme_store = state.meme_store.lock().await;

                    match meme_store.add_from_file(id, title.clone(), file_path, &mut engine) {
                        Ok(()) => {
                            let _ = meme_store.save_to_file(&app_config.json_storage);
                            bot.send_message(msg.chat.id, format!("[V] Your meme has been confiscated... {}", title)).await?;
                        }
                        Err(e) => {
                            bot.send_message(msg.chat.id, format!("[X] Walter we lost it... {}", e)).await?;
                        }
                    }
                }
            }
            else if let Some(text) = msg.text() {
                if text == "/start" {
                    bot.send_message(msg.chat.id, "Add meme description for search...").await?;
                    return Ok(());
                }

                bot.send_message(msg.chat.id, "Process search...").await?;

                let mut engine = state.engine.lock().await;
                let meme_store = state.meme_store.lock().await;

                match meme_store.search_top_k(text, &mut engine, app_config.top_k) {
                    Ok(results) if results.is_empty() => {
                        bot.send_message(msg.chat.id, "There is nothing here...").await?;
                    }
                    Ok(results) => {
                        for (meme, score) in results {
                            if !std::path::Path::new(&meme.file_path).exists() {
                                bot.send_message(msg.chat.id, format!("Somebody stole your meme(((( {} !", meme.title)).await?;
                                continue;
                            }
                            bot.send_photo(
                                msg.chat.id,
                                InputFile::file(&meme.file_path))
                                .caption(format!("🎯 {} (Score: {:.3} %)", meme.title, score * 100.0))
                                .await?;
                        }
                    }
                    Err(e) => {
                        bot.send_message(msg.chat.id, format!("Dead inside: {}", e)).await?;
                    }
                }
            }
            Ok(())
        }

    })
        .await;

    println!("DONE!");
}
