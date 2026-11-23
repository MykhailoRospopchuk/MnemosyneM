use std::sync::Arc;

use teloxide::Bot;
use teloxide::prelude::*;
use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup};
use tracing::{error, warn};

use crate::AppState;
use crate::meme_store::DeleteMemeOutcome;

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
}
