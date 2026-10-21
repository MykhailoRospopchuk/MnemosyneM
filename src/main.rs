use MnemosyneM::{ClipEngine, Meme, MemeStore, score_calibration};
use std::env;
use std::sync::{Arc, OnceLock};
use teloxide::Bot;
use teloxide::net::Download;
use teloxide::prelude::*;
use teloxide::types::{FileId, InputFile, InputMedia, InputMediaPhoto, Message};
use teloxide::utils::command::BotCommands;
use tokio::sync::Mutex;

const TOP_K: usize = 3;

pub struct AppConfig {
    pub db_path: String,
    pub top_k: usize,
}

#[derive(BotCommands, Clone)]
#[command(
    rename_rule = "lowercase",
    description = "These commands are supported:"
)]
enum Command {
    #[command(description = "Start a new bot")]
    Start,
    #[command(description = "Show list of all commands")]
    Help,
    #[command(description = "Get statistics of memes")]
    Stats,
    #[command(description = "Show meme random meme")]
    Random,
}

static CONFIG: OnceLock<AppConfig> = OnceLock::new();

pub fn config() -> &'static AppConfig {
    CONFIG.get_or_init(|| AppConfig {
        db_path: env::var("DB_PATH").expect("Couldn't read DB_PATH"),
        top_k: env::var("TOP_K")
            .expect("Couldn't read TOP_K")
            .parse::<usize>()
            .unwrap_or(TOP_K),
    })
}

struct AppState {
    meme_store: Mutex<MemeStore>,
    engine: Mutex<ClipEngine>,
}

#[tokio::main]
async fn main() {
    println!("Hello, world!");

    dotenvy::dotenv().ok();
    match env::var("TELOXIDE_TOKEN") {
        Ok(url) => println!("TELOXIDE_TOKEN: {}", url),
        Err(e) => eprintln!("Couldn't read TELOXIDE_TOKEN: {}", e),
    }

    match env::var("DB_PATH") {
        Ok(url) => println!("DB_PATH: {}", url),
        Err(e) => eprintln!("Couldn't read DB_PATH: {}", e),
    }

    let app_config = crate::config();

    println!("Loading storage and models...");

    let engine = ClipEngine::new().expect("Failed to create clip engine");
    let store = MemeStore::open(&app_config.db_path).expect("Failed to open database");

    let state = Arc::new(AppState {
        meme_store: Mutex::new(store),
        engine: Mutex::new(engine),
    });

    let bot = Bot::from_env();
    let _ = bot
        .set_my_commands(Command::bot_commands())
        .await
        .expect("Cannot set commands");

    println!("Let's get it started");

    teloxide::repl(bot, move |bot: Bot, msg: Message| {
        let state = Arc::clone(&state);
        async move {
            if let Some(photos) = msg.photo() {
                if let Some(photo) = photos.last() {
                    bot.send_message(msg.chat.id, "Your meme has been confiscated...")
                        .await?;

                    let file = bot.get_file(photo.file.id.clone()).await?;
                    let mut image_butes: Vec<u8> = Vec::new();
                    bot.download_file(&file.path, &mut image_butes).await?;

                    // 2. detect name and ID
                    let title = msg
                        .caption()
                        .unwrap_or("no content from lazy user")
                        .to_string();

                    let id = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs();

                    let telegram_file_id = photo.file.id.0.clone();

                    let mut engine = state.engine.lock().await;
                    let mut meme_store = state.meme_store.lock().await;

                    match meme_store.add_from_bytes(
                        id,
                        title.clone(),
                        telegram_file_id,
                        &image_butes,
                        &mut engine,
                    ) {
                        Ok(()) => {
                            bot.send_message(
                                msg.chat.id,
                                format!("[V] Your meme has been confiscated... {}", title),
                            )
                            .await?;
                        }
                        Err(e) => {
                            bot.send_message(
                                msg.chat.id,
                                format!("[X] Walter we lost it... {}", e),
                            )
                            .await?;
                        }
                    }
                }
            } else if let Some(text) = msg.text() {
                if text.starts_with('/') {
                    match Command::parse(text, "") {
                        Ok(Command::Start) => {
                            bot.send_message(
                                msg.chat.id,
                                "Hi! I'm MnemosyneM - semantic meme search \n\
                            Send me your meme pic and I will save it. \n\
                            Text me your search and I will find your meme\n\n\
                            List of commands: /help"
                                    .to_string(),
                            )
                            .await?;
                        }
                        Ok(Command::Help) => {
                            bot.send_message(msg.chat.id, Command::descriptions().to_string())
                                .await?;
                        }
                        Ok(Command::Stats) => {
                            let meme_store = state.meme_store.lock().await;
                            bot.send_message(
                                msg.chat.id,
                                format!("In storage we have [{}] memes", meme_store.len()),
                            )
                            .await?;
                        }
                        Ok(Command::Random) => {
                            let meme_store = state.meme_store.lock().await;
                            if let Some(meme) = meme_store.random_meme() {
                                if !meme.file_path.is_empty() {
                                    bot.send_photo(
                                        msg.chat.id,
                                        InputFile::file_id(FileId(meme.file_path.clone())),
                                    )
                                    .caption(format!("🎲 Випадковий мем: {}", meme.title))
                                    .await?;
                                } else {
                                    bot.send_message(
                                        msg.chat.id,
                                        "Sorry your meme somebody stole(((",
                                    )
                                    .await?;
                                }
                            } else {
                                bot.send_message(
                                    msg.chat.id,
                                    "What do yue even expect? Upload at least one meme",
                                )
                                .await?;
                            }
                        }
                        Err(_) => {
                            bot.send_message(
                                msg.chat.id,
                                "I have no idea what are you talking about. Use /help ...",
                            )
                            .await?;
                        }
                    }
                    return Ok(());
                }

                bot.send_message(msg.chat.id, "Process search...").await?;

                let mut engine = state.engine.lock().await;
                let meme_store = state.meme_store.lock().await;

                match meme_store.search_top_k(text, &mut engine, app_config.top_k) {
                    Ok(results) if results.is_empty() => {
                        bot.send_message(msg.chat.id, "There is nothing here...")
                            .await?;
                    }
                    Ok(results) => {
                        let valid_memes: Vec<(&Meme, f32)> = results
                            .into_iter()
                            .filter(|(meme, _)| !meme.file_path.is_empty())
                            .collect();

                        if valid_memes.is_empty() {
                            bot.send_message(msg.chat.id, "There is nothing useful here...")
                                .await?;
                        } else if valid_memes.len() == 1 {
                            let (single_meme, single_score) = valid_memes[0];

                            bot.send_photo(
                                msg.chat.id,
                                InputFile::file_id(FileId(single_meme.file_path.clone())),
                            )
                            .caption(format!(
                                "🎯 {} (Score: {:.3} %) - (Raw Score: {:.3})",
                                single_meme.title,
                                score_calibration(single_score),
                                single_score
                            ))
                            .await?;
                        } else {
                            let media_group: Vec<InputMedia> =
                                valid_memes
                                    .iter()
                                    .map(|(meme, score)| {
                                        InputMedia::Photo(
                                            InputMediaPhoto::new(InputFile::file_id(FileId(
                                                meme.file_path.clone(),
                                            )))
                                            .caption(format!(
                                                "🎯 {} (Score: {:.3} %) - (Raw Score: {:.3})",
                                                meme.title,
                                                score_calibration(*score),
                                                score
                                            )),
                                        )
                                    })
                                    .collect();

                            bot.send_media_group(msg.chat.id, media_group).await?;
                        }
                    }
                    Err(e) => {
                        bot.send_message(msg.chat.id, format!("Dead inside: {}", e))
                            .await?;
                    }
                }
            }
            Ok(())
        }
    })
    .await;

    println!("DONE!");
}
