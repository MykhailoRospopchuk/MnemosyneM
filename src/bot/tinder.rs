use std::sync::Arc;

use teloxide::Bot;
use teloxide::prelude::*;
use teloxide::types::{
    FileId, InlineKeyboardButton, InlineKeyboardMarkup, InputFile, InputMedia,
    InputMediaAnimation, InputMediaPhoto, Message,
};

use crate::AppState;
use crate::database::entity::media_type::MediaType;
use crate::database::entity::meme::Meme;
use tracing::{error, warn};

fn format_source_line(meme: &Meme) -> String {
    if meme.source_url.is_empty() {
        String::new()
    } else if meme.source_title.is_empty() {
        format!("\n📎 ({})", meme.source_url)
    } else {
        format!("\n📎 [{}] ({})", meme.source_title, meme.source_url)
    }
}

fn build_input_media(meme: &Meme, caption: String) -> InputMedia {
    let file = InputFile::file_id(FileId(meme.file_tg_id.clone()));
    match meme.media_type {
        MediaType::Photo => InputMedia::Photo(InputMediaPhoto::new(file).caption(caption)),
        MediaType::Animation => {
            InputMedia::Animation(InputMediaAnimation::new(file).caption(caption))
        }
    }
}

async fn send_meme_with_markup(
    bot: &Bot,
    chat_id: ChatId,
    meme: &Meme,
    caption: String,
    keyboard: InlineKeyboardMarkup,
) -> ResponseResult<Message> {
    let file = InputFile::file_id(FileId(meme.file_tg_id.clone()));
    match meme.media_type {
        MediaType::Photo => {
            bot.send_photo(chat_id, file)
                .caption(caption)
                .reply_markup(keyboard)
                .await
        }
        MediaType::Animation => {
            bot.send_animation(chat_id, file)
                .caption(caption)
                .reply_markup(keyboard)
                .await
        }
    }
}

fn tinder_caption(meme: &Meme, remaining: usize) -> String {
    format!(
        "🔥 Мем-Тіндер | Залишилось: {}{}",
        remaining,
        format_source_line(meme)
    )
}

fn tinder_keyboard(meme_id: u64) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![vec![
        InlineKeyboardButton::callback("❤️ Лайк", format!("tinder:like:{}", meme_id)),
        InlineKeyboardButton::callback("👎", format!("tinder:dislike:{}", meme_id)),
        InlineKeyboardButton::callback("⏭️ Скіп", format!("tinder:skip:{}", meme_id)),
    ]])
}

fn reset_skips_keyboard() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::callback(
        "🔄 Скинути скіпи",
        "tinder:reset_skips",
    )]])
}

fn liked_caption(meme: &Meme, idx: usize, total: usize) -> String {
    format!(
        "❤️ Вподобані ({}/{}){}",
        idx + 1,
        total,
        format_source_line(meme)
    )
}

fn liked_keyboard(meme: &Meme, user_id: i64, idx: usize, total: usize) -> InlineKeyboardMarkup {
    let mut nav_row = Vec::new();
    if total > 1 {
        let prev = if idx == 0 { total - 1 } else { idx - 1 };
        nav_row.push(InlineKeyboardButton::callback(
            "⬅️",
            format!("liked:page:{}", prev),
        ));
    }
    nav_row.push(InlineKeyboardButton::callback(
        "💔 Прибрати",
        format!("liked:unlike:{}:{}", meme.id, idx),
    ));
    if total > 1 {
        let next = (idx + 1) % total;
        nav_row.push(InlineKeyboardButton::callback(
            "➡️",
            format!("liked:page:{}", next),
        ));
    }

    let save_row = if meme.is_owned_by(user_id) {
        vec![InlineKeyboardButton::callback(
            "✅ Вже у твоїх мемах",
            "liked:noop",
        )]
    } else {
        vec![InlineKeyboardButton::callback(
            "📥 Зберегти в мої меми",
            format!("liked:save:{}:{}", meme.id, idx),
        )]
    };

    InlineKeyboardMarkup::new(vec![nav_row, save_row])
}

pub async fn cmd_tinder(bot: Bot, msg: Message, state: Arc<AppState>) -> ResponseResult<()> {
    let user_id = msg.chat.id.0;
    let (next, skips) = {
        let store = state.meme_store.lock().await;
        let next = store.get_next_tinder_meme(user_id);
        let skips = if next.is_none() {
            store.count_user_skips(user_id)
        } else {
            0
        };
        (next, skips)
    };

    if let Some((meme, remaining)) = next {
        send_meme_with_markup(
            &bot,
            msg.chat.id,
            &meme,
            tinder_caption(&meme, remaining),
            tinder_keyboard(meme.id),
        )
        .await?;
    } else if skips > 0 {
        bot.send_message(
            msg.chat.id,
            format!(
                "🎉 Усі меми вже оцінено! (Пропущених: {}). Можеш скинути скіпи і переглянути їх ще раз.",
                skips
            ),
        )
        .reply_markup(reset_skips_keyboard())
        .await?;
    } else {
        bot.send_message(
            msg.chat.id,
            "🎉 Ти оцінив(ла) усі доступні меми! Завантаж нові, заглянь пізніше або скинь реакції в /settings.",
        )
        .await?;
    }

    Ok(())
}

pub async fn cmd_liked(bot: Bot, msg: Message, state: Arc<AppState>) -> ResponseResult<()> {
    let user_id = msg.chat.id.0;
    let liked = {
        let store = state.meme_store.lock().await;
        store.get_liked_memes(user_id)
    };

    if liked.is_empty() {
        bot.send_message(
            msg.chat.id,
            "💔 Поки що немає вподобаних мемів. Зазирни у /tinder!",
        )
        .await?;
        return Ok(());
    }

    let first = &liked[0];
    let total = liked.len();
    send_meme_with_markup(
        &bot,
        msg.chat.id,
        first,
        liked_caption(first, 0, total),
        liked_keyboard(first, user_id, 0, total),
    )
    .await?;

    Ok(())
}

pub async fn handle_tinder_callback(
    bot: Bot,
    callback_query: CallbackQuery,
    state: Arc<AppState>,
    data: &str,
) -> ResponseResult<()> {
    let user_id = callback_query.from.id.0 as i64;
    let callback_id = callback_query.id.clone();

    if data == "tinder:reset_skips" {
        let next = {
            let store = state.meme_store.lock().await;
            if let Err(e) = store.reset_user_skips(user_id) {
                error!(user_id, error = %e, "Failed to reset tinder skips");
            }
            store.get_next_tinder_meme(user_id)
        };

        if let Err(e) = bot
            .answer_callback_query(callback_id)
            .text("🔄 Скіпи скинуто!")
            .await
        {
            warn!(user_id, error = %e, "Failed to answer tinder reset_skips callback");
        }

        if let Some(msg) = callback_query.regular_message() {
            match next {
                Some((meme, remaining)) => {
                    let caption = tinder_caption(&meme, remaining);
                    let kb = tinder_keyboard(meme.id);
                    if msg.photo().is_some() || msg.animation().is_some() {
                        if let Err(e) = bot
                            .edit_message_media(msg.chat.id, msg.id, build_input_media(&meme, caption))
                            .reply_markup(kb)
                            .await
                        {
                            warn!(user_id, meme_id = meme.id, error = %e, "Failed to edit tinder media after skip reset");
                        }
                    } else {
                        let _ = bot
                            .edit_message_text(msg.chat.id, msg.id, "🔄 Скіпи скинуто! Поїхали знову:")
                            .reply_markup(InlineKeyboardMarkup::default())
                            .await;
                        if let Err(e) =
                            send_meme_with_markup(&bot, msg.chat.id, &meme, caption, kb).await
                        {
                            warn!(user_id, meme_id = meme.id, error = %e, "Failed to send tinder meme after skip reset");
                        }
                    }
                }
                None => {
                    let text = "🎉 Більше немає мемів для перегляду.";
                    if msg.photo().is_some() || msg.animation().is_some() {
                        let _ = bot
                            .edit_message_caption(msg.chat.id, msg.id)
                            .caption(text)
                            .reply_markup(InlineKeyboardMarkup::default())
                            .await;
                    } else {
                        let _ = bot
                            .edit_message_text(msg.chat.id, msg.id, text)
                            .reply_markup(InlineKeyboardMarkup::default())
                            .await;
                    }
                }
            }
        }
        return Ok(());
    }

    let mut parts = data.splitn(3, ':');
    let _prefix = parts.next();
    let Some(action) = parts.next() else {
        let _ = bot.answer_callback_query(callback_id).await;
        return Ok(());
    };
    let Some(meme_id) = parts.next().and_then(|s| s.parse::<u64>().ok()) else {
        let _ = bot.answer_callback_query(callback_id).await;
        return Ok(());
    };

    if !matches!(action, "like" | "dislike" | "skip") {
        let _ = bot.answer_callback_query(callback_id).await;
        return Ok(());
    }

    let (next, skips) = {
        let store = state.meme_store.lock().await;
        if let Err(e) = store.record_reaction(user_id, meme_id, action) {
            error!(user_id, meme_id, action, error = %e, "Failed to record tinder reaction");
        }
        let next = store.get_next_tinder_meme(user_id);
        let skips = if next.is_none() {
            store.count_user_skips(user_id)
        } else {
            0
        };
        (next, skips)
    };

    let toast = match action {
        "like" => "❤️ Додано у /liked",
        "dislike" => "👎 Приховано",
        _ => "⏭️ Пропущено",
    };
    if let Err(e) = bot.answer_callback_query(callback_id).text(toast).await {
        warn!(user_id, meme_id, error = %e, "Failed to answer tinder callback query");
    }

    if let Some(msg) = callback_query.regular_message() {
        match next {
            Some((next_meme, remaining)) => {
                if let Err(e) = bot
                    .edit_message_media(
                        msg.chat.id,
                        msg.id,
                        build_input_media(&next_meme, tinder_caption(&next_meme, remaining)),
                    )
                    .reply_markup(tinder_keyboard(next_meme.id))
                    .await
                {
                    warn!(user_id, meme_id = next_meme.id, error = %e, "Failed to edit tinder media");
                }
            }
            None => {
                let (caption, kb) = if skips > 0 {
                    (
                        format!(
                            "🎉 Усі меми переглянуто! (Пропущених: {}). Натисни, щоб повернути скіпи.",
                            skips
                        ),
                        reset_skips_keyboard(),
                    )
                } else {
                    (
                        "🎉 Ти оцінив(ла) усі доступні меми! Зазирни у /liked або скинь реакції в /settings."
                            .to_string(),
                        InlineKeyboardMarkup::default(),
                    )
                };

                if let Err(e) = bot
                    .edit_message_caption(msg.chat.id, msg.id)
                    .caption(caption)
                    .reply_markup(kb)
                    .await
                {
                    warn!(user_id, error = %e, "Failed to edit tinder caption");
                }
            }
        }
    }

    Ok(())
}

pub async fn handle_liked_callback(
    bot: Bot,
    callback_query: CallbackQuery,
    state: Arc<AppState>,
    data: &str,
) -> ResponseResult<()> {
    let user_id = callback_query.from.id.0 as i64;
    let callback_id = callback_query.id.clone();

    if data == "liked:noop" {
        let _ = bot
            .answer_callback_query(callback_id)
            .text("Цей мем уже у твоїх мемах 👍")
            .await;
        return Ok(());
    }

    if let Some(idx_str) = data.strip_prefix("liked:page:") {
        let target_idx = idx_str.parse::<usize>().unwrap_or(0);
        let liked = {
            let store = state.meme_store.lock().await;
            store.get_liked_memes(user_id)
        };
        let _ = bot.answer_callback_query(callback_id).await;

        if let Some(msg) = callback_query.regular_message() {
            if liked.is_empty() {
                let _ = bot
                    .edit_message_caption(msg.chat.id, msg.id)
                    .caption("❤️ Список вподобаних порожній.")
                    .reply_markup(InlineKeyboardMarkup::default())
                    .await;
            } else {
                let idx = target_idx.min(liked.len() - 1);
                let meme = &liked[idx];
                if let Err(e) = bot
                    .edit_message_media(
                        msg.chat.id,
                        msg.id,
                        build_input_media(meme, liked_caption(meme, idx, liked.len())),
                    )
                    .reply_markup(liked_keyboard(meme, user_id, idx, liked.len()))
                    .await
                {
                    warn!(user_id, meme_id = meme.id, error = %e, "Failed to paginate liked media");
                }
            }
        }
        return Ok(());
    }

    let mut parts = data.splitn(4, ':');
    let _prefix = parts.next();
    let Some(action) = parts.next() else {
        let _ = bot.answer_callback_query(callback_id).await;
        return Ok(());
    };
    let Some(meme_id) = parts.next().and_then(|s| s.parse::<u64>().ok()) else {
        let _ = bot.answer_callback_query(callback_id).await;
        return Ok(());
    };
    let current_idx = parts
        .next()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);

    match action {
        "save" => {
            let (save_res, liked) = {
                let mut store = state.meme_store.lock().await;
                let save_res = store.link_existing_meme(meme_id, user_id, "");
                let liked = store.get_liked_memes(user_id);
                (save_res, liked)
            };

            let toast = match save_res {
                Ok(true) => "📥 Збережено в твою колекцію!",
                Ok(false) => "✅ Вже у твоїй колекції",
                Err(ref e) => {
                    error!(user_id, meme_id, error = %e, "Failed to link liked meme to collection");
                    "⚠️ Не вдалося зберегти"
                }
            };
            let _ = bot.answer_callback_query(callback_id).text(toast).await;

            if let Some(msg) = callback_query.regular_message()
                && !liked.is_empty()
            {
                let pos = liked
                    .iter()
                    .position(|m| m.id == meme_id)
                    .unwrap_or_else(|| current_idx.min(liked.len() - 1));
                let meme = &liked[pos];
                if let Err(e) = bot
                    .edit_message_reply_markup(msg.chat.id, msg.id)
                    .reply_markup(liked_keyboard(meme, user_id, pos, liked.len()))
                    .await
                {
                    warn!(user_id, meme_id = meme.id, error = %e, "Failed to refresh liked save markup");
                }
            }
        }
        "unlike" => {
            let liked = {
                let store = state.meme_store.lock().await;
                if let Err(e) = store.remove_reaction(user_id, meme_id) {
                    error!(user_id, meme_id, error = %e, "Failed to remove liked reaction");
                }
                store.get_liked_memes(user_id)
            };

            let _ = bot
                .answer_callback_query(callback_id)
                .text("💔 Прибрано з вподобаних")
                .await;

            if let Some(msg) = callback_query.regular_message() {
                if liked.is_empty() {
                    if let Err(e) = bot
                        .edit_message_caption(msg.chat.id, msg.id)
                        .caption("❤️ Список вподобаних тепер порожній.")
                        .reply_markup(InlineKeyboardMarkup::default())
                        .await
                    {
                        warn!(user_id, error = %e, "Failed to clear liked caption");
                    }
                } else {
                    let idx = current_idx.min(liked.len() - 1);
                    let meme = &liked[idx];
                    if let Err(e) = bot
                        .edit_message_media(
                            msg.chat.id,
                            msg.id,
                            build_input_media(meme, liked_caption(meme, idx, liked.len())),
                        )
                        .reply_markup(liked_keyboard(meme, user_id, idx, liked.len()))
                        .await
                    {
                        warn!(user_id, meme_id = meme.id, error = %e, "Failed to show next liked media");
                    }
                }
            }
        }
        _ => {
            let _ = bot.answer_callback_query(callback_id).await;
        }
    }

    Ok(())
}
