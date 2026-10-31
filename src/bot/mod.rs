pub mod comand;
pub mod handlers;

use crate::AppState;
use crate::config::AppConfig;
use crate::worker::MemeJob;

use std::sync::Arc;
use teloxide::Bot;
use teloxide::prelude::*;
use teloxide::types::Message;
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

    teloxide::repl(bot, move |bot: Bot, msg: Message| {
        let state = Arc::clone(&state);
        let sender = sender.clone();

        async move {
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
    })
    .await;
}
