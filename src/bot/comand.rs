use std::sync::Arc;

use teloxide::Bot;
use teloxide::RequestError;
use teloxide::prelude::*;
use teloxide::types::FileId;
use teloxide::types::InlineKeyboardButton;
use teloxide::types::InlineKeyboardMarkup;
use teloxide::types::InputFile;
use teloxide::types::Message;
use teloxide::utils::command::BotCommands;

use crate::AppState;
use crate::bot::handlers::build_caption;
use crate::database::entity::media_type::MediaType;
use crate::db::UserSetting;

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
    #[command(description = "Set up your stuff")]
    Settings,
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

    let text = format!("[S] User settings:\n");

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
