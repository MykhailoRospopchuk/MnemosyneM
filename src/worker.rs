use std::{sync::Arc, time::Duration};

use crate::config::AppConfig;
use crate::database::entity::media_type::MediaType;
use crate::database::entity::new_meme::NewMeme;
use crate::database::enums::vlm_error_enum::VlmError;
use crate::vlm;
use crate::{
    AppState,
    helpers::hash_helper::{compute_hash, extract_frame_from_video},
};
use teloxide::{
    Bot,
    net::Download,
    requests::Requester,
    types::{ChatId, FileId},
};
use tokio::sync::mpsc::{Receiver, Sender};

#[derive(Clone, Debug)]
pub struct MemeJob {
    pub chat_id: ChatId,
    pub file_id: String,
    pub file_unique_id: String,
    pub captions: String,
    pub retry_count: u8,
    pub user_id: i64,
    pub source_title: String,
    pub source_url: String,
    pub media_type: MediaType,
    pub thumbnail_file_id: Option<String>,
}

pub fn start_worker(
    mut receiver: Receiver<MemeJob>,
    sender: Sender<MemeJob>,
    state: Arc<AppState>,
    bot: Bot,
    app_config: &'static AppConfig,
) {
    tokio::spawn(async move {
        println!("[WORKER] Background queue consumer started....");

        while let Some(job) = receiver.recv().await {
            println!("[--- QUEUE WORKER ---]");
            println!(
                "[WORKER] Processing meme: {} | Total Queue: [{}]",
                job.file_unique_id,
                receiver.len()
            );

            let file = match bot.get_file(FileId(job.file_id.clone())).await {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("[WORKER]-[ERROR] Failed to get file from TG: {}", e);
                    continue;
                }
            };

            let mut image_bytes: Vec<u8> = Vec::new();
            if let Err(e) = bot.download_file(&file.path, &mut image_bytes).await {
                eprintln!("[WORKER]-[ERROR] Failed to download image: {}", e);
                continue;
            }

            let phash = match job.media_type {
                MediaType::Photo => {
                    let bytes = image_bytes.clone();
                    match tokio::task::spawn_blocking(move || compute_hash(&bytes)).await {
                        Ok(Ok(phash)) => phash,
                        Ok(Err(e)) => {
                            eprintln!("[WORKER]-[WARN] Failed to compute hash for image: {}", e);
                            0
                        }
                        Err(e) => {
                            eprintln!("[WORKER]-[WARN] Blocking hash task panicked: {}", e);
                            0
                        }
                    }
                }
                MediaType::Animation => {
                    let mut frame_bytes = None;
                    if let Some(thumbnail_id) = job.thumbnail_file_id.clone() {
                        if let Ok(thumb_file) = bot.get_file(FileId(thumbnail_id)).await {
                            let mut thumb_bytes: Vec<u8> = Vec::new();
                            if bot
                                .download_file(&thumb_file.path, &mut thumb_bytes)
                                .await
                                .is_ok()
                            {
                                frame_bytes = Some(thumb_bytes);
                            }
                        }
                    }

                    let bytes_for_ffmpeg = if frame_bytes.is_none() {
                        println!(
                            "[WORKER] Telegram provided no thumbnail. Extracting first frame via ffmpeg..."
                        );
                        Some(image_bytes.clone())
                    } else {
                        None
                    };

                    tokio::task::spawn_blocking(move || {
                        let jpeg_bytes = frame_bytes.or_else(|| {
                            bytes_for_ffmpeg.and_then(|b| extract_frame_from_video(&b).ok())
                        });
                        jpeg_bytes.and_then(|b| compute_hash(&b).ok()).unwrap_or(0)
                    })
                    .await
                    .unwrap_or(0)
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
                    println!(
                        "[WORKER] Duplicate meme detected by pHash. Matched meme [{}] | distance [{distance}])! Skipping Gemini VLM",
                        exiting_meme_id
                    );

                    match store.link_existing_meme(exiting_meme_id, job.user_id, &job.captions) {
                        Ok(is_new_for_user) => {
                            let text = if is_new_for_user {
                                "[D] This meme already persist in DB (> 92%)! Added immediattely to your collection"
                            } else {
                                "[E] Already exist in your collection"
                            };

                            let _ = bot.send_message(job.chat_id, text).await;
                        }
                        Err(e) => {
                            eprintln!("[WORKER]-[ERROR] Failed to ling duplicate meme: {}", e)
                        }
                    }

                    drop(store);
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    continue;
                }

                drop(store);
            }

            let analysis = match vlm::describe_meme(
                &image_bytes,
                &app_config.gemini_api_key,
                &mime_type,
            )
            .await
            {
                Ok(desc) => desc,
                Err(VlmError::InappropriateContent(reason)) => {
                    eprintln!(
                        "[WORKER SECURITY ALERT] Inappropriate meme rejected: {}",
                        reason
                    );
                    let _ = bot.send_message(
                            job.chat_id,
                            format!("[X] [REJECTED] Inappropriate content detected: {}\nNSFW content is strictly prohibited!", reason),
                        )
                        .await;
                    tokio::time::sleep(Duration::from_secs(4)).await;
                    continue;
                }
                Err(VlmError::Transient(e)) => {
                    eprintln!("[WORKER]-[ERROR] Something go wrong VLM: {}", e);
                    if job.retry_count < 2 {
                        let mut retried_job = job;
                        retried_job.retry_count += 1;
                        let delay_sec = 5 * retried_job.retry_count as u64;
                        println!(
                            "[WORKER] Retrying meme {} (attempt {}/2)... after {} sec",
                            retried_job.file_unique_id, retried_job.retry_count, delay_sec
                        );

                        tokio::time::sleep(Duration::from_secs(delay_sec)).await;
                        let _ = sender.send(retried_job).await;
                    } else {
                        eprintln!(
                            "[WORKER] Max retries reached for {}. Dropping task.",
                            job.file_unique_id
                        );
                        let _ = bot
                            .send_message(
                                job.chat_id,
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

            let mut engine = state.engine.lock().await;
            let mut meme_store = state.meme_store.lock().await;

            match meme_store.add_from_analysis(
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
                },
                &mut engine,
            ) {
                Ok(()) => {
                    println!("[WORKER] Sucessfully saved in SQLite!");
                    let _ = bot
                        .send_message(
                            job.chat_id,
                            format!("[V] Your meme has been confiscated..."),
                        )
                        .await;
                }
                Err(e) => {
                    eprintln!("[WORKER]-[ERROR] DB error SQLite: {}", e);
                    let _ = bot
                        .send_message(job.chat_id, format!("[X] Walter we lost it... {}", e))
                        .await;
                }
            }

            tokio::time::sleep(Duration::from_secs(4)).await;
        }
    });
}
