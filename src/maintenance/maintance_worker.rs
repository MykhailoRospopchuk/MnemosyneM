use std::{sync::Arc, time::Duration};

use teloxide::{Bot, net::Download, requests::Requester, types::FileId};
use tokio::sync::mpsc::Sender;
use tokio::time::{MissedTickBehavior, interval};
use tracing::{debug, error, info, warn};

use crate::config::AppConfig;
use crate::constants::{
    CURRENT_TAGS_VERSION, CURRENT_VLM_VERSION, TAGS_STATE_BLOCKED, TAGS_STATE_REVIEWED_OTHER,
    TAGS_STATE_UNDETERMINED,
};
use crate::database::entity::media_type::MediaType;
use crate::database::enums::vlm_error_enum::VlmError;
use crate::helpers::embeding_helper::format_embedding_passage;
use crate::helpers::telegram_bot_helper::download_telegram_file;
use crate::tagger::{self, TagError};
use crate::worker::MemeJob;
use crate::{
    AppState,
    helpers::hash_helper::{compute_hash, extract_frame_from_video},
};

pub fn start_maintenance(
    state: Arc<AppState>,
    bot: Bot,
    app_config: &'static AppConfig,
    sender: Sender<MemeJob>,
) {
    tokio::spawn(async move {
        info!("Background maintenance service started");

        let mut timer = interval(Duration::from_secs(600));
        timer.set_missed_tick_behavior(MissedTickBehavior::Delay);

        loop {
            timer.tick().await;

            debug!("Starting pHash backfill pass");
            backfill_phash_processor(Arc::clone(&state), bot.clone()).await;

            if app_config.auto_reprocess {
                reprocess_vlm(Arc::clone(&state), bot.clone(), app_config, sender.clone()).await;
            }

            backfill_tags(Arc::clone(&state), app_config, sender.clone()).await;
            discover_new_categories(Arc::clone(&state), app_config, sender.clone()).await;
        }
    });
}

async fn backfill_phash_processor(state: Arc<AppState>, bot: Bot) {
    let unhashed = {
        let store = state.meme_store.lock().await;
        store.get_unhashed_memes()
    };

    let total = unhashed.len();
    if total == 0 {
        debug!("All memes already have pHash; nothing to backfill");
        return;
    }

    info!(total, "Found memes without pHash; backfilling in background");

    let mut success_count = 0;

    for (idx, (meme_id, file_tg_id, media_type)) in unhashed.into_iter().enumerate() {
        let file = match bot.get_file(FileId(file_tg_id)).await {
            Ok(f) => f,
            Err(e) => {
                warn!(meme_id, error = %e, "Failed to get Telegram file metadata during pHash backfill");
                continue;
            }
        };

        let mut file_bytes: Vec<u8> = Vec::new();
        if let Err(e) = bot.download_file(&file.path, &mut file_bytes).await {
            warn!(meme_id, error = %e, "Failed to download media during pHash backfill");
            continue;
        }

        let phash = match media_type {
            MediaType::Photo => tokio::task::spawn_blocking(move || compute_hash(&file_bytes))
                .await
                .map_err(|e| format!("Blocking task panicked: {}", e))
                .and_then(|res| res),
            MediaType::Animation => tokio::task::spawn_blocking(move || {
                extract_frame_from_video(&file_bytes).and_then(|frame| compute_hash(&frame))
            })
            .await
            .map_err(|e| format!("Blocking task panicked: {}", e))
            .and_then(|res| res),
        };

        let phash = match phash {
            Ok(h) => h,
            Err(e) => {
                warn!(meme_id, error = %e, "Failed to calculate pHash during backfill");
                continue;
            }
        };

        if phash != 0 {
            let mut storage = state.meme_store.lock().await;
            if let Err(e) = storage.update_phash(meme_id, phash) {
                warn!(meme_id, error = %e, "Failed to save pHash during backfill");
            } else {
                success_count += 1;
            }
        }

        if (idx + 1) % 25 == 0 || idx + 1 == total {
            info!(processed = idx + 1, total, "pHash backfill progress");
        }

        tokio::time::sleep(Duration::from_millis(60)).await;
    }

    info!(success_count, total, "pHash backfill pass finished");
}

async fn reprocess_vlm(
    state: Arc<AppState>,
    bot: Bot,
    app_config: &'static AppConfig,
    sender: Sender<MemeJob>,
) {
    let reprocess_candidate = {
        let store = state.meme_store.lock().await;
        store.get_memes_for_reprocess(CURRENT_VLM_VERSION, Some(100))
    };

    if !reprocess_candidate.is_empty() {
        let backup_path = format!("{}.pre_v{}.bak", app_config.db_path, CURRENT_VLM_VERSION);
        let store = state.meme_store.lock().await;
        match store.create_pre_process_backup(backup_path.clone()) {
            Ok(()) => info!(backup_path = %backup_path, "Pre-reprocess backup created"),
            Err(e) => {
                error!(backup_path = %backup_path, error = %e, "Failed to create pre-reprocess backup");
                return;
            }
        };
        drop(store);
    }

    let mut updated: usize = 0;
    let mut skipped: usize = 0;
    let mut consecutive_transient: u8 = 0;
    let total = reprocess_candidate.len();

    for (idx, item) in reprocess_candidate.into_iter().enumerate() {
        while sender.capacity() < sender.max_capacity() {
            tokio::time::sleep(Duration::from_secs(5)).await;
        }

        let mime_type = match item.media_type {
            MediaType::Photo => "image/jpeg",
            MediaType::Animation => "video/mp4",
        };

        let bytes = match download_telegram_file(&bot, &item.file_tg_id, 3).await {
            Ok(b) => b,
            Err(e) => {
                warn!(meme_id = item.id, error = %e, "Failed to download media for VLM reprocess");
                continue;
            }
        };

        let vlm_res = state
            .vlm_router
            .describe_meme(&bytes, &app_config.gemini_api_key, mime_type)
            .await;
        crate::persist_dirty_vlm_quotas(&state).await;

        match vlm_res {
            Ok(analysis) => {
                let passage_uk =
                    format_embedding_passage(&analysis.ocr_text, &analysis.uk_description);
                let passage_en =
                    format_embedding_passage(&analysis.ocr_text, &analysis.en_description);

                let embeds: Result<(Vec<f32>, Vec<f32>), String> = {
                    let mut engine = state.engine.lock().await;
                    engine.embed_passages_pair(&passage_uk, &passage_en)
                };

                let (embed_uk, embed_en) = match embeds {
                    Ok(pair) => pair,
                    Err(e) => {
                        warn!(meme_id = item.id, error = %e, "Failed to embed reprocessed descriptions");
                        continue;
                    }
                };

                let res = {
                    let mut store = state.meme_store.lock().await;
                    store.update_meme_reprocessed(
                        item.id,
                        &analysis,
                        embed_uk,
                        embed_en,
                        CURRENT_VLM_VERSION,
                        &item.title,
                    )
                };

                if let Err(e) = res {
                    warn!(meme_id = item.id, error = %e, "Failed to save reprocessed meme in DB");
                } else {
                    updated += 1;
                    consecutive_transient = 0;
                    info!(
                        processed = idx + 1,
                        total,
                        meme_id = item.id,
                        "Reprocessed meme VLM metadata"
                    );
                }
            }
            Err(VlmError::InappropriateContent(reason)) => {
                warn!(meme_id = item.id, reason = %reason, "Skipped NSFW meme during reprocess");
                let mut store = state.meme_store.lock().await;
                let _ = store.mark_meme_vlm_version(item.id, -CURRENT_VLM_VERSION);
                skipped += 1;
            }
            Err(VlmError::DailyQuotaExhausted(msg)) => {
                warn!(
                    meme_id = item.id,
                    error = %msg,
                    "All VLM models hit daily quota during reprocess; pausing until PT reset"
                );
                break;
            }
            Err(VlmError::Transient(e)) => {
                consecutive_transient += 1;
                warn!(
                    meme_id = item.id,
                    consecutive_transient,
                    error = %e,
                    "Transient VLM error during reprocess"
                );
                if consecutive_transient >= 3 {
                    warn!("Pausing VLM reprocess until next tick due to repeated transient errors");
                    break;
                }
            }
        }

        let pace = state
            .vlm_router
            .recommended_worker_sleep()
            .await
            .max(Duration::from_secs(3));
        tokio::time::sleep(pace).await;
    }

    if total == 0 {
        debug!("Reprocess VLM pass done; nothing to reprocess");
    } else {
        info!(total, updated, skipped, "Reprocess VLM pass finished");
    }
}

async fn backfill_tags(
    state: Arc<AppState>,
    app_config: &'static AppConfig,
    sender: Sender<MemeJob>,
) {
    let (candidates, taxonomy) = {
        let store = state.meme_store.lock().await;
        (
            store.get_memes_for_tagging(CURRENT_TAGS_VERSION, 150),
            store.get_active_taxonomy(),
        )
    };

    let total = candidates.len();
    if total == 0 {
        debug!("All memes already have tags; nothing to backfill");
        return;
    }

    info!(batch_size = total, "Starting background meme tag backfill pass");

    let mut tagged: usize = 0;
    let mut consecutive_errors: u8 = 0;

    for (idx, item) in candidates.into_iter().enumerate() {
        while sender.capacity() < sender.max_capacity() {
            tokio::time::sleep(Duration::from_secs(5)).await;
        }

        match tagger::classify_meme_tags(
            &app_config.gemini_api_key,
            &item.ocr_text,
            &item.description_uk,
            &item.description_en,
            &item.title,
            &taxonomy,
        )
        .await
        {
            Ok(res) => {
                let tags_version = if res.is_undetermined {
                    TAGS_STATE_UNDETERMINED
                } else {
                    CURRENT_TAGS_VERSION
                };
                let save_res = {
                    let mut store = state.meme_store.lock().await;
                    store.set_meme_tags(item.id, &res.tags, tags_version)
                };
                match save_res {
                    Ok(()) => {
                        tagged += 1;
                        consecutive_errors = 0;
                        if (idx + 1) % 25 == 0 || idx + 1 == total {
                            info!(
                                processed = idx + 1,
                                total,
                                tagged,
                                "Background meme tag backfill progress"
                            );
                        }
                    }
                    Err(e) => {
                        warn!(meme_id = item.id, error = %e, "Failed to save backfilled tags");
                    }
                }
            }
            Err(TagError::RateLimited(e)) => {
                warn!(
                    meme_id = item.id,
                    error = %e,
                    "Decision API rate-limited during tag backfill; pausing until next tick"
                );
                break;
            }
            Err(TagError::ContentBlocked(e)) => {
                warn!(
                    meme_id = item.id,
                    error = %e,
                    "Decision API blocked meme text; marking as blocked/#інше"
                );
                let save_res = {
                    let mut store = state.meme_store.lock().await;
                    store.set_meme_tags(
                        item.id,
                        &[(tagger::OTHER_TAG_SLUG.to_string(), 1.0)],
                        TAGS_STATE_BLOCKED,
                    )
                };
                if let Err(err) = save_res {
                    warn!(meme_id = item.id, error = %err, "Failed to mark blocked meme tags");
                }
            }
            Err(TagError::Transient(e)) => {
                consecutive_errors += 1;
                warn!(
                    meme_id = item.id,
                    consecutive_errors,
                    error = %e,
                    "Transient error during tag backfill"
                );
                if consecutive_errors >= 3 {
                    warn!("Pausing tag backfill until next tick due to repeated transient errors");
                    break;
                }
            }
        }

        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    info!(tagged, total, "Tag backfill pass finished");
}

const DISCOVERY_MIN_BATCH: usize = 10;
const DISCOVERY_BATCH_LIMIT: usize = 30;

async fn discover_new_categories(
    state: Arc<AppState>,
    app_config: &'static AppConfig,
    sender: Sender<MemeJob>,
) {
    let (samples, taxonomy) = {
        let store = state.meme_store.lock().await;
        (
            store.get_undetermined_memes_for_discovery(DISCOVERY_BATCH_LIMIT),
            store.get_active_taxonomy(),
        )
    };

    if samples.len() < DISCOVERY_MIN_BATCH {
        debug!(
            pending = samples.len(),
            min_batch = DISCOVERY_MIN_BATCH,
            "Not enough undetermined memes for category discovery yet"
        );
        return;
    }

    let batch_ids: Vec<u64> = samples
        .iter()
        .filter_map(|s| u64::try_from(s.id).ok())
        .collect();
    let remaining_slots = tagger::MAX_ACTIVE_TAGS.saturating_sub(taxonomy.len());

    if remaining_slots == 0 {
        let mut store = state.meme_store.lock().await;
        if let Err(e) = store.mark_memes_tags_version(&batch_ids, TAGS_STATE_REVIEWED_OTHER) {
            warn!(error = %e, "Failed to mark undetermined memes as reviewed at tag cap");
        }
        return;
    }

    while sender.capacity() < sender.max_capacity() {
        tokio::time::sleep(Duration::from_secs(5)).await;
    }

    let proposed = match tagger::propose_new_categories(
        &app_config.gemini_api_key,
        &samples,
        &taxonomy,
        remaining_slots,
    )
    .await
    {
        Ok(list) => list,
        Err(e) => {
            warn!(
                error = ?e,
                "Category discovery call failed; will retry on next tick"
            );
            return;
        }
    };

    if proposed.is_empty() {
        let mut store = state.meme_store.lock().await;
        if let Err(e) = store.mark_memes_tags_version(&batch_ids, TAGS_STATE_REVIEWED_OTHER) {
            warn!(error = %e, "Failed to mark reviewed undetermined memes");
        } else {
            info!(
                reviewed = batch_ids.len(),
                "Category discovery found no recurring missing categories"
            );
        }
        return;
    }

    let (added_slugs, updated_taxonomy) = {
        let mut store = state.meme_store.lock().await;
        let mut added = Vec::new();
        for def in &proposed {
            match store.add_dynamic_category(def) {
                Ok(true) => added.push(def.slug.clone()),
                Ok(false) => {}
                Err(e) => warn!(slug = %def.slug, error = %e, "Failed to store dynamic category"),
            }
        }
        let updated = store.get_active_taxonomy();
        (added, updated)
    };

    if added_slugs.is_empty() {
        let mut store = state.meme_store.lock().await;
        if let Err(e) = store.mark_memes_tags_version(&batch_ids, TAGS_STATE_REVIEWED_OTHER) {
            warn!(error = %e, "Failed to mark reviewed undetermined memes after empty insert");
        }
        return;
    }

    info!(
        new_categories = ?added_slugs,
        batch_size = samples.len(),
        "Discovered new categories; re-classifying undetermined batch once"
    );

    let mut consecutive_errors: u8 = 0;
    for item in samples {
        let Ok(meme_id) = u64::try_from(item.id) else {
            continue;
        };

        while sender.capacity() < sender.max_capacity() {
            tokio::time::sleep(Duration::from_secs(5)).await;
        }

        match tagger::classify_meme_tags(
            &app_config.gemini_api_key,
            &item.ocr_text,
            &item.description_uk,
            &item.description_en,
            &item.title,
            &updated_taxonomy,
        )
        .await
        {
            Ok(res) => {
                consecutive_errors = 0;
                let tags_version = if res.is_undetermined {
                    TAGS_STATE_REVIEWED_OTHER
                } else {
                    CURRENT_TAGS_VERSION
                };
                let save_res = {
                    let mut store = state.meme_store.lock().await;
                    store.set_meme_tags(meme_id, &res.tags, tags_version)
                };
                if let Err(e) = save_res {
                    warn!(meme_id, error = %e, "Failed to save re-classified discovery meme tags");
                }
            }
            Err(TagError::RateLimited(e)) => {
                warn!(
                    meme_id,
                    error = %e,
                    "Decision API rate-limited during discovery re-classification; pausing"
                );
                break;
            }
            Err(TagError::ContentBlocked(e)) => {
                warn!(
                    meme_id,
                    error = %e,
                    "Decision API blocked meme text during discovery re-classification; marking as blocked/#інше"
                );
                let save_res = {
                    let mut store = state.meme_store.lock().await;
                    store.set_meme_tags(
                        meme_id,
                        &[(tagger::OTHER_TAG_SLUG.to_string(), 1.0)],
                        TAGS_STATE_BLOCKED,
                    )
                };
                if let Err(err) = save_res {
                    warn!(meme_id, error = %err, "Failed to mark blocked discovery meme tags");
                }
            }
            Err(TagError::Transient(e)) => {
                consecutive_errors += 1;
                warn!(
                    meme_id,
                    consecutive_errors,
                    error = %e,
                    "Transient error during discovery re-classification"
                );
                if consecutive_errors >= 3 {
                    warn!("Stopping discovery re-classification after repeated transient errors");
                    break;
                }
            }
        }

        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}
