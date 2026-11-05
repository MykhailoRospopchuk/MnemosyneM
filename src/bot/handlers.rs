use crate::AppState;
use crate::config::AppConfig;
use crate::worker::MemeJob;
use std::sync::Arc;
use teloxide::prelude::*;
use teloxide::types::{
    FileId, InlineQueryResult, InlineQueryResultCachedPhoto, InputFile, InputMedia,
    InputMediaPhoto, Message, PhotoSize,
};
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
        bot.send_message(
            msg.chat.id,
            "[Ї] I already have this meme in my database! 🧠",
        )
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
        user_id: msg.chat.id.0,
    };

    if let Err(e) = sender.send(job).await {
        eprintln!("[ERROR] Queue error: {}", e);
        bot.send_message(msg.chat.id, "Queue is full, please try again later!")
            .await?;
        return Ok(());
    }

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
    println!("\n---- [SEARCH] ---- User query: [{}]", text);

    let mut engine = state.engine.lock().await;
    let meme_store = state.meme_store.lock().await;

    match meme_store.search_top_k(text, &mut engine, app_config.top_k, msg.chat.id.0) {
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
                    .caption(format!("📊 Match: {:.3} %", single_score))
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
                            .caption(format!("📊 Match: {:.3}", score)),
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

pub async fn handle_inline_query(
    bot: Bot,
    query: InlineQuery,
    state: Arc<AppState>,
    app_config: &'static AppConfig,
) -> ResponseResult<()> {
    let user_id = query.from.id.0 as i64;
    let query_text = query.query.trim();

    if query_text.is_empty() {
        let meme_store = state.meme_store.lock().await;
        let recent_memes = meme_store.get_recent_memes(user_id, 5);

        let result: Vec<InlineQueryResult> = recent_memes
            .into_iter()
            .enumerate()
            .filter(|(_, meme)| !meme.file_tg_id.is_empty())
            .map(|(idx, meme)| {
                InlineQueryResult::CachedPhoto(InlineQueryResultCachedPhoto::new(
                    format!("{}_{}", meme.id, idx),
                    FileId(meme.file_tg_id.clone()),
                ))
            })
            .collect();

        if let Err(e) = bot
            .answer_inline_query(query.id, result)
            .is_personal(true)
            .cache_time(5)
            .await
        {
            eprintln!("[ERROR] Inline search for empty query error: {}", e);
        }
        return Ok(());
    }

    if query_text.chars().count() < 4 {
        if let Err(e) = bot
            .answer_inline_query(query.id.clone(), vec![])
            .is_personal(true)
            .cache_time(5)
            .await
        {
            eprintln!("[ERROR] Inline search fro short query error: {}", e);
        }
        return Ok(());
    }

    {
        let mut last_queries = state.last_inline_query.lock().await;
        last_queries.insert(user_id, query.id.0.clone());
    }

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    {
        let last_queries = state.last_inline_query.lock().await;
        if last_queries.get(&user_id) != Some(&query.id.0) {
            return Ok(());
        }
    }

    println!("\n---- [INLINE_SEARCH] ---- User query: [{}]", query_text);

    let mut engine = state.engine.lock().await;
    let meme_store = state.meme_store.lock().await;

    let results = match meme_store.search_top_k(query_text, &mut engine, app_config.top_k, user_id)
    {
        Ok(matches) => matches
            .into_iter()
            .enumerate()
            .filter(|(_, (meme, _))| !meme.file_tg_id.is_empty())
            .map(|(idx, (meme, score))| {
                InlineQueryResult::CachedPhoto(
                    InlineQueryResultCachedPhoto::new(
                        format!("{}_{}", meme.id, idx),
                        FileId(meme.file_tg_id.clone()),
                    )
                    .caption(format!("📊 Match: {:.3}", score)),
                )
            })
            .collect(),
        Err(e) => {
            eprintln!("[ERROR] Inline search error: {}", e);
            vec![]
        }
    };

    if let Err(e) = bot
        .answer_inline_query(query.id, results)
        .is_personal(true)
        .cache_time(30)
        .await
    {
        eprintln!("[WARN] Failed to answer inline query: {}", e);
    }

    Ok(())
}
