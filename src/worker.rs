use std::{sync::Arc, time::Duration};

use crate::config::AppConfig;
use crate::constants;
use crate::database::entity::media_type::MediaType;
use crate::database::entity::new_meme::NewMeme;
use crate::database::enums::vlm_error_enum::VlmError;
use crate::helpers::telegram_bot_helper::download_telegram_file;
use crate::vlm::PreSafetyDecision;
use crate::{
    AppState,
    helpers::hash_helper::{compute_hash, extract_frame_from_video},
};
use teloxide::{
    Bot,
    payloads::SendMessageSetters,
    requests::Requester,
    types::{ChatId, InlineKeyboardMarkup, MessageId, ReplyParameters},
};
use tokio::sync::mpsc::{Receiver, Sender};
use tracing::{debug, error, info, warn};

#[derive(Clone, Debug)]
pub struct MemeJob {
    pub chat_id: ChatId,
    pub message_id: MessageId,
    pub file_id: String,
    pub file_unique_id: String,
    pub captions: String,
    pub retry_count: u8,
    pub user_id: i64,
    pub username: Option<String>,
    pub source_title: String,
    pub source_url: String,
    pub media_type: MediaType,
    pub thumbnail_file_id: Option<String>,
}

async fn reply_to_job(bot: &Bot, chat_id: ChatId, message_id: MessageId, text: impl Into<String>) {
    let _ = bot
        .send_message(chat_id, text)
        .reply_parameters(ReplyParameters::new(message_id).allow_sending_without_reply())
        .await;
}

async fn reply_to_job_with_markup(
    bot: &Bot,
    chat_id: ChatId,
    message_id: MessageId,
    text: impl Into<String>,
    keyboard: InlineKeyboardMarkup,
) {
    let _ = bot
        .send_message(chat_id, text)
        .reply_parameters(ReplyParameters::new(message_id).allow_sending_without_reply())
        .reply_markup(keyboard)
        .await;
}

pub fn start_worker(
    mut receiver: Receiver<MemeJob>,
    sender: Sender<MemeJob>,
    state: Arc<AppState>,
    bot: Bot,
    app_config: &'static AppConfig,
) {
    tokio::spawn(async move {
        info!("Background queue worker started");

        while let Some(job) = receiver.recv().await {
            let user_id = job.user_id;
            let username_owned = job.username.clone();
            let username = username_owned.as_deref().unwrap_or("-");
            let file_unique_id = job.file_unique_id.clone();

            info!(
                user_id,
                username,
                file_unique_id = %file_unique_id,
                queue_len = receiver.len(),
                "Processing meme ingestion job"
            );

            let image_bytes = match download_telegram_file(&bot, &job.file_id, 3).await {
                Ok(bytes) => bytes,
                Err(e) => {
                    error!(
                        user_id,
                        username,
                        file_unique_id = %file_unique_id,
                        error = %e,
                        "Failed to download file after retries"
                    );
                    reply_to_job(
                        &bot,
                        job.chat_id,
                        job.message_id,
                        format!("[X] Failed to download media from Telegram: {}", e),
                    )
                    .await;
                    continue;
                }
            };

            let (phash, animation_preview_jpeg) = match job.media_type {
                MediaType::Photo => {
                    let bytes = image_bytes.clone();
                    let h = match tokio::task::spawn_blocking(move || compute_hash(&bytes)).await {
                        Ok(Ok(phash)) => phash,
                        Ok(Err(e)) => {
                            warn!(
                                user_id,
                                username,
                                file_unique_id = %file_unique_id,
                                error = %e,
                                "Failed to compute pHash for image"
                            );
                            0
                        }
                        Err(e) => {
                            warn!(
                                user_id,
                                username,
                                file_unique_id = %file_unique_id,
                                error = %e,
                                "Blocking pHash task panicked"
                            );
                            0
                        }
                    };
                    (h, None)
                }
                MediaType::Animation => {
                    let mut frame_bytes = None;
                    if let Some(thumbnail_id) = job.thumbnail_file_id.clone() {
                        match download_telegram_file(&bot, &thumbnail_id, 3).await {
                            Ok(bytes) => frame_bytes = Some(bytes),
                            Err(e) => {
                                error!(
                                    user_id,
                                    username,
                                    file_unique_id = %file_unique_id,
                                    error = %e,
                                    "Failed to download animation thumbnail after retries"
                                );
                                reply_to_job(
                                    &bot,
                                    job.chat_id,
                                    job.message_id,
                                    format!("[X] Failed to download media from Telegram: {}", e),
                                )
                                .await;
                            }
                        };
                    }

                    let bytes_for_ffmpeg = if frame_bytes.is_none() {
                        debug!(
                            user_id,
                            username,
                            file_unique_id = %file_unique_id,
                            "Telegram provided no thumbnail; extracting first frame via ffmpeg"
                        );
                        Some(image_bytes.clone())
                    } else {
                        None
                    };

                    tokio::task::spawn_blocking(move || {
                        let jpeg_bytes = frame_bytes.or_else(|| {
                            bytes_for_ffmpeg.and_then(|b| extract_frame_from_video(&b).ok())
                        });
                        let h = jpeg_bytes
                            .as_deref()
                            .and_then(|b| compute_hash(b).ok())
                            .unwrap_or(0);
                        (h, jpeg_bytes)
                    })
                    .await
                    .unwrap_or((0, None))
                }
            };

            let mime_type = match job.media_type {
                MediaType::Animation => "video/mp4",
                MediaType::Photo => "image/jpeg",
            };

            if phash != 0 {
                let mut store = state.meme_store.lock().await;

                if let Some((exiting_meme_id, distance)) =
                    store.find_duplicate_by_phash(phash, app_config.max_phash_distance)
                {
                    info!(
                        user_id,
                        username,
                        file_unique_id = %file_unique_id,
                        meme_id = exiting_meme_id,
                        distance,
                        "Duplicate meme detected by pHash; skipping Gemini VLM"
                    );

                    match store.link_existing_meme(exiting_meme_id, job.user_id, &job.captions) {
                        Ok(is_new_for_user) => {
                            let text = if is_new_for_user {
                                "[D] This meme already persist in DB (> 92%)! Added immediattely to your collection"
                            } else {
                                "[E] Already exist in your collection"
                            };

                            reply_to_job(&bot, job.chat_id, job.message_id, text).await;
                        }
                        Err(e) => {
                            error!(
                                user_id,
                                username,
                                meme_id = exiting_meme_id,
                                error = %e,
                                "Failed to link duplicate meme"
                            );
                        }
                    }

                    drop(store);
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    continue;
                }

                drop(store);
            }

            let safety_image: Option<&[u8]> = match job.media_type {
                MediaType::Photo => Some(&image_bytes),
                MediaType::Animation => animation_preview_jpeg.as_deref(),
            };
            if let Some(safety_bytes) = safety_image
                && let PreSafetyDecision::Reject {
                    reason,
                    safety_score,
                } = state
                    .vlm_router
                    .check_pre_ingestion_safety(
                        &app_config.gemini_api_key,
                        safety_bytes,
                        "image/jpeg",
                    )
                    .await
            {
                warn!(
                    user_id,
                    username,
                    file_unique_id = %file_unique_id,
                    safety_score,
                    reason = %reason,
                    "Inappropriate meme rejected by pre-ingestion safety gate"
                );
                reply_to_job(
                    &bot,
                    job.chat_id,
                    job.message_id,
                    format!(
                        "⛔ [REJECTED] {}\nNSFW or graphic content is strictly prohibited!",
                        reason
                    ),
                )
                .await;
                tokio::time::sleep(Duration::from_millis(300)).await;
                continue;
            }

            let vlm_result = state
                .vlm_router
                .describe_meme(&image_bytes, &app_config.gemini_api_key, mime_type)
                .await;
            crate::persist_dirty_vlm_quotas(&state).await;

            let analysis = match vlm_result {
                Ok(desc) => desc,
                Err(VlmError::InappropriateContent(reason)) => {
                    warn!(
                        user_id,
                        username,
                        file_unique_id = %file_unique_id,
                        reason = %reason,
                        "Inappropriate meme rejected by safety filter"
                    );
                    reply_to_job(
                        &bot,
                        job.chat_id,
                        job.message_id,
                        format!(
                            "⛔ [REJECTED] Inappropriate content detected: {}\nNSFW content is strictly prohibited!",
                            reason
                        ),
                    )
                    .await;
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }
                Err(VlmError::DailyQuotaExhausted(msg)) => {
                    warn!(
                        user_id,
                        username,
                        file_unique_id = %file_unique_id,
                        error = %msg,
                        "All VLM models hit daily quota during ingestion"
                    );
                    reply_to_job(
                        &bot,
                        job.chat_id,
                        job.message_id,
                        "⏳ Денна квота AI-аналізу тимчасово вичерпана. Спробуй надіслати мем пізніше після оновлення ліміту (00:00 PT).",
                    )
                    .await;
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }
                Err(VlmError::Transient(e)) => {
                    error!(
                        user_id,
                        username,
                        file_unique_id = %file_unique_id,
                        error = %e,
                        "Transient VLM error during ingestion"
                    );
                    if job.retry_count < 2 {
                        let mut retried_job = job;
                        retried_job.retry_count += 1;
                        let delay_sec = 5 * retried_job.retry_count as u64;
                        warn!(
                            user_id,
                            username,
                            file_unique_id = %file_unique_id,
                            attempt = retried_job.retry_count,
                            delay_secs = delay_sec,
                            "Retrying meme ingestion job"
                        );

                        tokio::time::sleep(Duration::from_secs(delay_sec)).await;
                        let _ = sender.send(retried_job).await;
                    } else {
                        error!(
                            user_id,
                            username,
                            file_unique_id = %file_unique_id,
                            "Max retries reached; dropping meme ingestion job"
                        );
                        reply_to_job(
                            &bot,
                            job.chat_id,
                            job.message_id,
                            format!("[X] AI could not analyze meme: {}", e),
                        )
                        .await;
                    }
                    tokio::time::sleep(Duration::from_secs(4)).await;
                    continue;
                }
            };

            let id = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();

            let user_title = if job.captions == "no content from lazy user" {
                "".to_string()
            } else {
                job.captions
            };
            let title_for_tagger = user_title.clone();

            let save_result = {
                let mut engine = state.engine.lock().await;
                let mut meme_store = state.meme_store.lock().await;

                meme_store.add_from_analysis(
                    NewMeme {
                        id,
                        title: user_title,
                        analysis: &analysis,
                        file_tg_id: job.file_id,
                        file_tg_unique_id: job.file_unique_id,
                        user_id: job.user_id,
                        source_title: job.source_title,
                        source_url: job.source_url,
                        phash,
                        media_type: job.media_type,
                        thumbnail_tg_id: job.thumbnail_file_id.unwrap_or_default(),
                        vlm_version: constants::CURRENT_VLM_VERSION,
                    },
                    &mut engine,
                )
            };

            match save_result {
                Ok((saved_id, initial_is_public)) => {
                    info!(
                        user_id,
                        username,
                        meme_id = saved_id,
                        file_unique_id = %file_unique_id,
                        "Meme saved to SQLite"
                    );
                    let taxonomy = {
                        let meme_store = state.meme_store.lock().await;
                        meme_store.get_active_taxonomy()
                    };
                    match crate::tagger::classify_meme_tags(
                        &app_config.gemini_api_key,
                        &analysis.ocr_text,
                        &analysis.uk_description,
                        &analysis.en_description,
                        &title_for_tagger,
                        &taxonomy,
                    )
                    .await
                    {
                        Ok(res) => {
                            let tags_count = res.tags.len();
                            let tags_version = if res.is_undetermined {
                                constants::TAGS_STATE_UNDETERMINED
                            } else {
                                constants::CURRENT_TAGS_VERSION
                            };
                            let tag_save_res = {
                                let mut meme_store = state.meme_store.lock().await;
                                meme_store.set_meme_tags(saved_id, &res.tags, tags_version)
                            };
                            match tag_save_res {
                                Ok(()) => info!(
                                    user_id,
                                    meme_id = saved_id,
                                    tags_count,
                                    is_undetermined = res.is_undetermined,
                                    "Auto-tagged newly uploaded meme"
                                ),
                                Err(e) => warn!(
                                    user_id,
                                    meme_id = saved_id,
                                    error = %e,
                                    "Failed to save tags for newly uploaded meme"
                                ),
                            }
                        }
                        Err(crate::tagger::TagError::ContentBlocked(e)) => {
                            warn!(
                                user_id,
                                meme_id = saved_id,
                                error = %e,
                                "Decision API blocked meme text; marking as blocked/#інше"
                            );
                            let tag_save_res = {
                                let mut meme_store = state.meme_store.lock().await;
                                meme_store.set_meme_tags(
                                    saved_id,
                                    &[(crate::tagger::OTHER_TAG_SLUG.to_string(), 1.0)],
                                    constants::TAGS_STATE_BLOCKED,
                                )
                            };
                            if let Err(err) = tag_save_res {
                                warn!(
                                    user_id,
                                    meme_id = saved_id,
                                    error = %err,
                                    "Failed to mark blocked meme tags"
                                );
                            }
                        }
                        Err(e) => {
                            warn!(
                                user_id,
                                meme_id = saved_id,
                                error = ?e,
                                "Failed to auto-tag new meme; maintenance worker will backfill"
                            );
                        }
                    }

                    let card_info = {
                        let meme_store = state.meme_store.lock().await;
                        meme_store.get_meme_card_info(user_id, saved_id)
                    };
                    let (user_pub, eff_pub, card_tags) = match card_info {
                        Some(info) => (
                            info.user_is_public,
                            info.effective_is_public,
                            info.tags,
                        ),
                        None => (initial_is_public, initial_is_public, Vec::new()),
                    };
                    let (card_text, card_kb) = crate::bot::meme_actions::format_uploaded_meme_card(
                        saved_id,
                        user_pub,
                        eff_pub,
                        &card_tags,
                    );
                    reply_to_job_with_markup(&bot, job.chat_id, job.message_id, card_text, card_kb)
                        .await;
                }
                Err(e) => {
                    error!(
                        user_id,
                        username,
                        file_unique_id = %file_unique_id,
                        error = %e,
                        "Failed to save meme in SQLite"
                    );
                    reply_to_job(
                        &bot,
                        job.chat_id,
                        job.message_id,
                        format!("[X] Walter we lost it... {}", e),
                    )
                    .await;
                }
            }

            let sleep_dur = state.vlm_router.recommended_worker_sleep().await;
            tokio::time::sleep(sleep_dur).await;
        }
    });
}
