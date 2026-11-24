use crate::AppState;
use crate::config::AppConfig;
use crate::database::entity::media_type::MediaType;
use crate::database::entity::meme::Meme;
use crate::database::enums::book_mark_status_enum::BookMarkStatus;
use crate::helpers::searsh_query_helper::parse_search_query;
use crate::meme_store::SearchFilter;
use crate::reranker::filter_reranked;
use crate::worker::MemeJob;
use std::sync::Arc;
use std::time::Instant;
use teloxide::prelude::*;
use teloxide::types::{
    Animation, FileId, InlineQueryResult, InlineQueryResultCachedMpeg4Gif,
    InlineQueryResultCachedPhoto, InputFile, InputMedia, InputMediaPhoto, Message, MessageOrigin,
    ParseMode, PhotoSize, ReplyParameters,
};
use tokio::sync::mpsc::Sender;
use tracing::{error, info, warn};

const INLINE_PAGE_SIZE: usize = 25;

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
    filter: SearchFilter<'_>,
    candidate_limit: usize,
    use_reranker: bool,
) -> Result<Vec<(Meme, f32)>, String> {
    let t_total = Instant::now();
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

    let t_embed = Instant::now();
    let query_vec = {
        let mut engine = state.engine.lock().await;
        engine.embed_query(clean_query).map_err(|e| e.to_string())?
    };
    let embed_ms = t_embed.elapsed().as_millis() as u64;

    let (Some(reranker), Some(_permit)) = (reranker, rerank_permit) else {
        let t_stage1 = Instant::now();
        let results = {
            let meme_store = state.meme_store.lock().await;
            let settings = meme_store.get_user_settings(user_id);
            meme_store
                .find_top_k(
                    &query_vec,
                    clean_query,
                    app_config,
                    user_id,
                    settings.search_scope,
                    filter,
                )
                .into_iter()
                .map(|(meme, score)| (meme.clone(), score))
                .collect()
        };
        let stage1_ms = t_stage1.elapsed().as_millis() as u64;
        state
            .search_metrics
            .record(embed_ms, stage1_ms, 0, t_total.elapsed().as_millis() as u64);
        return Ok(results);
    };

    let t_stage1 = Instant::now();
    let candidates = {
        let meme_store = state.meme_store.lock().await;
        meme_store.stage1_candidates_from_embedding(
            &query_vec,
            clean_query,
            app_config,
            user_id,
            filter,
            candidate_limit,
        )
    };
    let stage1_ms = t_stage1.elapsed().as_millis() as u64;

    if candidates.is_empty() {
        state
            .search_metrics
            .record(embed_ms, stage1_ms, 0, t_total.elapsed().as_millis() as u64);
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
    let rerank_ms = t0.elapsed().as_millis() as u64;

    state.search_metrics.record(
        embed_ms,
        stage1_ms,
        rerank_ms,
        t_total.elapsed().as_millis() as u64,
    );

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
        embed_ms,
        stage1_ms,
        rerank_ms,
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

async fn send_search_results(
    bot: &Bot,
    msg: &Message,
    valid_memes: Vec<(Meme, f32)>,
    show_score: bool,
    user_id: i64,
    username: &str,
) -> ResponseResult<()> {
    let mut photo_items: Vec<(String, String)> = Vec::new();

    for (media, score) in valid_memes {
        let caption = build_caption(&media, show_score.then_some(score));
        match media.media_type {
            MediaType::Animation => {
                let mut req = bot.send_animation(
                    msg.chat.id,
                    InputFile::file_id(FileId(media.file_tg_id.clone())),
                );
                if !caption.is_empty() {
                    req = req.caption(caption).parse_mode(ParseMode::Html);
                }
                if let Err(e) = req.await {
                    error!(
                        user_id,
                        username,
                        error = ?e,
                        "Telegram send_animation failed"
                    );
                    bot.send_message(msg.chat.id, format!("Send animation error: {}", e))
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
            let mut req = bot.send_photo(msg.chat.id, InputFile::file_id(FileId(file_id)));
            if !caption.is_empty() {
                req = req.caption(caption).parse_mode(ParseMode::Html);
            }
            if let Err(e) = req.await {
                error!(user_id, username, error = ?e, "Telegram send_photo failed");
                bot.send_message(msg.chat.id, format!("Send photo error: {}", e))
                    .await?;
            }
        }
        _ => {
            let result_media_group: Vec<InputMedia> = photo_items
                .into_iter()
                .map(|(file_id, caption)| {
                    let mut photo = InputMediaPhoto::new(InputFile::file_id(FileId(file_id)));
                    if !caption.is_empty() {
                        photo = photo.caption(caption).parse_mode(ParseMode::Html);
                    }
                    InputMedia::Photo(photo)
                })
                .collect();
            if let Err(e) = bot.send_media_group(msg.chat.id, result_media_group).await {
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

    Ok(())
}

pub async fn handle_search(
    bot: &Bot,
    msg: &Message,
    text: &str,
    state: &Arc<AppState>,
    app_config: &'static AppConfig,
) -> ResponseResult<()> {
    let parsed = parse_search_query(text);
    let user_id = msg.chat.id.0;
    let username = msg
        .from
        .as_ref()
        .and_then(|u| u.username.as_deref())
        .unwrap_or("-");

    if parsed.clean_query.is_empty() {
        if !parsed.tag_filters.is_empty() {
            let tagged_memes: Vec<(Meme, f32)> = {
                let store = state.meme_store.lock().await;
                store
                    .get_memes_by_tags(
                        user_id,
                        &parsed.tag_filters,
                        app_config.top_k,
                        parsed.media_filter,
                    )
                    .into_iter()
                    .map(|m| (m.clone(), 1.0))
                    .collect()
            };

            info!(
                user_id,
                username,
                tag_filters = ?parsed.tag_filters,
                media_filter = ?parsed.media_filter,
                results = tagged_memes.len(),
                "Tag browse search executed"
            );

            if tagged_memes.is_empty() {
                let tags_label = parsed
                    .tag_filters
                    .iter()
                    .map(|t| format!("#{}", t))
                    .collect::<Vec<_>>()
                    .join(" ");
                bot.send_message(
                    msg.chat.id,
                    format!("За тегами {} поки нічого не знайдено.", tags_label),
                )
                .await?;
            } else {
                send_search_results(bot, msg, tagged_memes, false, user_id, username).await?;
            }
            return Ok(());
        }

        bot.send_message(
            msg.chat.id,
            "User ..!gif... or ...!img... and then your search",
        )
        .await?;
        return Ok(());
    }

    let t0 = Instant::now();
    bot.send_message(msg.chat.id, "Process search...").await?;

    let filter = SearchFilter {
        media: parsed.media_filter,
        tags: &parsed.tag_filters,
    };

    match run_search(
        state,
        app_config,
        &parsed.clean_query,
        user_id,
        filter,
        app_config.rerank_candidates,
        true,
    )
    .await
    {
        Ok(results) if results.is_empty() => {
            info!(
                user_id,
                username,
                query = %parsed.clean_query,
                media_filter = ?parsed.media_filter,
                tag_filters = ?parsed.tag_filters,
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
                query = %parsed.clean_query,
                media_filter = ?parsed.media_filter,
                tag_filters = ?parsed.tag_filters,
                results = results_count,
                latency_ms = t0.elapsed().as_millis(),
                "Search completed"
            );

            if valid_memes.is_empty() {
                bot.send_message(msg.chat.id, "There is nothing useful here...")
                    .await?;
            } else {
                send_search_results(bot, msg, valid_memes, true, user_id, username).await?;
            }
        }
        Err(e) => {
            error!(
                user_id,
                username,
                query = %parsed.clean_query,
                tag_filters = ?parsed.tag_filters,
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
    let offset: usize = query.offset.trim().parse().unwrap_or(0);
    let parsed = parse_search_query(&query.query);

    let make_inline_result =
        |abs_idx: usize, meme: &Meme, score: Option<f32>| -> InlineQueryResult {
            let caption = build_caption(meme, score);
            let result_id = format!("{}_{}", meme.id, abs_idx);

            match meme.media_type {
                MediaType::Animation => {
                    let mut item = InlineQueryResultCachedMpeg4Gif::new(
                        result_id,
                        FileId(meme.file_tg_id.clone()),
                    );
                    if !caption.is_empty() {
                        item = item.caption(caption).parse_mode(ParseMode::Html);
                    }
                    InlineQueryResult::CachedMpeg4Gif(item)
                }
                MediaType::Photo => {
                    let mut item =
                        InlineQueryResultCachedPhoto::new(result_id, FileId(meme.file_tg_id.clone()));
                    if !caption.is_empty() {
                        item = item.caption(caption).parse_mode(ParseMode::Html);
                    }
                    InlineQueryResult::CachedPhoto(item)
                }
            }
        };

    if parsed.clean_query.is_empty() {
        let (result, has_more) = {
            let meme_store = state.meme_store.lock().await;
            let (memes, has_more) = if !parsed.tag_filters.is_empty() {
                meme_store.get_memes_by_tags_page(
                    user_id,
                    &parsed.tag_filters,
                    offset,
                    INLINE_PAGE_SIZE,
                    parsed.media_filter,
                )
            } else {
                meme_store.get_recent_memes_page(
                    user_id,
                    offset,
                    INLINE_PAGE_SIZE,
                    parsed.media_filter,
                )
            };

            let items: Vec<InlineQueryResult> = memes
                .into_iter()
                .enumerate()
                .map(|(i, meme)| make_inline_result(offset + i, meme, None))
                .collect();
            (items, has_more)
        };

        let next_offset = if has_more && !result.is_empty() {
            (offset + result.len()).to_string()
        } else {
            String::new()
        };

        if let Err(e) = bot
            .answer_inline_query(query.id, result)
            .is_personal(true)
            .cache_time(5)
            .next_offset(next_offset)
            .await
        {
            error!(
                user_id,
                username,
                tag_filters = ?parsed.tag_filters,
                offset,
                error = %e,
                "Inline search for empty/tag query failed"
            );
        }
        return Ok(());
    }

    if parsed.clean_query.chars().count() < 4 {
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

    if offset == 0 {
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
    }

    let t0 = Instant::now();
    let filter = SearchFilter {
        media: parsed.media_filter,
        tags: &parsed.tag_filters,
    };
    let (results, next_offset) = match run_search(
        &state,
        app_config,
        &parsed.clean_query,
        user_id,
        filter,
        app_config.rerank_candidates.min(8),
        app_config.rerank_inline,
    )
    .await
    {
        Ok(matches) => {
            let mut valid: Vec<(usize, InlineQueryResult)> = matches
                .into_iter()
                .filter(|(meme, _)| !meme.file_tg_id.is_empty())
                .skip(offset)
                .take(INLINE_PAGE_SIZE + 1)
                .enumerate()
                .map(|(i, (meme, score))| {
                    let abs_idx = offset + i;
                    (abs_idx, make_inline_result(abs_idx, &meme, Some(score)))
                })
                .collect();

            let has_more = valid.len() > INLINE_PAGE_SIZE;
            valid.truncate(INLINE_PAGE_SIZE);
            let items: Vec<InlineQueryResult> = valid.into_iter().map(|(_, item)| item).collect();
            let next_off = if has_more && !items.is_empty() {
                (offset + items.len()).to_string()
            } else {
                String::new()
            };

            info!(
                user_id,
                username,
                query = %parsed.clean_query,
                media_filter = ?parsed.media_filter,
                tag_filters = ?parsed.tag_filters,
                offset,
                results = items.len(),
                latency_ms = t0.elapsed().as_millis(),
                "Inline search completed"
            );
            (items, next_off)
        }
        Err(e) => {
            error!(
                user_id,
                username,
                query = %parsed.clean_query,
                tag_filters = ?parsed.tag_filters,
                error = %e,
                "Inline search failed"
            );
            (vec![], String::new())
        }
    };

    if let Err(e) = bot
        .answer_inline_query(query.id, results)
        .is_personal(true)
        .cache_time(30)
        .next_offset(next_offset)
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

pub fn build_caption(meme: &Meme, score: Option<f32>) -> String {
    let source_html = crate::bot::tinder::format_source_html(meme);
    match score {
        Some(s) => format!("📊 {:.0}%{}", (s * 100.0).clamp(0.0, 100.0), source_html),
        None => source_html
            .strip_prefix(" · ")
            .unwrap_or(&source_html)
            .to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_meme(source_title: &str, source_url: &str) -> Meme {
        Meme {
            id: 1,
            title: String::new(),
            ocr_text: String::new(),
            embedding_uk: Vec::new(),
            embedding_en: Vec::new(),
            file_tg_id: "tg1".to_string(),
            file_tg_unique_id: "u1".to_string(),
            source_title: source_title.to_string(),
            source_url: source_url.to_string(),
            is_public: true,
            owners: Vec::new(),
            phash: 0,
            media_type: MediaType::Photo,
            thumbnail_tg_id: String::new(),
            tags: Vec::new(),
        }
    }

    #[test]
    fn build_caption_renders_compact_html_for_search_and_tag_browse() {
        let with_source = sample_meme("WTF & IT", "https://t.me/WTF_UKR/42");
        assert_eq!(
            build_caption(&with_source, Some(0.923)),
            "📊 92% · 📎 <a href=\"https://t.me/WTF_UKR/42\">WTF &amp; IT</a>"
        );
        assert_eq!(
            build_caption(&with_source, None),
            "📎 <a href=\"https://t.me/WTF_UKR/42\">WTF &amp; IT</a>"
        );

        let no_source = sample_meme("", "");
        assert_eq!(build_caption(&no_source, Some(0.87)), "📊 87%");
        assert_eq!(build_caption(&no_source, None), "");
    }
}
