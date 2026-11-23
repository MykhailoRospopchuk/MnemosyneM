use std::time::Duration;

use teloxide::{Bot, net::Download, requests::Requester, types::FileId};
use tracing::warn;

pub async fn download_telegram_file(
    bot: &Bot,
    file_id: &str,
    max_attemp: u32,
) -> Result<Vec<u8>, String> {
    let mut last_error = String::new();

    for attemp in 1..=max_attemp {
        match bot.get_file(FileId(file_id.to_string())).await {
            Ok(file) => {
                let mut file_bytes: Vec<u8> = Vec::new();
                match bot.download_file(&file.path, &mut file_bytes).await {
                    Ok(()) => return Ok(file_bytes),
                    Err(e) => {
                        last_error = e.to_string();
                    }
                }
            }
            Err(e) => {
                last_error = e.to_string();
            }
        }

        if attemp < max_attemp {
            let delay_second = attemp as u64 * (2 * attemp as u64);
            warn!(
                attempt = attemp,
                max_attempts = max_attemp,
                file_id,
                delay_secs = delay_second,
                error = %last_error,
                "Telegram file download failed; retrying"
            );
            tokio::time::sleep(Duration::from_secs(delay_second)).await;
        }
    }

    Err(last_error)
}
