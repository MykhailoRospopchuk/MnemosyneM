use std::sync::Arc;
use tokio::sync::Mutex;
use teloxide::Bot;
use teloxide::net::Download;
use teloxide::types::{InputFile, Message};
use teloxide::prelude::*;
use MnemosyneM::{cosine_similarity, ClipEngine, MemeStore};

struct AppState {
    meme_store: Mutex<MemeStore>,
    engine: Mutex<ClipEngine>
}

#[tokio::main]
async fn main() {
    println!("Hello, world!");

    dotenvy::dotenv().ok();
    println!("Loading storage and models...");

    let engine = ClipEngine::new().expect("Failed to create clip engine");
    let store = MemeStore::load_from_file("meme_storage.json").unwrap_or_else(|_| MemeStore::new());

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
                            let _ = meme_store.save_to_file("meme_storage.json");
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

                match meme_store.search(text, &mut engine) {
                    Ok(Some(meme)) => {
                        if !std::path::Path::new(&meme.file_path).exists() { 
                            bot.send_message(msg.chat.id, format!("Somebody stole your meme(((( {} !", meme.title)).await?;
                        }
                        bot.send_photo(
                            msg.chat.id,
                            InputFile::file(&meme.file_path))
                            .caption(format!("🎯 {}", meme.title))
                            .await?;
                    }
                    Ok(None) => {
                        bot.send_message(msg.chat.id, "There is nothing here...").await?;
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
