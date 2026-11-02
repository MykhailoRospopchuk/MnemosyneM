use std::sync::Arc;

use teloxide::Bot;
use teloxide::RequestError;
use teloxide::prelude::*;
use teloxide::types::FileId;
use teloxide::types::InputFile;
use teloxide::types::Message;
use teloxide::utils::command::BotCommands;

use crate::AppState;

#[derive(BotCommands, Clone)]
#[command(
    rename_rule = "lowercase",
    description = "These commands are supported:"
)]
pub enum Command {
    #[command(description = "Start a new bot")]
    Start,
    #[command(description = "Show list of all commands")]
    Help,
    #[command(description = "Get statistics of memes")]
    Stats,
    #[command(description = "Show meme random meme")]
    Random,
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
        .await?;

    Ok(message)
}

pub async fn help(bot: Bot, msg: Message) -> Result<Message, RequestError> {
    let message = bot
        .send_message(msg.chat.id, Command::descriptions().to_string())
        .await?;

    Ok(message)
}

pub async fn stats(bot: Bot, msg: Message, state: Arc<AppState>) -> Result<Message, RequestError> {
    let meme_store = state.meme_store.lock().await;

    let (public, private) = meme_store.len(msg.chat.id.0);

    let message = bot
        .send_message(
            msg.chat.id,
            format!(
                "In storage we have PUBLIC of pure haos: [{}] and PRIVATE best of the best trash: [{}] memes",
                public, private
            ),
        )
        .await?;

    Ok(message)
}

pub async fn random(bot: Bot, msg: Message, state: Arc<AppState>) -> Result<Message, RequestError> {
    let message: Message;

    let meme_store = state.meme_store.lock().await;
    if let Some(meme) = meme_store.random_meme(msg.chat.id.0) {
        if !meme.file_tg_id.is_empty() {
            message = bot
                .send_photo(
                    msg.chat.id,
                    InputFile::file_id(FileId(meme.file_tg_id.clone())),
                )
                .caption(format!("🎲 Random meme"))
                .await?;
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
