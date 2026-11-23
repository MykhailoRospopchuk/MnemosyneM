pub mod comand;
pub mod handlers;

use crate::AppState;
use crate::config::AppConfig;
use crate::database::enums::command_enum::Command;
use crate::worker::MemeJob;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use teloxide::Bot;
use teloxide::prelude::*;
use teloxide::types::Message;
use teloxide::types::User;
use teloxide::update_listeners;
use teloxide::utils::command::BotCommands;
use tokio::sync::mpsc::Sender;

async fn register_commands_with_retry(bot: &Bot, max_attempts: u32) {
    for attempt in 1..=max_attempts {
        match bot.set_my_commands(Command::bot_commands()).await {
            Ok(_) => {
                println!("[BOT] Commands registered successfully.");
                return;
            }
            Err(e) => {
                eprintln!(
                    "[BOT]-[WARN] set_my_commands failed (attempt {}/{}): {}",
                    attempt, max_attempts, e
                );
                if attempt < max_attempts {
                    let delay_secs = 2 * attempt as u64;
                    tokio::time::sleep(Duration::from_secs(delay_secs)).await;
                }
            }
        }
    }

    eprintln!(
        "[BOT]-[WARN] Could not register commands after {} attempts; continuing startup anyway.",
        max_attempts
    );
}

pub async fn run_bot(
    bot: Bot,
    state: Arc<AppState>,
    app_config: &'static AppConfig,
    sender: Sender<MemeJob>,
) {
    register_commands_with_retry(&bot, 3).await;

    let handler = dptree::entry()
        .branch(Update::filter_message().endpoint(message_handler))
        .branch(Update::filter_callback_query().endpoint(callback_handler))
        .branch(Update::filter_inline_query().endpoint(inline_query_handler));

    let polling = update_listeners::polling_default(bot.clone()).await;
    let consecutive_errors = Arc::new(AtomicU32::new(0));
    let polling_error_handler = Arc::new(move |err| {
        let counter = Arc::clone(&consecutive_errors);
        async move {
            let streak = counter.fetch_add(1, Ordering::Relaxed) + 1;
            let backoff_secs = (streak as u64 * 3).min(30);
            eprintln!(
                "[BOT]-[ERROR] Telegram polling error (streak={}): {:?}. Backing off {}s...",
                streak, err, backoff_secs
            );
            tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
        }
    });

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![state, sender, app_config])
        .enable_ctrlc_handler()
        .build()
        .dispatch_with_listener(polling, polling_error_handler)
        .await;
}

pub async fn message_handler(
    bot: Bot,
    msg: Message,
    state: Arc<AppState>,
    sender: Sender<MemeJob>,
    app_config: &'static AppConfig,
) -> ResponseResult<()> {
    if let Some(user) = msg.from.clone() {
        check_user(user, &state).await;
    }

    if let Some(photos) = msg.photo() {
        if let Some(photo) = photos.last() {
            handlers::handle_photo(&bot, &msg, photo, state, &sender).await?;
        }
    } else if let Some(animation) = msg.animation() {
        handlers::handle_animation(&bot, &msg, animation, state, &sender).await?;
    } else if let Some(text) = msg.text() {
        if text.starts_with('/') {
            match Command::parse(text, "") {
                Ok(Command::Start) => {
                    comand::start(bot, msg).await?;
                }
                Ok(Command::Help) => {
                    comand::help(bot, msg).await?;
                }
                Ok(Command::Stats) => {
                    comand::stats(bot, msg, state, app_config, &sender).await?;
                }
                Ok(Command::Random) => {
                    comand::random(bot, msg, state).await?;
                }
                Ok(Command::Settings) => {
                    comand::settings(bot, msg, state).await?;
                }
                Err(_) => {
                    comand::error(bot, msg).await?;
                }
            }
            return Ok(());
        }

        match text.trim() {
            comand::BTN_RANDOM => {
                comand::random(bot, msg, state).await?;
                return Ok(());
            }
            comand::BTN_STATS => {
                comand::stats(bot, msg, state, app_config, &sender).await?;
                return Ok(());
            }
            comand::BTN_SETTINGS => {
                comand::settings(bot, msg, state).await?;
                return Ok(());
            }
            comand::BTN_HELP => {
                comand::help(bot, msg).await?;
                return Ok(());
            }
            _ => {}
        }

        handlers::handle_search(&bot, &msg, text, &state, app_config).await?;
    }
    Ok(())
}
pub async fn callback_handler(
    bot: Bot,
    callback_query: CallbackQuery,
    state: Arc<AppState>,
) -> ResponseResult<()> {
    check_user(callback_query.from.clone(), &state).await;

    if let Some(data) = callback_query.data.clone() {
        let user_id = callback_query.from.id.0 as i64;

        let (text, keyboard) = {
            let mut meme_store = state.meme_store.lock().await;
            match data.as_str() {
                "toggle_upload" => {
                    if let Err(e) = meme_store.toggle_upload_mode(user_id) {
                        eprintln!("[ERROR] toggle_upload_mode failed: {}", e)
                    };
                }
                "cycle_scope" => {
                    if let Err(e) = meme_store.cycle_search_scope(user_id) {
                        eprintln!("[ERROR] cycle_search_scope failed: {}", e)
                    };
                }
                _ => {}
            }

            let settings = meme_store.get_user_settings(user_id);
            comand::format_settings(&settings)
        };

        let callback_query_id = callback_query.id.clone();

        if let Err(e) = bot.answer_callback_query(callback_query_id).await {
            eprintln!("[WARN] Failed to answer callback query: {}", e);
        };

        if let Some(msg) = callback_query.regular_message()
            && let Err(e) = bot
                .edit_message_text(msg.chat.id, msg.id, text)
                .reply_markup(keyboard)
                .await
        {
            eprintln!("[WARN] Failed to edit message: {}", e);
        }
    }
    Ok(())
}

pub async fn inline_query_handler(
    bot: Bot,
    query: InlineQuery,
    state: Arc<AppState>,
    app_config: &'static AppConfig,
) -> ResponseResult<()> {
    check_user(query.from.clone(), &state).await;

    handlers::handle_inline_query(bot, query, state, app_config).await?;

    Ok(())
}

pub async fn check_user(user: User, state: &Arc<AppState>) {
    let username = user.username.unwrap_or_default();
    let first_name = user.first_name;
    let last_name = user.last_name.unwrap_or_default();

    let mut meme_store = state.meme_store.lock().await;
    if let Err(e) = meme_store.sync_user(user.id.0 as i64, &username, &first_name, &last_name) {
        eprintln!("[ERROR] Failed to sync user {} : {}", user.id.0, e)
    };
}
