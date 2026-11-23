use std::sync::Arc;
use std::time::Duration;

use teloxide::Bot;
use teloxide::RequestError;
use teloxide::prelude::*;
use teloxide::types::FileId;
use teloxide::types::InlineKeyboardButton;
use teloxide::types::InlineKeyboardMarkup;
use teloxide::types::InputFile;
use teloxide::types::KeyboardButton;
use teloxide::types::KeyboardMarkup;
use teloxide::types::Message;
use teloxide::utils::command::BotCommands;
use tokio::sync::mpsc::Sender;

use crate::AppState;
use crate::bot::handlers::build_caption;
use crate::config::AppConfig;
use crate::database::entity::media_type::MediaType;
use crate::database::entity::user_setting::UserSetting;
use crate::database::enums::command_enum::Command;
use crate::worker::MemeJob;

pub const BTN_RANDOM: &str = "🎲 Випадковий мем";
pub const BTN_STATS: &str = "📊 Статистика";
pub const BTN_SETTINGS: &str = "⚙️ Налаштування";
pub const BTN_HELP: &str = "❓ Допомога";

pub fn main_keyboard() -> KeyboardMarkup {
    KeyboardMarkup::new(vec![
        vec![
            KeyboardButton::new(BTN_RANDOM),
            KeyboardButton::new(BTN_STATS),
        ],
        vec![
            KeyboardButton::new(BTN_SETTINGS),
            KeyboardButton::new(BTN_HELP),
        ],
    ])
    .resize_keyboard()
    .persistent()
}

pub async fn start(bot: Bot, msg: Message) -> Result<Message, RequestError> {
    let message = bot
        .send_message(
            msg.chat.id,
            "Hi! I'm MnemosyneM - semantic meme search \n\
    Send me your meme pic and I will save it. \n\
    Text me your search and I will find your meme\n\n\
    List of commands: /help"
                .to_string(),
        )
        .reply_markup(main_keyboard())
        .await?;

    Ok(message)
}

pub async fn help(bot: Bot, msg: Message) -> Result<Message, RequestError> {
    let message = bot
        .send_message(msg.chat.id, Command::descriptions().to_string())
        .reply_markup(main_keyboard())
        .await?;

    Ok(message)
}

fn format_uptime(duration: Duration) -> String {
    let total_secs = duration.as_secs();
    let days = total_secs / 86_400;
    let hours = (total_secs % 86_400) / 3_600;
    let mins = (total_secs % 3_600) / 60;
    let secs = total_secs % 60;

    if days > 0 {
        format!("{}d {}h {}m {}s", days, hours, mins, secs)
    } else if hours > 0 {
        format!("{}h {}m {}s", hours, mins, secs)
    } else if mins > 0 {
        format!("{}m {}s", mins, secs)
    } else {
        format!("{}s", secs)
    }
}

fn read_process_rss_mb() -> Option<f64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let rss_pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    Some((rss_pages * 4096) as f64 / (1024.0 * 1024.0))
}

pub async fn stats(
    bot: Bot,
    msg: Message,
    state: Arc<AppState>,
    app_config: &'static AppConfig,
    sender: &Sender<MemeJob>,
) -> Result<Message, RequestError> {
    let (public, private) = {
        let meme_store = state.meme_store.lock().await;
        meme_store.len(msg.chat.id.0)
    };

    let uptime = format_uptime(state.started_at.elapsed());
    let rss_label = match read_process_rss_mb() {
        Some(mb) => format!("{:.1} MB", mb),
        None => "N/A".to_string(),
    };
    let db_size_label = match std::fs::metadata(&app_config.db_path) {
        Ok(meta) => format!("{:.2} MB", meta.len() as f64 / (1024.0 * 1024.0)),
        Err(_) => "N/A".to_string(),
    };
    let max_queue = sender.max_capacity();
    let queued = max_queue.saturating_sub(sender.capacity());
    let reranker_label = if state.reranker.is_some() {
        "ON (Jina v2)"
    } else {
        "OFF (Phase 1A only)"
    };

    let text = format!(
        "📊 MnemosyneM Stats\n\n\
        🗂 Public memes: {}\n\
        🔒 Your private memes: {}\n\
        ⏱ Uptime: {}\n\
        🧠 RAM (RSS): {}\n\
        💾 SQLite DB: {}\n\
        📥 Upload queue: {}/{}\n\
        🎯 Stage-2 Reranker: {}",
        public, private, uptime, rss_label, db_size_label, queued, max_queue, reranker_label
    );

    let message = bot.send_message(msg.chat.id, text).await?;

    Ok(message)
}

pub async fn random(bot: Bot, msg: Message, state: Arc<AppState>) -> Result<Message, RequestError> {
    let meme = {
        let meme_store = state.meme_store.lock().await;
        meme_store.random_meme(msg.chat.id.0).cloned()
    };

    let message: Message;
    if let Some(meme) = meme {
        if !meme.file_tg_id.is_empty() {
            let captions = format!(
                "🎲 Random meme | {}",
                build_caption(&meme.source_title, &meme.source_url, &1.0)
            );
            match meme.media_type {
                MediaType::Photo => {
                    message = bot
                        .send_photo(
                            msg.chat.id,
                            InputFile::file_id(FileId(meme.file_tg_id.clone())),
                        )
                        .caption(captions)
                        .await?;
                }
                MediaType::Animation => {
                    message = bot
                        .send_animation(
                            msg.chat.id,
                            InputFile::file_id(FileId(meme.file_tg_id.clone())),
                        )
                        .caption(captions)
                        .await?;
                }
            }
        } else {
            message = bot
                .send_message(msg.chat.id, "Sorry your meme somebody stole(((")
                .await?;
        }
    } else {
        message = bot
            .send_message(
                msg.chat.id,
                "What do yue even expect? Upload at least one meme",
            )
            .await?;
    }

    Ok(message)
}

pub async fn error(bot: Bot, msg: Message) -> Result<Message, RequestError> {
    let message = bot
        .send_message(
            msg.chat.id,
            "I have no idea what are you talking about. Use /help ...",
        )
        .await?;

    Ok(message)
}

pub async fn settings(
    bot: Bot,
    msg: Message,
    state: Arc<AppState>,
) -> Result<Message, RequestError> {
    let (text, keyboard) = {
        let meme_store = state.meme_store.lock().await;
        let s = meme_store.get_user_settings(msg.chat.id.0);

        format_settings(&s)
    };

    bot.send_message(msg.chat.id, text)
        .reply_markup(keyboard)
        .await
}

pub fn format_settings(settings: &UserSetting) -> (String, InlineKeyboardMarkup) {
    let upload_label = if settings.upload_as_public {
        "[P] Public"
    } else {
        "[P] Private"
    };

    let scope_label = match settings.search_scope.as_str() {
        "private_only" => "[M] Only my memes",
        "public_only" => "[O] Only public memes",
        _ => "[A] All (Public + Mine)",
    };

    let text = "[S] User settings:\n".to_string();

    let keyboard = InlineKeyboardMarkup::new(vec![
        vec![InlineKeyboardButton::callback(
            format!("Uploading: {}", upload_label),
            "toggle_upload",
        )],
        vec![InlineKeyboardButton::callback(
            format!("Search: {}", scope_label),
            "cycle_scope",
        )],
    ]);

    (text, keyboard)
}
