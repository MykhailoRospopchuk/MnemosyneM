use crate::AppState;
use crate::config::AppConfig;
use crate::database::entity::media_type::MediaType;
use crate::db::{BookMarkStatus, Meme};
use crate::helpers::searsh_query_helper::parse_media_filter;
use crate::worker::MemeJob;
use std::sync::Arc;
use teloxide::prelude::*;
use teloxide::types::{
    Animation, FileId, InlineQueryResult, InlineQueryResultCachedMpeg4Gif,
    InlineQueryResultCachedPhoto, InputFile, InputMedia, InputMediaPhoto, Message, MessageOrigin,
    PhotoSize,
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
    let user_id = msg.chat.id.0;
    let caption = msg.caption().unwrap_or("").to_string();

    let (source_title, source_url) = match msg.forward_origin() {
        Some(MessageOrigin::Channel {
            date: _,
            chat,
            message_id,
            author_signature: _,
        }) => {
            if let Some(username) = chat.username() {
                let title = chat.title().unwrap_or(username).to_string();
                let url = format!("https://t.me/{}/{}", username, message_id);
                (title, url)
            } else {
                ("".to_string(), "".to_string())
            }
        }
        _ => ("".to_string(), "".to_string()),
    };

    let check_status = {
        let mut meme_store = state.meme_store.lock().await;
        meme_store.check_or_add_bookmark(&telegram_file_unique_id, user_id, &caption)
    };

    match check_status {
        Ok(BookMarkStatus::AlreadyInCollection) => {
            bot.send_message(msg.chat.id, "[V] This meme already in your collection!")
                .await?;
            return Ok(());
        }
        Ok(BookMarkStatus::AddetInstantly) => {
            bot.send_message(msg.chat.id, "[V] We know your meme! Collected! ")
                .await?;
            return Ok(());
        }
        Ok(BookMarkStatus::NewMemeNeeded) => {
            println!("[PHOTO-HANDLER]-[INFO] someone just drop new meme!")
        }
        Err(e) => {
            eprintln!(
                "[PHOTO-HANDLER]-[ERROR] Bookmark check error: {}",
                e.to_string()
            );
        }
    };

    let job = MemeJob {
        chat_id: msg.chat.id,
        file_id: telegram_file_id,
        file_unique_id: telegram_file_unique_id,
        captions: caption,
        source_title: source_title,
        source_url: source_url,
        retry_count: 0,
        user_id: msg.chat.id.0,
        media_type: MediaType::Photo,
        thumbnail_file_id: None,
    };

    if let Err(e) = sender.send(job).await {
        eprintln!("[PHOTO-HANDLER]-[ERROR] Queue error: {}", e);
        bot.send_message(msg.chat.id, "Queue is full, please try again later!")
            .await?;
        return Ok(());
    }

    Ok(())
}

pub async fn handle_animation(
    bot: &Bot,
    msg: &Message,
    animation: &Animation,
    state: Arc<AppState>,
    sender: &Sender<MemeJob>,
) -> ResponseResult<()> {
    let telegram_file_id = animation.file.id.0.clone();
    let telegram_file_unique_id = animation.file.unique_id.0.clone();
    let user_id = msg.chat.id.0;
    let caption = msg.caption().unwrap_or("").to_string();

    let (source_title, source_url) = match msg.forward_origin() {
        Some(MessageOrigin::Channel {
            date: _,
            chat,
            message_id,
            author_signature: _,
        }) => {
            if let Some(username) = chat.username() {
                let title = chat.title().unwrap_or(username).to_string();
                let url = format!("https://t.me/{}/{}", username, message_id);
                (title, url)
            } else {
                ("".to_string(), "".to_string())
            }
        }
        _ => ("".to_string(), "".to_string()),
    };

    let check_status = {
        let mut meme_store = state.meme_store.lock().await;
        meme_store.check_or_add_bookmark(&telegram_file_unique_id, user_id, &caption)
    };

    match check_status {
        Ok(BookMarkStatus::AlreadyInCollection) => {
            bot.send_message(msg.chat.id, "[V] This GIF already in your collection!")
                .await?;
            return Ok(());
        }
        Ok(BookMarkStatus::AddetInstantly) => {
            bot.send_message(msg.chat.id, "[V] We know your GIF! Collected! ")
                .await?;
            return Ok(());
        }
        Ok(BookMarkStatus::NewMemeNeeded) => {
            println!("[ANIMATION-HANDLER]-[INFO] someone just drop new GIF!")
        }
        Err(e) => {
            eprintln!(
                "[ANIMATION-HANDLER]-[ERROR] Bookmark check error: {}",
                e.to_string()
            );
        }
    };

    let thumbnail_file_id = animation.thumbnail.as_ref().map(|t| t.file.id.0.clone());

    let job = MemeJob {
        chat_id: msg.chat.id,
        file_id: telegram_file_id,
        file_unique_id: telegram_file_unique_id,
        captions: caption,
        source_title: source_title,
        source_url: source_url,
        retry_count: 0,
        user_id: msg.chat.id.0,
        media_type: MediaType::Animation,
        thumbnail_file_id: thumbnail_file_id,
    };

    if let Err(e) = sender.send(job).await {
        eprintln!("[ANIMATION-HANDLER]-[ERROR] Queue error: {}", e);
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
    let (media_filter, clean_query) = parse_media_filter(text);

    if clean_query.is_empty() {
        bot.send_message(
            msg.chat.id,
            "User ..!gif... or ...!img... and then your search",
        )
        .await?;
        return Ok(());
    }

    bot.send_message(msg.chat.id, "Process search...").await?;
    println!(
        "\n---- [SEARCH] ---- User query: [{}] | Media Type {:?}",
        text, media_filter
    );

    let mut engine = state.engine.lock().await;
    let meme_store = state.meme_store.lock().await;

    match meme_store.search_top_k(
        &clean_query,
        &mut engine,
        app_config.top_k,
        msg.chat.id.0,
        media_filter,
    ) {
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
            } else {
                let mut result_media_group: Vec<InputMedia> = Vec::new();

                for (media, score) in valid_memes {
                    match media.media_type {
                        MediaType::Animation => {
                            if let Err(e) = bot
                                .send_animation(
                                    msg.chat.id,
                                    InputFile::file_id(FileId(media.file_tg_id.clone())),
                                )
                                .caption(build_caption(
                                    &media.source_title,
                                    &media.source_url,
                                    &score,
                                ))
                                .await
                            {
                                eprintln!("[ERROR] Telegram send_media_group failed: {:?}", e);
                                bot.send_message(msg.chat.id, format!("Send album error: {}", e))
                                    .await?;
                            }
                        }
                        MediaType::Photo => {
                            result_media_group.push(InputMedia::Photo(
                                InputMediaPhoto::new(InputFile::file_id(FileId(
                                    media.file_tg_id.clone(),
                                )))
                                .caption(build_caption(
                                    &media.source_title,
                                    &media.source_url,
                                    &score,
                                )),
                            ));
                        }
                    }
                }

                if result_media_group.len() != 0 {
                    if let Err(e) = bot.send_media_group(msg.chat.id, result_media_group).await {
                        eprintln!("[ERROR] Telegram send_media_group failed: {:?}", e);
                        bot.send_message(msg.chat.id, format!("Send album error: {}", e))
                            .await?;
                    }
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
    let (media_filter, clean_query) = parse_media_filter(&query.query);

    let make_inline_result = |idx: usize, meme: &Meme, score: f32| -> InlineQueryResult {
        let caption = build_caption(&meme.source_title, &meme.source_url, &score);

        match meme.media_type {
            MediaType::Animation => InlineQueryResult::CachedMpeg4Gif(
                InlineQueryResultCachedMpeg4Gif::new(
                    format!("{}_{}", meme.id, idx),
                    FileId(meme.file_tg_id.clone()),
                )
                .caption(caption),
            ),
            MediaType::Photo => InlineQueryResult::CachedPhoto(
                InlineQueryResultCachedPhoto::new(
                    format!("{}_{}", meme.id, idx),
                    FileId(meme.file_tg_id.clone()),
                )
                .caption(caption),
            ),
        }
    };

    if clean_query.is_empty() {
        let meme_store = state.meme_store.lock().await;
        let recent_memes = meme_store.get_recent_memes(user_id, 5);

        let result: Vec<InlineQueryResult> = recent_memes
            .into_iter()
            .enumerate()
            .filter(|(_, meme)| !meme.file_tg_id.is_empty())
            .map(|(idx, meme)| make_inline_result(idx, meme, 1.0))
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

    if clean_query.chars().count() < 4 {
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

    println!("\n---- [INLINE_SEARCH] ---- User query: [{}]", clean_query);

    let mut engine = state.engine.lock().await;
    let meme_store = state.meme_store.lock().await;

    let results = match meme_store.search_top_k(
        &clean_query,
        &mut engine,
        app_config.top_k,
        user_id,
        media_filter,
    ) {
        Ok(matches) => matches
            .into_iter()
            .enumerate()
            .filter(|(_, (meme, _))| !meme.file_tg_id.is_empty())
            .map(|(idx, (meme, score))| make_inline_result(idx, meme, score))
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

pub fn build_caption(source_title: &str, source_url: &str, score: &f32) -> String {
    let caption = if !source_url.is_empty() {
        format!(
            "📊 Match: {:.1}%\n => Source: [{}] => ({})",
            score * 100.0,
            source_title,
            source_url
        )
    } else {
        format!("📊 Match: {:.1}%", score * 100.0)
    };

    caption
}
