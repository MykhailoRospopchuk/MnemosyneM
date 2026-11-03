pub mod comand;
pub mod handlers;

use crate::AppState;
use crate::config::AppConfig;
use crate::worker::MemeJob;
use std::sync::Arc;
use teloxide::Bot;
use teloxide::prelude::*;
use teloxide::types::Message;
use teloxide::types::User;
use teloxide::utils::command::BotCommands;
use tokio::sync::mpsc::Sender;

pub async fn run_bot(
    bot: Bot,
    state: Arc<AppState>,
    app_config: &'static AppConfig,
    sender: Sender<MemeJob>,
) {
    let _ = bot
        .set_my_commands(comand::Command::bot_commands())
        .await
        .expect("Cannot set commands");

    let handler = dptree::entry()
        .branch(Update::filter_message().endpoint(message_handler))
        .branch(Update::filter_callback_query().endpoint(callback_handler));

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![state, sender, app_config])
        .enable_ctrlc_handler()
        .build()
        .dispatch()
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
    } else if let Some(text) = msg.text() {
        if text.starts_with('/') {
            match comand::Command::parse(text, "") {
                Ok(comand::Command::Start) => {
                    comand::start(bot, msg).await?;
                }
                Ok(comand::Command::Help) => {
                    comand::help(bot, msg).await?;
                }
                Ok(comand::Command::Stats) => {
                    comand::stats(bot, msg, state).await?;
                }
                Ok(comand::Command::Random) => {
                    comand::random(bot, msg, state).await?;
                }
                Ok(comand::Command::Settings) => {
                    comand::settings(bot, msg, state).await?;
                }
                Err(_) => {
                    comand::error(bot, msg).await?;
                }
            }
            return Ok(());
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

        if let Some(msg) = callback_query.regular_message() {
            if let Err(e) = bot
                .edit_message_text(msg.chat.id, msg.id, text)
                .reply_markup(keyboard)
                .await
            {
                eprintln!("[WARN] Failed to edit message: {}", e);
            }
        }
    }
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
