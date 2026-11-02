use std::{sync::Arc, time::Duration};

use crate::AppState;
use crate::config::AppConfig;
use crate::vlm;
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

            let descriptions = match vlm::describe_meme(&image_bytes, &app_config.gemini_api_key)
                .await
            {
                Ok(desc) => desc,
                Err(vlm::VlmError::InappropriateContent(reason)) => {
                    eprintln!(
                        "[WORKER SECURITY ALERT] Inappropriate meme rejected: {}",
                        reason
                    );
                    let _ = bot.send_message(
                            job.chat_id,
                            format!("⛔ [REJECTED] Inappropriate content detected: {}\nNSFW content is strictly prohibited!", reason),
                        )
                        .await;
                    tokio::time::sleep(Duration::from_secs(4)).await;
                    continue;
                }
                Err(vlm::VlmError::Transient(e)) => {
                    eprintln!("[WORKER]-[ERROR] Something go wrong VLM: {}", e);
                    if job.retry_count < 2 {
                        let mut retried_job = job;
                        retried_job.retry_count += 1;
                        println!(
                            "[WORKER] Retrying meme {} (attempt {}/2)...",
                            retried_job.file_unique_id, retried_job.retry_count
                        );
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

            let full_title = format!("{} | {}", job.captions, descriptions);

            let mut engine = state.engine.lock().await;
            let mut meme_store = state.meme_store.lock().await;

            match meme_store.add_from_descriptions(
                id,
                full_title.clone(),
                job.file_id,
                job.file_unique_id,
                job.user_id,
                descriptions,
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
