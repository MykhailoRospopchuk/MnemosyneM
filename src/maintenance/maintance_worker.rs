use std::{sync::Arc, time::Duration};

use teloxide::{Bot, net::Download, requests::Requester, types::FileId};
use tokio::time::{MissedTickBehavior, interval};

use crate::{AppState, helpers::hash_helper::compute_hash};

pub fn start_maintenance(state: Arc<AppState>, bot: Bot) {
    tokio::spawn(async move {
        println!("[MAINTENANCE] Background maintenance service started.");

        let mut timer = interval(Duration::from_secs(600));
        timer.set_missed_tick_behavior(MissedTickBehavior::Delay);

        loop {
            timer.tick().await;

            println!("[MAINTENANCE] Backfill pHash is started...");
            backfill_phash_processor(Arc::clone(&state), bot.clone()).await;
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
        "[MAINTENANCE] Fount {} memes withoud phash, Processing on background...",
        total
    );

    let mut success_count = 0;

    for (idx, (meme_id, file_tg_id)) in unhashed.into_iter().enumerate() {
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

        let mut image_bytes: Vec<u8> = Vec::new();
        if let Err(e) = bot.download_file(&file.path, &mut image_bytes).await {
            eprintln!(
                "[MAINTENANCE]-[WARN] Failed to download image for meme {} : {}",
                meme_id, e
            );
            continue;
        }

        let phash = match compute_hash(&image_bytes) {
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
        "[MAINTENANCE] Processing finished! Sucessfully hashed {}/{} memes.",
        success_count, total
    );
}
