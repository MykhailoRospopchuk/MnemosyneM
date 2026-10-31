use crate::AppState;
use crate::config::AppConfig;
use crate::worker::MemeJob;
use m_nemosyne_m::score_calibration;
use std::sync::Arc;
use teloxide::prelude::*;
use teloxide::types::{FileId, InputFile, InputMedia, InputMediaPhoto, Message, PhotoSize};
use tokio::sync::mpsc::Sender;

pub async fn handle_photo(
    bot: &Bot,
    msg: &Message,
    photo: &PhotoSize,
    state: Arc<AppState>,
    sender: &Sender<MemeJob>,
) -> ResponseResult<()> {
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
        bot.send_message(msg.chat.id, "I already have this meme in my database! 🧠")
            .await?;
        return Ok(());
    }

    let job = MemeJob {
        chat_id: msg.chat.id,
        file_id: telegram_file_id,
        file_unique_id: telegram_file_unique_id,
        captions: msg
            .caption()
            .unwrap_or("no content from lazy user")
            .to_string(),
        retry_count: 0,
    };

    if let Err(e) = sender.send(job).await {
        eprintln!("[ERROR] Queue error: {}", e);
        bot.send_message(msg.chat.id, "Queue is full, please try again later!")
            .await?;
        return Ok(());
    }

    bot.send_message(msg.chat.id, "📥 Added to processing queue...")
        .await?;
    Ok(())
}

pub async fn handle_search(
    bot: &Bot,
    msg: &Message,
    text: &str,
    state: &Arc<AppState>,
    app_config: &'static AppConfig,
) -> ResponseResult<()> {
    bot.send_message(msg.chat.id, "Process search...").await?;
    println!("\n---- [SEARCH] ----");
    println!("[1/4] User query: [{}]", text);

    let mut engine = state.engine.lock().await;
    let meme_store = state.meme_store.lock().await;

    match meme_store.search_top_k(text, &mut engine, app_config.top_k) {
        Ok(results) if results.is_empty() => {
            bot.send_message(msg.chat.id, "There is nothing here...")
                .await?;
        }
        Ok(results) => {
            let valid_memes: Vec<(&crate::db::Meme, f32)> = results
                .into_iter()
                .filter(|(meme, _)| !meme.file_tg_id.is_empty())
                .collect();

            if valid_memes.is_empty() {
                bot.send_message(msg.chat.id, "There is nothing useful here...")
                    .await?;
            } else if valid_memes.len() == 1 {
                let (single_meme, single_score) = valid_memes[0];

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
                    bot.send_message(msg.chat.id, format!("Send error: {}", e))
                        .await?;
                }
            } else {
                let media_group: Vec<InputMedia> = valid_memes
                    .iter()
                    .map(|(meme, score)| {
                        InputMedia::Photo(
                            InputMediaPhoto::new(InputFile::file_id(FileId(
                                meme.file_tg_id.clone(),
                            )))
                            .caption(format!("📊 Match: {:.3} %", score_calibration(*score))),
                        )
                    })
                    .collect();

                if let Err(e) = bot.send_media_group(msg.chat.id, media_group).await {
                    eprintln!("[ERROR] Telegram send_media_group failed: {:?}", e);
                    bot.send_message(msg.chat.id, format!("Send album error: {}", e))
                        .await?;
                }
            }
        }
        Err(e) => {
            eprintln!("[ERROR] Search error: {}", e);
            bot.send_message(msg.chat.id, format!("Dead inside: {}", e))
                .await?;
        }
    }
    Ok(())
}
