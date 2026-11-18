use std::{sync::Arc, time::Duration};

use teloxide::{Bot, net::Download, requests::Requester, types::FileId};
use tokio::sync::mpsc::Sender;
use tokio::time::{MissedTickBehavior, interval};

use crate::config::AppConfig;
use crate::constants::CURRENT_VLM_VERSION;
use crate::database::entity::media_type::MediaType;
use crate::database::enums::vlm_error_enum::VlmError;
use crate::helpers::embeding_helper::format_embedding_passage;
use crate::helpers::telegram_bot_helper::download_telegram_file;
use crate::vlm;
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
        println!("[MAINTENANCE] Background maintenance service started.");

        let mut timer = interval(Duration::from_secs(600));
        timer.set_missed_tick_behavior(MissedTickBehavior::Delay);

        loop {
            timer.tick().await;

            println!("[MAINTENANCE] Backfill pHash is started...");
            backfill_phash_processor(Arc::clone(&state), bot.clone()).await;

            if app_config.auto_reprocess {
                reprocess_vlm(Arc::clone(&state), bot.clone(), app_config, sender.clone()).await;
            }
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
        println!("[MAINTENANCE] All memes already have phash. Nothing to process.");
        return;
    }

    println!(
        "[MAINTENANCE] Found {} memes without phash, Processing in background...",
        total
    );

    let mut success_count = 0;

    for (idx, (meme_id, file_tg_id, media_type)) in unhashed.into_iter().enumerate() {
        let file = match bot.get_file(FileId(file_tg_id)).await {
            Ok(f) => f,
            Err(e) => {
                eprintln!(
                    "[MAINTENANCE]-[WARN] Failed to get file for meme {} : {}",
                    meme_id, e
                );
                continue;
            }
        };

        let mut file_bytes: Vec<u8> = Vec::new();
        if let Err(e) = bot.download_file(&file.path, &mut file_bytes).await {
            eprintln!(
                "[MAINTENANCE]-[WARN] Failed to download media for meme {} : {}",
                meme_id, e
            );
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
                eprintln!(
                    "[MAINTENANCE]-[WARN] Failed to calculate hash for meme {} : {}",
                    meme_id, e
                );
                continue;
            }
        };

        if phash != 0 {
            let mut storage = state.meme_store.lock().await;
            if let Err(e) = storage.update_phash(meme_id, phash) {
                eprintln!(
                    "[MAINTENANCE]-[WARN] Failed to save hash for meme {} : {}",
                    meme_id, e
                );
            } else {
                success_count += 1;
            }
        }

        if (idx + 1) % 25 == 0 || idx + 1 == total {
            println!(
                "[MAINTENANCE] hashed and synced {}/{} memes in background...",
                idx + 1,
                total
            );
        }

        tokio::time::sleep(Duration::from_millis(60)).await;
    }

    println!(
        "[MAINTENANCE] Processing finished! Successfully hashed {}/{} memes.",
        success_count, total
    );
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
        let store = state.meme_store.lock().await;
        match store.create_pre_process_backup(format!(
            "{}.pre_v{}.bak",
            app_config.db_path, CURRENT_VLM_VERSION
        )) {
            Ok(()) => println!("[MAINTENANCE] - Backup created"),
            Err(e) => {
                eprintln!("[MAINTENANCE]-[ERROR] - Backup NOT created | ERROR: {}", e);
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
                eprintln!(
                    "[MAINTENANCE]-[WARN] Download failed for {}: {}",
                    item.id, e
                );
                continue;
            }
        };

        match vlm::describe_meme(&bytes, &app_config.gemini_api_key, mime_type).await {
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
                        eprintln!("[MAINTENANCE]-[WARN] Embed failed for {}: {}", item.id, e);
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
                    eprintln!(
                        "[MAINTENANCE]-[WARN] DB update failed for {}: {}",
                        item.id, e
                    );
                } else {
                    updated += 1;
                    consecutive_transient = 0;
                    println!(
                        "[MAINTENANCE] Reprocessed {}/{} (id={})",
                        idx + 1,
                        total,
                        item.id
                    );
                }
            }
            Err(VlmError::InappropriateContent(reason)) => {
                eprintln!(
                    "[MAINTENANCE]-[WARN] Skipped NSFW meme {}: {}",
                    item.id, reason
                );
                let mut store = state.meme_store.lock().await;
                let _ = store.mark_meme_vlm_version(item.id, -CURRENT_VLM_VERSION);
                skipped += 1;
            }
            Err(VlmError::Transient(e)) => {
                consecutive_transient += 1;
                eprintln!(
                    "[MAINTENANCE]-[WARN] Transient VLM error ({}/3) for {}: {}",
                    consecutive_transient, item.id, e
                );
                if consecutive_transient >= 3 {
                    eprintln!(
                        "[MAINTENANCE] Pausing reprocess until next tick (possible RPD/429 limit)."
                    );
                    break;
                }
            }
        }

        tokio::time::sleep(Duration::from_secs(5)).await;
    }

    println!(
        "[MAINTENANCE] Reprocess pass done: updated={}, skipped={}",
        updated, skipped
    );
}
