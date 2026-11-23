use crate::AppState;
use crate::config::AppConfig;
use crate::database::entity::media_type::MediaType;
use crate::database::entity::meme::Meme;
use crate::database::enums::book_mark_status_enum::BookMarkStatus;
use crate::helpers::searsh_query_helper::parse_media_filter;
use crate::reranker::filter_reranked;
use crate::worker::MemeJob;
use std::sync::Arc;
use std::time::Instant;
use teloxide::prelude::*;
use teloxide::types::{
    Animation, FileId, InlineQueryResult, InlineQueryResultCachedMpeg4Gif,
    InlineQueryResultCachedPhoto, InputFile, InputMedia, InputMediaPhoto, Message, MessageOrigin,
    PhotoSize, ReplyParameters,
};
use tokio::sync::mpsc::Sender;
use tracing::{error, info, warn};

struct MediaPayload {
    file_id: String,
    file_unique_id: String,
    media_type: MediaType,
    thumbnail_file_id: Option<String>,
}

async fn enqueue_meme_job(
    bot: &Bot,
    msg: &Message,
    payload: MediaPayload,
    state: Arc<AppState>,
    sender: &Sender<MemeJob>,
) -> ResponseResult<()> {
    let user_id = msg.chat.id.0;
    let username_opt = msg.from.as_ref().and_then(|u| u.username.clone());
    let username = username_opt.as_deref().unwrap_or("-");
    let caption = msg.caption().unwrap_or("").to_string();

    let label = match payload.media_type {
        MediaType::Photo => "meme",
        MediaType::Animation => "GIF",
    };

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
        meme_store.check_or_add_bookmark(&payload.file_unique_id, user_id, &caption)
    };

    match check_status {
        Ok(BookMarkStatus::AlreadyInCollection) => {
            bot.send_message(
                msg.chat.id,
                format!("[V] This {} already in your collection!", label),
            )
            .reply_parameters(ReplyParameters::new(msg.id).allow_sending_without_reply())
            .await?;
            return Ok(());
        }
        Ok(BookMarkStatus::AddedInstantly) => {
            bot.send_message(
                msg.chat.id,
                format!("[V] We know your {}! Collected!", label),
            )
            .reply_parameters(ReplyParameters::new(msg.id).allow_sending_without_reply())
            .await?;
            return Ok(());
        }
        Ok(BookMarkStatus::NewMemeNeeded) => {
            info!(
                user_id,
                username,
                chat_id = msg.chat.id.0,
                media = label,
                file_unique_id = %payload.file_unique_id,
                "New media received for indexing"
            );
        }
        Err(e) => {
            error!(
                user_id,
                username,
                media = label,
                error = %e,
                "Bookmark check failed"
            );
        }
    };

    let job = MemeJob {
        chat_id: msg.chat.id,
        message_id: msg.id,
        file_id: payload.file_id,
        file_unique_id: payload.file_unique_id,
        captions: caption,
        source_title,
        source_url,
        retry_count: 0,
        user_id,
        username: username_opt.clone(),
        media_type: payload.media_type,
        thumbnail_file_id: payload.thumbnail_file_id,
    };

    if let Err(e) = sender.send(job).await {
        error!(
            user_id,
            username,
            media = label,
            error = %e,
            "Failed to enqueue media job"
        );
        bot.send_message(msg.chat.id, "Queue is full, please try again later!")
            .reply_parameters(ReplyParameters::new(msg.id).allow_sending_without_reply())
            .await?;
        return Ok(());
    }

    Ok(())
}

pub async fn handle_photo(
    bot: &Bot,
    msg: &Message,
    photo: &PhotoSize,
    state: Arc<AppState>,
    sender: &Sender<MemeJob>,
) -> ResponseResult<()> {
    enqueue_meme_job(
        bot,
        msg,
        MediaPayload {
            file_id: photo.file.id.0.clone(),
            file_unique_id: photo.file.unique_id.0.clone(),
            media_type: MediaType::Photo,
            thumbnail_file_id: None,
        },
        state,
        sender,
    )
    .await
}

pub async fn handle_animation(
    bot: &Bot,
    msg: &Message,
    animation: &Animation,
    state: Arc<AppState>,
    sender: &Sender<MemeJob>,
) -> ResponseResult<()> {
    let thumbnail_file_id = animation.thumbnail.as_ref().map(|t| t.file.id.0.clone());
    enqueue_meme_job(
        bot,
        msg,
        MediaPayload {
            file_id: animation.file.id.0.clone(),
            file_unique_id: animation.file.unique_id.0.clone(),
            media_type: MediaType::Animation,
            thumbnail_file_id,
        },
        state,
        sender,
    )
    .await
}

async fn run_search(
    state: &Arc<AppState>,
    app_config: &AppConfig,
    clean_query: &str,
    user_id: i64,
    media_filter: Option<MediaType>,
    candidate_limit: usize,
    use_reranker: bool,
) -> Result<Vec<(Meme, f32)>, String> {
    let reranker = if use_reranker {
        state.reranker.as_ref().map(Arc::clone)
    } else {
        None
    };

    let rerank_permit = match reranker.as_ref() {
        Some(_) => match state.rerank_semaphore.clone().try_acquire_owned() {
            Ok(permit) => Some(permit),
            Err(_) => {
                warn!(
                    user_id,
                    "Reranker queue busy; falling back to fast Phase 1A search"
                );
                None
            }
        },
        None => None,
    };

    let (Some(reranker), Some(_permit)) = (reranker, rerank_permit) else {
        let mut engine = state.engine.lock().await;
        let meme_store = state.meme_store.lock().await;
        let results = meme_store
            .search_top_k(clean_query, &mut engine, app_config, user_id, media_filter)?
            .into_iter()
            .map(|(meme, score)| (meme.clone(), score))
            .collect();
        return Ok(results);
    };

    let candidates = {
        let mut engine = state.engine.lock().await;
        let meme_store = state.meme_store.lock().await;
        meme_store.search_stage1_candidates(
            clean_query,
            &mut engine,
            app_config,
            user_id,
            media_filter,
            candidate_limit,
        )?
    };

    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let query_owned = clean_query.to_string();
    let passages: Vec<String> = candidates.iter().map(|c| c.passage.clone()).collect();
    let passage_count = passages.len();

    let t0 = Instant::now();
    let ranked = tokio::task::spawn_blocking(move || {
        let mut guard = reranker
            .lock()
            .map_err(|_| "Reranker lock poisoned".to_string())?;
        guard.rerank(&query_owned, &passages)
    })
    .await
    .map_err(|e| format!("Reranker task panicked: {}", e))??;

    let top3_debug: Vec<String> = ranked
        .iter()
        .take(3)
        .filter_map(|&(idx, score)| {
            candidates
                .get(idx)
                .map(|c| format!("id={}({:.4})", c.meme.id, score))
        })
        .collect();
    info!(
        user_id,
        candidates = passage_count,
        latency_ms = t0.elapsed().as_millis(),
        top3 = %top3_debug.join(", "),
        "Stage-2 rerank completed"
    );

    let filtered = filter_reranked(ranked, app_config.rerank_min_score, app_config.top_k);
    let results = filtered
        .into_iter()
        .filter_map(|(idx, score)| candidates.get(idx).map(|c| (c.meme.clone(), score)))
        .collect();

    Ok(results)
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

    let user_id = msg.chat.id.0;
    let username = msg
        .from
        .as_ref()
        .and_then(|u| u.username.as_deref())
        .unwrap_or("-");
    let t0 = Instant::now();

    bot.send_message(msg.chat.id, "Process search...").await?;

    match run_search(
        state,
        app_config,
        &clean_query,
        user_id,
        media_filter,
        app_config.rerank_candidates,
        true,
    )
    .await
    {
        Ok(results) if results.is_empty() => {
            info!(
                user_id,
                username,
                query = %clean_query,
                ?media_filter,
                results = 0usize,
                latency_ms = t0.elapsed().as_millis(),
                "Search completed"
            );
            bot.send_message(msg.chat.id, "There is nothing here...")
                .await?;
        }
        Ok(results) => {
            let valid_memes: Vec<(Meme, f32)> = results
                .into_iter()
                .filter(|(meme, _)| !meme.file_tg_id.is_empty())
                .collect();
            let results_count = valid_memes.len();
            info!(
                user_id,
                username,
                query = %clean_query,
                ?media_filter,
                results = results_count,
                latency_ms = t0.elapsed().as_millis(),
                "Search completed"
            );

            if valid_memes.is_empty() {
                bot.send_message(msg.chat.id, "There is nothing useful here...")
                    .await?;
            } else {
                let mut photo_items: Vec<(String, String)> = Vec::new();

                for (media, score) in valid_memes {
                    let caption = build_caption(&media.source_title, &media.source_url, &score);
                    match media.media_type {
                        MediaType::Animation => {
                            if let Err(e) = bot
                                .send_animation(
                                    msg.chat.id,
                                    InputFile::file_id(FileId(media.file_tg_id.clone())),
                                )
                                .caption(caption)
                                .await
                            {
                                error!(
                                    user_id,
                                    username,
                                    error = ?e,
                                    "Telegram send_animation failed"
                                );
                                bot.send_message(
                                    msg.chat.id,
                                    format!("Send animation error: {}", e),
                                )
                                .await?;
                            }
                        }
                        MediaType::Photo => {
                            photo_items.push((media.file_tg_id.clone(), caption));
                        }
                    }
                }

                match photo_items.len() {
                    0 => {}
                    1 => {
                        let (file_id, caption) = photo_items.remove(0);
                        if let Err(e) = bot
                            .send_photo(msg.chat.id, InputFile::file_id(FileId(file_id)))
                            .caption(caption)
                            .await
                        {
                            error!(
                                user_id,
                                username,
                                error = ?e,
                                "Telegram send_photo failed"
                            );
                            bot.send_message(msg.chat.id, format!("Send photo error: {}", e))
                                .await?;
                        }
                    }
                    _ => {
                        let result_media_group: Vec<InputMedia> = photo_items
                            .into_iter()
                            .map(|(file_id, caption)| {
                                InputMedia::Photo(
                                    InputMediaPhoto::new(InputFile::file_id(FileId(file_id)))
                                        .caption(caption),
                                )
                            })
                            .collect();
                        if let Err(e) = bot.send_media_group(msg.chat.id, result_media_group).await
                        {
                            error!(
                                user_id,
                                username,
                                error = ?e,
                                "Telegram send_media_group failed"
                            );
                            bot.send_message(msg.chat.id, format!("Send album error: {}", e))
                                .await?;
                        }
                    }
                }
            }
        }
        Err(e) => {
            error!(
                user_id,
                username,
                query = %clean_query,
                error = %e,
                "Search failed"
            );
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
    let username = query.from.username.as_deref().unwrap_or("-");
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
        let recent_memes = meme_store.get_recent_memes(user_id, app_config.top_k, media_filter);

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
            error!(
                user_id,
                username,
                error = %e,
                "Inline search for empty query failed"
            );
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
            error!(
                user_id,
                username,
                error = %e,
                "Inline search for short query failed"
            );
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

    let t0 = Instant::now();
    let results = match run_search(
        &state,
        app_config,
        &clean_query,
        user_id,
        media_filter,
        app_config.rerank_candidates.min(8),
        app_config.rerank_inline,
    )
    .await
    {
        Ok(matches) => {
            let items: Vec<InlineQueryResult> = matches
                .into_iter()
                .enumerate()
                .filter(|(_, (meme, _))| !meme.file_tg_id.is_empty())
                .map(|(idx, (meme, score))| make_inline_result(idx, &meme, score))
                .collect();
            info!(
                user_id,
                username,
                query = %clean_query,
                ?media_filter,
                results = items.len(),
                latency_ms = t0.elapsed().as_millis(),
                "Inline search completed"
            );
            items
        }
        Err(e) => {
            error!(
                user_id,
                username,
                query = %clean_query,
                error = %e,
                "Inline search failed"
            );
            vec![]
        }
    };

    if let Err(e) = bot
        .answer_inline_query(query.id, results)
        .is_personal(true)
        .cache_time(30)
        .await
    {
        warn!(
            user_id,
            username,
            error = %e,
            "Failed to answer inline query"
        );
    }

    Ok(())
}

pub fn build_caption(source_title: &str, source_url: &str, score: &f32) -> String {
    if !source_url.is_empty() {
        format!(
            "📊 Match: {:.1}%\n => Source: [{}] => ({})",
            score * 100.0,
            source_title,
            source_url
        )
    } else {
        format!("📊 Match: {:.1}%", score * 100.0)
    }
}
