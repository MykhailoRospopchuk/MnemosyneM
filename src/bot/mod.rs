pub mod comand;

use crate::db;
use crate::vlm;

use std::sync::Arc;

use m_nemosyne_m::score_calibration;
use teloxide::Bot;
use teloxide::net::Download;
use teloxide::prelude::*;
use teloxide::types::{FileId, InputFile, InputMedia, InputMediaPhoto, Message};
use teloxide::utils::command::BotCommands;

use crate::AppState;
use crate::config::AppConfig;

pub async fn run_bot(state: Arc<AppState>, app_config: &'static AppConfig) {
    let bot = Bot::from_env();
    let _ = bot
        .set_my_commands(comand::Command::bot_commands())
        .await
        .expect("Cannot set commands");

    teloxide::repl(bot, move |bot: Bot, msg: Message| {
        let state = Arc::clone(&state);
        async move {
            if let Some(photos) = msg.photo() {
                if let Some(photo) = photos.last() {
                    let telegram_file_id = photo.file.id.0.clone();
                    let telegram_file_unique_id = photo.file.unique_id.0.clone();

                    let already_exist = {
                        let meme_store = state.meme_store.lock().await;
                        meme_store.conains_meme(&telegram_file_unique_id)
                    };

                    if already_exist {
                        println!("\n--- [DUPLICATE MEME] ---");
                        println!(
                            "[INFO] Meme with unique ID [{}] already exists in DB",
                            telegram_file_unique_id
                        );
                        bot.send_message(
                            msg.chat.id,
                            "I already have this meme in my database! 🧠",
                        )
                        .await?;
                        return Ok(());
                    }

                    // 1. Prepare processing
                    println!("\n--- [NEW MEME] ---");
                    println!("[1/5] Received photo from user, Downloading from Ttltgram...");

                    let file = bot.get_file(photo.file.id.clone()).await?;
                    let mut image_butes: Vec<u8> = Vec::new();
                    bot.download_file(&file.path, &mut image_butes).await?;

                    println!(
                        "[2/5] Photo uploaded ({} byte). Send toв Gemini...",
                        image_butes.len()
                    );

                    // 2. detect name and ID
                    let title = msg
                        .caption()
                        .unwrap_or("no content from lazy user")
                        .to_string();

                    let id = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs();

                    let descriptions =
                        match vlm::describe_meme(&image_butes, &app_config.gemini_api_key).await {
                            Ok(desc) => desc,
                            Err(e) => {
                                eprintln!("[ERROR] Something go wrong VLM: {}", e);
                                bot.send_message(
                                    msg.chat.id,
                                    format!("[X] AI cant recognize meme: {}", e),
                                )
                                .await?;
                                return Ok(());
                            }
                        };

                    println!("[3/5] AI description:\n{}", descriptions);

                    let full_title = format!("{} | {}", title, descriptions);

                    println!("[4/5] Vec calculating by MultilingualE5Base...");

                    let mut engine = state.engine.lock().await;
                    let mut meme_store = state.meme_store.lock().await;

                    match meme_store.add_from_descriptions(
                        id,
                        full_title.clone(),
                        telegram_file_id,
                        telegram_file_unique_id,
                        descriptions,
                        &mut engine,
                    ) {
                        Ok(()) => {
                            println!("[5/5] Sucessfully saved in SQLite!");
                            bot.send_message(
                                msg.chat.id,
                                format!("[V] 📥 Your meme has been confiscated..."),
                            )
                            .await?;
                        }
                        Err(e) => {
                            eprintln!("[ERROR] DB error SQLite: {}", e);
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
                    match comand::Command::parse(text, "") {
                        Ok(comand::Command::Start) => {
                            comand::start(bot, msg).await?;
                        }
                        Ok(comand::Command::Help) => {
                            comand::help(bot, msg).await?;
                        }
                        Ok(comand::Command::Stats) => {
                            comand::stats(bot, msg, state).await?;
                        }
                        Ok(comand::Command::Random) => {
                            comand::random(bot, msg, state).await?;
                        }
                        Err(_) => {
                            comand::error(bot, msg).await?;
                        }
                    }
                    return Ok(());
                }

                bot.send_message(msg.chat.id, "Process search...").await?;

                println!("\n---- [SEARCH] ----");
                println!("[1/4] User query: [{}]", text);

                let mut engine = state.engine.lock().await;
                let meme_store = state.meme_store.lock().await;

                println!(
                    "[2/4] Searching top-{} in DB ({} memes total)...",
                    app_config.top_k,
                    meme_store.len()
                );

                match meme_store.search_top_k(text, &mut engine, app_config.top_k) {
                    Ok(results) if results.is_empty() => {
                        println!("[3/4] No memes found matching query.");
                        bot.send_message(msg.chat.id, "There is nothing here...")
                            .await?;
                    }
                    Ok(results) => {
                        println!("[3/4] Found {} matching meme(s):", results.len());

                        for (meme, score) in &results {
                            println!(
                                "  -> ID: [{}] | Raw Score: [{:.4}] | Title [{:.50}]...",
                                meme.id, score, meme.title
                            )
                        }

                        let valid_memes: Vec<(&db::Meme, f32)> = results
                            .into_iter()
                            .filter(|(meme, _)| !meme.file_tg_id.is_empty())
                            .collect();

                        if valid_memes.is_empty() {
                            bot.send_message(msg.chat.id, "There is nothing useful here...")
                                .await?;
                        } else if valid_memes.len() == 1 {
                            let (single_meme, single_score) = valid_memes[0];

                            println!("[4/4] Sending 1 photo to Telegram...");

                            if let Err(e) = bot
                                .send_photo(
                                    msg.chat.id,
                                    InputFile::file_id(FileId(single_meme.file_tg_id.clone())),
                                )
                                .caption(format!(
                                    "📊 Match: {:.3} %",
                                    score_calibration(single_score)
                                ))
                                .await
                            {
                                eprintln!("[ERROR] Telegram send_photo failed: {:?}", e);
                                bot.send_message(
                                    msg.chat.id,
                                    format!("Failed to send photo: {}", e),
                                )
                                .await?;
                            };
                        } else {
                            println!(
                                "[4/4] Sending media group album with {} photos to Telegram...",
                                valid_memes.len()
                            );

                            let media_group: Vec<InputMedia> = valid_memes
                                .iter()
                                .map(|(meme, score)| {
                                    InputMedia::Photo(
                                        InputMediaPhoto::new(InputFile::file_id(FileId(
                                            meme.file_tg_id.clone(),
                                        )))
                                        .caption(
                                            format!("📊 Match: {:.3} %", score_calibration(*score)),
                                        ),
                                    )
                                })
                                .collect();

                            if let Err(e) = bot.send_media_group(msg.chat.id, media_group).await {
                                eprintln!("[ERROR] Telegram send_media_group failed: {:?}", e);
                                let _ = bot.send_message(
                                    msg.chat.id,
                                    format!("failed to send media group: {}", e),
                                );
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("[ERROR] Search error: {}", e);
                        bot.send_message(msg.chat.id, format!("Dead inside: {}", e))
                            .await?;
                    }
                }
            }
            Ok(())
        }
    })
    .await;
}
