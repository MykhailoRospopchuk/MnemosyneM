use std::sync::Arc;

use teloxide::Bot;
use teloxide::prelude::*;
use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup, ParseMode};
use tracing::{error, warn};

use crate::AppState;
use crate::meme_store::{DeleteMemeOutcome, OwnedMemeEntry};

fn build_card_header(
    user_is_public: bool,
    effective_is_public: bool,
    tags: &[(String, String)],
) -> String {
    let vis_line = if user_is_public {
        "Видимість: 🌐 Публічний (у загальному каталозі)"
    } else if effective_is_public {
        "Видимість: 🔒 Приватний у твоїй колекції (інший власник зробив публічним)"
    } else {
        "Видимість: 🔒 Приватний (тільки для тебе)"
    };

    let tags_line = if tags.is_empty() {
        String::new()
    } else {
        let joined: Vec<String> = tags
            .iter()
            .map(|(slug, emoji)| format!("{} #{}", emoji, slug))
            .collect();
        format!("\nТеги: {}", joined.join("  "))
    };

    format!("✅ Мем збережено!\n{}{}", vis_line, tags_line)
}

pub fn format_uploaded_meme_card(
    meme_id: u64,
    user_is_public: bool,
    effective_is_public: bool,
    tags: &[(String, String)],
) -> (String, InlineKeyboardMarkup) {
    let text = build_card_header(user_is_public, effective_is_public, tags);
    let vis_btn = if user_is_public {
        InlineKeyboardButton::callback(
            "🔒 Зробити приватним",
            format!("meme:vis:{}:priv", meme_id),
        )
    } else {
        InlineKeyboardButton::callback("🌐 Зробити публічним", format!("meme:vis:{}:pub", meme_id))
    };

    let kb = InlineKeyboardMarkup::new(vec![vec![
        vis_btn,
        InlineKeyboardButton::callback("🗑 Видалити", format!("meme:del_ask:{}", meme_id)),
    ]]);

    (text, kb)
}

pub fn format_delete_confirm_card(
    meme_id: u64,
    user_is_public: bool,
    effective_is_public: bool,
    tags: &[(String, String)],
) -> (String, InlineKeyboardMarkup) {
    let header = build_card_header(user_is_public, effective_is_public, tags);
    let text = format!("{}\n\n⚠️ Видалити цей мем?", header);
    let kb = InlineKeyboardMarkup::new(vec![vec![
        InlineKeyboardButton::callback(
            "✅ Так, видалити",
            format!("meme:del_confirm:{}", meme_id),
        ),
        InlineKeyboardButton::callback("↩️ Скасувати", format!("meme:del_cancel:{}", meme_id)),
    ]]);

    (text, kb)
}

pub async fn handle_meme_callback(
    bot: Bot,
    callback_query: CallbackQuery,
    state: Arc<AppState>,
    data: &str,
) -> ResponseResult<()> {
    let user_id = callback_query.from.id.0 as i64;
    let callback_id = callback_query.id.clone();

    enum CardUpdate {
        Edit {
            text: String,
            kb: InlineKeyboardMarkup,
            toast: Option<&'static str>,
        },
        Unavailable {
            toast: &'static str,
        },
    }

    let parts: Vec<&str> = data.split(':').collect();
    let update = match parts.as_slice() {
        ["meme", "vis", id_str, mode] if matches!(*mode, "pub" | "priv") => {
            let Some(meme_id) = id_str.parse::<u64>().ok() else {
                let _ = bot.answer_callback_query(callback_id).await;
                return Ok(());
            };
            let target_public = *mode == "pub";
            let res = {
                let mut store = state.meme_store.lock().await;
                store.set_user_meme_visibility(user_id, meme_id, target_public)
            };
            match res {
                Ok(Some(info)) => {
                    let (text, kb) = format_uploaded_meme_card(
                        meme_id,
                        info.user_is_public,
                        info.effective_is_public,
                        &info.tags,
                    );
                    let toast = if info.user_is_public {
                        "🌐 Тепер публічний"
                    } else {
                        "🔒 Тепер приватний"
                    };
                    CardUpdate::Edit {
                        text,
                        kb,
                        toast: Some(toast),
                    }
                }
                Ok(None) => CardUpdate::Unavailable {
                    toast: "ℹ️ Мем вже видалено або недоступний",
                },
                Err(e) => {
                    error!(user_id, meme_id, error = %e, "Failed to update meme visibility");
                    let _ = bot
                        .answer_callback_query(callback_id)
                        .text("⚠️ Не вдалося змінити видимість")
                        .await;
                    return Ok(());
                }
            }
        }
        ["meme", "del_ask", id_str] | ["meme", "del_cancel", id_str] => {
            let Some(meme_id) = id_str.parse::<u64>().ok() else {
                let _ = bot.answer_callback_query(callback_id).await;
                return Ok(());
            };
            let is_ask = parts[1] == "del_ask";
            let info_opt = {
                let store = state.meme_store.lock().await;
                store.get_meme_card_info(user_id, meme_id)
            };
            match info_opt {
                Some(info) => {
                    let (text, kb) = if is_ask {
                        format_delete_confirm_card(
                            meme_id,
                            info.user_is_public,
                            info.effective_is_public,
                            &info.tags,
                        )
                    } else {
                        format_uploaded_meme_card(
                            meme_id,
                            info.user_is_public,
                            info.effective_is_public,
                            &info.tags,
                        )
                    };
                    CardUpdate::Edit {
                        text,
                        kb,
                        toast: None,
                    }
                }
                None => CardUpdate::Unavailable {
                    toast: "ℹ️ Мем вже видалено або недоступний",
                },
            }
        }
        ["meme", "del_confirm", id_str] => {
            let Some(meme_id) = id_str.parse::<u64>().ok() else {
                let _ = bot.answer_callback_query(callback_id).await;
                return Ok(());
            };
            let outcome = {
                let mut store = state.meme_store.lock().await;
                store.delete_user_meme(user_id, meme_id)
            };
            match outcome {
                Ok(DeleteMemeOutcome::DeletedCompletely) => CardUpdate::Edit {
                    text: "🗑 Мем видалено з бази.".to_string(),
                    kb: InlineKeyboardMarkup::default(),
                    toast: Some("🗑 Мем видалено"),
                },
                Ok(DeleteMemeOutcome::RemovedFromCollection) => CardUpdate::Edit {
                    text: "🗑 Прибрано з твоєї колекції.".to_string(),
                    kb: InlineKeyboardMarkup::default(),
                    toast: Some("🗑 Прибрано з твоєї колекції"),
                },
                Ok(DeleteMemeOutcome::NotFoundOrNotOwner) => CardUpdate::Unavailable {
                    toast: "ℹ️ Мем вже видалено або недоступний",
                },
                Err(e) => {
                    error!(user_id, meme_id, error = %e, "Failed to delete meme");
                    let _ = bot
                        .answer_callback_query(callback_id)
                        .text("⚠️ Не вдалося видалити мем")
                        .await;
                    return Ok(());
                }
            }
        }
        _ => {
            let _ = bot.answer_callback_query(callback_id).await;
            return Ok(());
        }
    };

    match update {
        CardUpdate::Edit { text, kb, toast } => {
            let ans = bot.answer_callback_query(callback_id);
            let ans_res = if let Some(msg) = toast {
                ans.text(msg).await
            } else {
                ans.await
            };
            if let Err(e) = ans_res {
                warn!(user_id, error = %e, "Failed to answer meme action callback");
            }

            if let Some(msg) = callback_query.regular_message()
                && let Err(e) = bot
                    .edit_message_text(msg.chat.id, msg.id, text)
                    .reply_markup(kb)
                    .await
                && !e.to_string().contains("message is not modified")
            {
                warn!(user_id, error = %e, "Failed to edit uploaded meme card");
            }
        }
        CardUpdate::Unavailable { toast } => {
            if let Err(e) = bot.answer_callback_query(callback_id).text(toast).await {
                warn!(user_id, error = %e, "Failed to answer unavailable meme callback");
            }
            if let Some(msg) = callback_query.regular_message()
                && let Err(e) = bot
                    .edit_message_reply_markup(msg.chat.id, msg.id)
                    .reply_markup(InlineKeyboardMarkup::default())
                    .await
                && !e.to_string().contains("message is not modified")
            {
                warn!(user_id, error = %e, "Failed to clear unavailable meme markup");
            }
        }
    }

    Ok(())
}

pub fn my_caption(entry: &OwnedMemeEntry, idx: usize, total: usize) -> String {
    let vis_badge = if entry.user_is_public {
        "🌐 Публічний"
    } else if entry.meme.is_public {
        "🔒 Приватний (у каталозі іншим)"
    } else {
        "🔒 Приватний"
    };

    let source_html = crate::bot::tinder::format_source_html(&entry.meme);
    let tags_line = if entry.tags.is_empty() {
        String::new()
    } else {
        let formatted: Vec<String> = entry
            .tags
            .iter()
            .map(|(slug, emoji)| {
                format!(
                    "{} #{}",
                    crate::bot::tinder::escape_html(emoji),
                    crate::bot::tinder::escape_html(slug)
                )
            })
            .collect();
        format!("\n🏷️ {}", formatted.join(" "))
    };

    format!(
        "🗂 Мої меми ({}/{}) · {}{}{}",
        idx + 1,
        total,
        vis_badge,
        source_html,
        tags_line
    )
}

pub fn my_keyboard(
    entry: &OwnedMemeEntry,
    idx: usize,
    total: usize,
    confirm_delete: bool,
) -> InlineKeyboardMarkup {
    if confirm_delete {
        return InlineKeyboardMarkup::new(vec![vec![
            InlineKeyboardButton::callback(
                "✅ Так, видалити",
                format!("my:del_confirm:{}:{}", entry.meme.id, idx),
            ),
            InlineKeyboardButton::callback(
                "↩️ Скасувати",
                format!("my:del_cancel:{}:{}", entry.meme.id, idx),
            ),
        ]]);
    }

    let mut rows = Vec::new();
    if total > 1 {
        let prev = if idx == 0 { total - 1 } else { idx - 1 };
        let next = (idx + 1) % total;
        rows.push(vec![
            InlineKeyboardButton::callback("⬅️", format!("my:page:{}", prev)),
            InlineKeyboardButton::callback(format!("{}/{}", idx + 1, total), "my:noop"),
            InlineKeyboardButton::callback("➡️", format!("my:page:{}", next)),
        ]);
    }

    let vis_btn = if entry.user_is_public {
        InlineKeyboardButton::callback(
            "🔒 Приватним",
            format!("my:vis:{}:priv:{}", entry.meme.id, idx),
        )
    } else {
        InlineKeyboardButton::callback(
            "🌐 Публічним",
            format!("my:vis:{}:pub:{}", entry.meme.id, idx),
        )
    };

    rows.push(vec![
        vis_btn,
        InlineKeyboardButton::callback(
            "🗑 Видалити",
            format!("my:del_ask:{}:{}", entry.meme.id, idx),
        ),
    ]);

    InlineKeyboardMarkup::new(rows)
}

pub async fn cmd_my(bot: Bot, msg: Message, state: Arc<AppState>) -> ResponseResult<()> {
    let user_id = msg.chat.id.0;
    let owned = {
        let store = state.meme_store.lock().await;
        store.get_user_owned_memes(user_id)
    };

    if owned.is_empty() {
        bot.send_message(
            msg.chat.id,
            "🗂 Твоя колекція порожня. Надішли мем або GIF, або збережи вподобані через /liked!",
        )
        .await?;
        return Ok(());
    }

    let first = &owned[0];
    let total = owned.len();
    crate::bot::tinder::send_meme_with_markup(
        &bot,
        msg.chat.id,
        &first.meme,
        my_caption(first, 0, total),
        my_keyboard(first, 0, total, false),
    )
    .await?;

    Ok(())
}

pub async fn handle_my_callback(
    bot: Bot,
    callback_query: CallbackQuery,
    state: Arc<AppState>,
    data: &str,
) -> ResponseResult<()> {
    let user_id = callback_query.from.id.0 as i64;
    let callback_id = callback_query.id.clone();

    if data == "my:noop" {
        let _ = bot.answer_callback_query(callback_id).await;
        return Ok(());
    }

    let parts: Vec<&str> = data.split(':').collect();
    match parts.as_slice() {
        ["my", "page", idx_str] => {
            let target_idx = idx_str.parse::<usize>().unwrap_or(0);
            let owned = {
                let store = state.meme_store.lock().await;
                store.get_user_owned_memes(user_id)
            };
            let _ = bot.answer_callback_query(callback_id).await;

            if let Some(msg) = callback_query.regular_message() {
                if owned.is_empty() {
                    let _ = bot
                        .edit_message_caption(msg.chat.id, msg.id)
                        .caption("🗂 Твоя колекція порожня.")
                        .reply_markup(InlineKeyboardMarkup::default())
                        .await;
                } else {
                    let idx = target_idx.min(owned.len() - 1);
                    let entry = &owned[idx];
                    if let Err(e) = bot
                        .edit_message_media(
                            msg.chat.id,
                            msg.id,
                            crate::bot::tinder::build_input_media(
                                &entry.meme,
                                my_caption(entry, idx, owned.len()),
                            ),
                        )
                        .reply_markup(my_keyboard(entry, idx, owned.len(), false))
                        .await
                        && !e.to_string().contains("message is not modified")
                    {
                        warn!(user_id, meme_id = entry.meme.id, error = %e, "Failed to paginate /my meme");
                    }
                }
            }
        }
        ["my", "vis", id_str, mode, idx_str] if matches!(*mode, "pub" | "priv") => {
            let Some(meme_id) = id_str.parse::<u64>().ok() else {
                let _ = bot.answer_callback_query(callback_id).await;
                return Ok(());
            };
            let idx_hint = idx_str.parse::<usize>().unwrap_or(0);
            let target_public = *mode == "pub";

            let (res, owned) = {
                let mut store = state.meme_store.lock().await;
                let res = store.set_user_meme_visibility(user_id, meme_id, target_public);
                let owned = store.get_user_owned_memes(user_id);
                (res, owned)
            };

            let toast = match res {
                Ok(Some(info)) => {
                    if info.user_is_public {
                        "🌐 Тепер публічний"
                    } else {
                        "🔒 Тепер приватний"
                    }
                }
                Ok(None) => "ℹ️ Мем вже видалено або недоступний",
                Err(ref e) => {
                    error!(user_id, meme_id, error = %e, "Failed to toggle /my visibility");
                    "⚠️ Не вдалося змінити видимість"
                }
            };
            let _ = bot.answer_callback_query(callback_id).text(toast).await;

            if let Some(msg) = callback_query.regular_message() {
                if owned.is_empty() {
                    let _ = bot
                        .edit_message_caption(msg.chat.id, msg.id)
                        .caption("🗂 Твоя колекція порожня.")
                        .reply_markup(InlineKeyboardMarkup::default())
                        .await;
                } else {
                    let pos = owned
                        .iter()
                        .position(|e| e.meme.id == meme_id)
                        .unwrap_or_else(|| idx_hint.min(owned.len() - 1));
                    let entry = &owned[pos];
                    if let Err(e) = bot
                        .edit_message_caption(msg.chat.id, msg.id)
                        .caption(my_caption(entry, pos, owned.len()))
                        .parse_mode(ParseMode::Html)
                        .reply_markup(my_keyboard(entry, pos, owned.len(), false))
                        .await
                        && !e.to_string().contains("message is not modified")
                    {
                        warn!(user_id, meme_id, error = %e, "Failed to update /my caption after visibility change");
                    }
                }
            }
        }
        ["my", "del_ask", id_str, idx_str] | ["my", "del_cancel", id_str, idx_str] => {
            let Some(meme_id) = id_str.parse::<u64>().ok() else {
                let _ = bot.answer_callback_query(callback_id).await;
                return Ok(());
            };
            let idx_hint = idx_str.parse::<usize>().unwrap_or(0);
            let confirm_delete = parts[1] == "del_ask";

            let owned = {
                let store = state.meme_store.lock().await;
                store.get_user_owned_memes(user_id)
            };
            let _ = bot.answer_callback_query(callback_id).await;

            if let Some(msg) = callback_query.regular_message() {
                if let Some(pos) = owned.iter().position(|e| e.meme.id == meme_id) {
                    let entry = &owned[pos];
                    if let Err(e) = bot
                        .edit_message_reply_markup(msg.chat.id, msg.id)
                        .reply_markup(my_keyboard(entry, pos, owned.len(), confirm_delete))
                        .await
                        && !e.to_string().contains("message is not modified")
                    {
                        warn!(user_id, meme_id, error = %e, "Failed to toggle /my delete confirmation");
                    }
                } else if !owned.is_empty() {
                    let pos = idx_hint.min(owned.len() - 1);
                    let entry = &owned[pos];
                    let _ = bot
                        .edit_message_media(
                            msg.chat.id,
                            msg.id,
                            crate::bot::tinder::build_input_media(
                                &entry.meme,
                                my_caption(entry, pos, owned.len()),
                            ),
                        )
                        .reply_markup(my_keyboard(entry, pos, owned.len(), false))
                        .await;
                } else {
                    let _ = bot
                        .edit_message_caption(msg.chat.id, msg.id)
                        .caption("🗂 Твоя колекція порожня.")
                        .reply_markup(InlineKeyboardMarkup::default())
                        .await;
                }
            }
        }
        ["my", "del_confirm", id_str, idx_str] => {
            let Some(meme_id) = id_str.parse::<u64>().ok() else {
                let _ = bot.answer_callback_query(callback_id).await;
                return Ok(());
            };
            let idx_hint = idx_str.parse::<usize>().unwrap_or(0);

            let (outcome, owned) = {
                let mut store = state.meme_store.lock().await;
                let outcome = store.delete_user_meme(user_id, meme_id);
                let owned = store.get_user_owned_memes(user_id);
                (outcome, owned)
            };

            let toast = match outcome {
                Ok(DeleteMemeOutcome::DeletedCompletely) => "🗑 Мем видалено",
                Ok(DeleteMemeOutcome::RemovedFromCollection) => "🗑 Прибрано з твоєї колекції",
                Ok(DeleteMemeOutcome::NotFoundOrNotOwner) => "ℹ️ Мем вже видалено або недоступний",
                Err(ref e) => {
                    error!(user_id, meme_id, error = %e, "Failed to delete /my meme");
                    "⚠️ Не вдалося видалити мем"
                }
            };
            let _ = bot.answer_callback_query(callback_id).text(toast).await;

            if let Some(msg) = callback_query.regular_message() {
                if owned.is_empty() {
                    if let Err(e) = bot
                        .edit_message_caption(msg.chat.id, msg.id)
                        .caption("🗂 Твоя колекція тепер порожня.")
                        .reply_markup(InlineKeyboardMarkup::default())
                        .await
                        && !e.to_string().contains("message is not modified")
                    {
                        warn!(user_id, error = %e, "Failed to clear empty /my caption");
                    }
                } else {
                    let pos = idx_hint.min(owned.len() - 1);
                    let entry = &owned[pos];
                    if let Err(e) = bot
                        .edit_message_media(
                            msg.chat.id,
                            msg.id,
                            crate::bot::tinder::build_input_media(
                                &entry.meme,
                                my_caption(entry, pos, owned.len()),
                            ),
                        )
                        .reply_markup(my_keyboard(entry, pos, owned.len(), false))
                        .await
                        && !e.to_string().contains("message is not modified")
                    {
                        warn!(user_id, meme_id = entry.meme.id, error = %e, "Failed to show next /my meme");
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

#[cfg(test)]
mod tests {
    use super::*;
    use teloxide::types::InlineKeyboardButtonKind;

    #[test]
    fn uploaded_meme_card_switches_visibility_button_and_renders_tags() {
        let tags = vec![
            ("коти".to_string(), "🐱".to_string()),
            ("айті".to_string(), "💻".to_string()),
        ];

        let (priv_text, priv_kb) = format_uploaded_meme_card(77, false, false, &tags);
        assert!(priv_text.contains("🔒 Приватний (тільки для тебе)"));
        assert!(priv_text.contains("Теги: 🐱 #коти  💻 #айті"));
        assert_eq!(priv_kb.inline_keyboard.len(), 1);
        assert_eq!(priv_kb.inline_keyboard[0][0].text, "🌐 Зробити публічним");
        assert_eq!(
            priv_kb.inline_keyboard[0][0].kind,
            InlineKeyboardButtonKind::CallbackData("meme:vis:77:pub".to_string())
        );
        assert_eq!(
            priv_kb.inline_keyboard[0][1].kind,
            InlineKeyboardButtonKind::CallbackData("meme:del_ask:77".to_string())
        );

        let (pub_text, pub_kb) = format_uploaded_meme_card(77, true, true, &[]);
        assert!(pub_text.contains("🌐 Публічний (у загальному каталозі)"));
        assert!(!pub_text.contains("Теги:"));
        assert_eq!(pub_kb.inline_keyboard[0][0].text, "🔒 Зробити приватним");
        assert_eq!(
            pub_kb.inline_keyboard[0][0].kind,
            InlineKeyboardButtonKind::CallbackData("meme:vis:77:priv".to_string())
        );
    }

    #[test]
    fn uploaded_meme_card_notes_shared_public_and_formats_delete_confirm() {
        let (shared_text, _) = format_uploaded_meme_card(99, false, true, &[]);
        assert!(shared_text.contains("інший власник зробив публічним"));

        let (del_text, del_kb) = format_delete_confirm_card(99, true, true, &[]);
        assert!(del_text.contains("⚠️ Видалити цей мем?"));
        assert_eq!(del_kb.inline_keyboard.len(), 1);
        assert_eq!(
            del_kb.inline_keyboard[0][0].kind,
            InlineKeyboardButtonKind::CallbackData("meme:del_confirm:99".to_string())
        );
        assert_eq!(
            del_kb.inline_keyboard[0][1].kind,
            InlineKeyboardButtonKind::CallbackData("meme:del_cancel:99".to_string())
        );
    }

    #[test]
    fn my_caption_and_keyboard_render_visibility_tags_and_confirm() {
        use crate::database::entity::{media_type::MediaType, meme::Meme};

        let entry = OwnedMemeEntry {
            meme: Meme {
                id: 55,
                title: String::new(),
                ocr_text: String::new(),
                embedding_uk: Vec::new(),
                embedding_en: Vec::new(),
                file_tg_id: "tg".to_string(),
                file_tg_unique_id: "u".to_string(),
                source_title: "WTF & IT".to_string(),
                source_url: "https://t.me/WTF_UKR/55".to_string(),
                is_public: true,
                owners: vec![1, 2],
                phash: 0,
                media_type: MediaType::Photo,
                thumbnail_tg_id: String::new(),
                tags: vec!["айті".to_string()],
            },
            user_is_public: false,
            tags: vec![("айті".to_string(), "💻".to_string())],
        };

        let cap = my_caption(&entry, 1, 5);
        assert!(cap.contains("🗂 Мої меми (2/5) · 🔒 Приватний (у каталозі іншим)"));
        assert!(cap.contains("<a href=\"https://t.me/WTF_UKR/55\">WTF &amp; IT</a>"));
        assert!(cap.contains("🏷️ 💻 #айті"));

        let kb = my_keyboard(&entry, 1, 5, false);
        assert_eq!(kb.inline_keyboard.len(), 2);
        assert_eq!(
            kb.inline_keyboard[0][0].kind,
            InlineKeyboardButtonKind::CallbackData("my:page:0".to_string())
        );
        assert_eq!(
            kb.inline_keyboard[0][2].kind,
            InlineKeyboardButtonKind::CallbackData("my:page:2".to_string())
        );
        assert_eq!(
            kb.inline_keyboard[1][0].kind,
            InlineKeyboardButtonKind::CallbackData("my:vis:55:pub:1".to_string())
        );
        assert_eq!(
            kb.inline_keyboard[1][1].kind,
            InlineKeyboardButtonKind::CallbackData("my:del_ask:55:1".to_string())
        );

        let confirm_kb = my_keyboard(&entry, 1, 5, true);
        assert_eq!(confirm_kb.inline_keyboard.len(), 1);
        assert_eq!(
            confirm_kb.inline_keyboard[0][0].kind,
            InlineKeyboardButtonKind::CallbackData("my:del_confirm:55:1".to_string())
        );
        assert_eq!(
            confirm_kb.inline_keyboard[0][1].kind,
            InlineKeyboardButtonKind::CallbackData("my:del_cancel:55:1".to_string())
        );
    }
}
