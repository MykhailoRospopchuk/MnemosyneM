use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use teloxide::Bot;
use teloxide::prelude::*;
use teloxide::types::{
    ChatId, InlineKeyboardButton, InlineKeyboardMarkup, InputFile, MessageId, ParseMode,
    ReplyParameters,
};
use tracing::{error, info, warn};

use crate::AppState;
use crate::bot::tinder::{build_input_media, escape_html, format_source_html, send_meme_with_markup};
use crate::config::AppConfig;
use crate::constants;
use crate::database::entity::media_type::MediaType;
use crate::database::entity::meme::Meme;
use crate::database::entity::new_meme::NewMeme;
use crate::helpers::telegram_bot_helper::download_telegram_file;
use crate::meme_store::{AdminStatsSummary, AdminTagInfo, ModerationQueueItem};
use crate::tagger;
use crate::vlm::{MemeAnalysis, ModelTier, VlmQuotaSummary};

const ADMIN_RECENT_LIMIT: usize = 50;
const ADMIN_TAGS_PER_PAGE: usize = 10;

pub fn format_admin_dashboard(
    summary: &AdminStatsSummary,
    quota: &VlmQuotaSummary,
) -> (String, InlineKeyboardMarkup) {
    let tier1_parts: Vec<String> = quota
        .models
        .iter()
        .filter(|m| m.tier == ModelTier::Tier1)
        .map(|m| {
            let short = m
                .id
                .strip_prefix("gemini-")
                .unwrap_or(m.id)
                .replace("-flash-lite", "-lite");
            format!("{}: {}/{}", short, m.used_today, m.rpd_limit)
        })
        .collect();

    let text = format!(
        "👑 Панель адміністратора\n\n\
         🗂 Мемів: {} (🌐 {} публічних / 🔒 {} приватних)\n\
         👥 Користувачів: {}\n\
         🚨 Черга модерації: {}\n\
         🏷️ Категорій: {}/40 активних (усього {})\n\
         📦 Невизначені (#інше -1): {} | Заблоковані (-3): {}\n\
         ⚡ Gemini VLM ({} PT): T1 {}/{} ({}) | T2 {}/{}",
        summary.total_memes,
        summary.public_memes,
        summary.private_memes,
        summary.total_users,
        summary.pending_moderation,
        summary.active_tags,
        summary.total_tags,
        summary.undetermined_memes,
        summary.blocked_tag_memes,
        quota.pt_day_key,
        quota.tier1_used,
        quota.tier1_limit,
        tier1_parts.join(", "),
        quota.tier2_used,
        quota.tier2_limit
    );

    let kb = InlineKeyboardMarkup::new(vec![
        vec![InlineKeyboardButton::callback(
            format!("🚨 Черга модерації ({})", summary.pending_moderation),
            "admin:queue:0",
        )],
        vec![
            InlineKeyboardButton::callback("🔎 Останні меми", "admin:recent:0"),
            InlineKeyboardButton::callback(
                format!("🏷️ Категорії ({})", summary.active_tags),
                "admin:tags:0",
            ),
        ],
        vec![
            InlineKeyboardButton::callback("💾 Бекап БД", "admin:backup"),
            InlineKeyboardButton::callback("🔄 Оновити", "admin:home"),
        ],
    ]);

    (text, kb)
}

pub fn format_moderation_caption(
    item: &ModerationQueueItem,
    nav: Option<(usize, usize)>,
) -> String {
    let header = match nav {
        Some((idx, total)) => format!("🚨 Черга ({}/{}) · #{}", idx + 1, total, item.id),
        None => format!("🚨 Черга модерації #{}", item.id),
    };
    let who = if item.username.is_empty() {
        format!("ID {}", item.user_id)
    } else {
        format!("@{} (ID {})", escape_html(&item.username), item.user_id)
    };
    let caption_line = if item.user_caption.trim().is_empty() {
        String::new()
    } else {
        format!("\n💬 {}", escape_html(item.user_caption.trim()))
    };

    let media_label = match item.media_type {
        MediaType::Photo => "фото",
        MediaType::Animation => "GIF",
    };

    format!(
        "<b>{}</b>\nВід: {}\nSafety: {:.0}% · {}\n{}{}",
        header,
        who,
        (item.safety_score * 100.0).clamp(0.0, 100.0),
        media_label,
        escape_html(&item.reason),
        caption_line
    )
}

pub fn moderation_keyboard(queue_id: i64, nav: Option<(usize, usize)>) -> InlineKeyboardMarkup {
    let mut rows = Vec::new();
    if let Some((idx, total)) = nav
        && total > 1
    {
        let prev = if idx == 0 { total - 1 } else { idx - 1 };
        let next = (idx + 1) % total;
        rows.push(vec![
            InlineKeyboardButton::callback("⬅️", format!("admin:queue:{}", prev)),
            InlineKeyboardButton::callback(format!("{}/{}", idx + 1, total), "admin:noop"),
            InlineKeyboardButton::callback("➡️", format!("admin:queue:{}", next)),
        ]);
    }

    rows.push(vec![
        InlineKeyboardButton::callback("✅ Публічний", format!("admin:mod:pub:{}", queue_id)),
        InlineKeyboardButton::callback("🔒 Приватний", format!("admin:mod:priv:{}", queue_id)),
        InlineKeyboardButton::callback("⛔ Відхилити", format!("admin:mod:rej:{}", queue_id)),
    ]);

    InlineKeyboardMarkup::new(rows)
}

fn format_admin_recent_caption(meme: &Meme, idx: usize, total: usize) -> String {
    let vis = if meme.is_public {
        "🌐 Публічний"
    } else {
        "🔒 Приватний"
    };
    let source = format_source_html(meme);
    let tags = if meme.tags.is_empty() {
        String::new()
    } else {
        let joined: Vec<String> = meme
            .tags
            .iter()
            .map(|t| format!("#{}", escape_html(t)))
            .collect();
        format!("\n🏷️ {}", joined.join(" "))
    };

    format!(
        "🔎 Каталог ({}/{}) · #{} · {} · власників: {}{}{}",
        idx + 1,
        total,
        meme.id,
        vis,
        meme.owners.len(),
        source,
        tags
    )
}

fn admin_recent_keyboard(
    meme: &Meme,
    idx: usize,
    total: usize,
    confirm_purge: bool,
) -> InlineKeyboardMarkup {
    if confirm_purge {
        return InlineKeyboardMarkup::new(vec![vec![
            InlineKeyboardButton::callback(
                "✅ Так, видалити назавжди",
                format!("admin:purge_confirm:{}:{}", meme.id, idx),
            ),
            InlineKeyboardButton::callback(
                "↩️ Скасувати",
                format!("admin:purge_cancel:{}:{}", meme.id, idx),
            ),
        ]]);
    }

    let mut rows = Vec::new();
    if total > 1 {
        let prev = if idx == 0 { total - 1 } else { idx - 1 };
        let next = (idx + 1) % total;
        rows.push(vec![
            InlineKeyboardButton::callback("⬅️", format!("admin:recent:{}", prev)),
            InlineKeyboardButton::callback(format!("{}/{}", idx + 1, total), "admin:noop"),
            InlineKeyboardButton::callback("➡️", format!("admin:recent:{}", next)),
        ]);
    }

    let vis_btn = if meme.is_public {
        InlineKeyboardButton::callback(
            "🔒 Зробити приватним",
            format!("admin:recent_vis:{}:priv:{}", meme.id, idx),
        )
    } else {
        InlineKeyboardButton::callback(
            "🌐 Зробити публічним",
            format!("admin:recent_vis:{}:pub:{}", meme.id, idx),
        )
    };

    rows.push(vec![
        vis_btn,
        InlineKeyboardButton::callback(
            "🗑 Видалити",
            format!("admin:purge_ask:{}:{}", meme.id, idx),
        ),
    ]);

    InlineKeyboardMarkup::new(rows)
}

pub fn format_admin_tags_panel(
    tags: &[AdminTagInfo],
    page: usize,
) -> (String, InlineKeyboardMarkup) {
    let total_pages = tags.len().div_ceil(ADMIN_TAGS_PER_PAGE).max(1);
    let safe_page = page.min(total_pages - 1);
    let start = safe_page * ADMIN_TAGS_PER_PAGE;
    let slice = &tags[start..tags.len().min(start + ADMIN_TAGS_PER_PAGE)];
    let active_count = tags.iter().filter(|t| t.is_active).count();

    let text = format!(
        "🏷️ Керування категоріями ({}/40 активних, сторінка {}/{})\n\
         Натисни на категорію, щоб увімкнути або вимкнути її в автотегуванні.",
        active_count,
        safe_page + 1,
        total_pages
    );

    let mut rows: Vec<Vec<InlineKeyboardButton>> = slice
        .chunks(2)
        .map(|pair| {
            pair.iter()
                .map(|t| {
                    let status = if t.is_active { "✅" } else { "⏸" };
                    let next_flag = if t.is_active { 0 } else { 1 };
                    InlineKeyboardButton::callback(
                        format!("{} {} #{} ({})", status, t.emoji, t.name, t.meme_count),
                        format!("admin:tag_toggle:{}:{}:{}", t.id, next_flag, safe_page),
                    )
                })
                .collect()
        })
        .collect();

    if total_pages > 1 {
        let prev = if safe_page == 0 {
            total_pages - 1
        } else {
            safe_page - 1
        };
        let next = (safe_page + 1) % total_pages;
        rows.push(vec![
            InlineKeyboardButton::callback("⬅️", format!("admin:tags:{}", prev)),
            InlineKeyboardButton::callback(
                format!("{}/{}", safe_page + 1, total_pages),
                "admin:noop",
            ),
            InlineKeyboardButton::callback("➡️", format!("admin:tags:{}", next)),
        ]);
    }

    rows.push(vec![InlineKeyboardButton::callback(
        "⬅️ Назад до панелі",
        "admin:home",
    )]);

    (text, InlineKeyboardMarkup::new(rows))
}

fn meme_from_moderation_item(item: &ModerationQueueItem, meme_id: u64, is_public: bool) -> Meme {
    Meme {
        id: meme_id,
        title: item.user_caption.clone(),
        ocr_text: String::new(),
        embedding_uk: Vec::new(),
        embedding_en: Vec::new(),
        file_tg_id: item.file_tg_id.clone(),
        file_tg_unique_id: item.file_tg_unique_id.clone(),
        source_title: item.source_title.clone(),
        source_url: item.source_url.clone(),
        is_public,
        owners: vec![item.user_id],
        phash: item.phash,
        media_type: item.media_type,
        thumbnail_tg_id: item.thumbnail_tg_id.clone(),
        tags: Vec::new(),
    }
}

async fn notify_uploader(bot: &Bot, item: &ModerationQueueItem, text: &str) {
    let mut req = bot.send_message(ChatId(item.chat_id), text);
    if item.message_id > 0 && item.message_id <= i32::MAX as i64 {
        req = req.reply_parameters(
            ReplyParameters::new(MessageId(item.message_id as i32)).allow_sending_without_reply(),
        );
    }
    if let Err(e) = req.await {
        warn!(
            user_id = item.user_id,
            queue_id = item.id,
            error = %e,
            "Failed to notify uploader of moderation decision"
        );
    }
}

pub async fn notify_admins_of_moderation(
    bot: &Bot,
    app_config: &AppConfig,
    item: &ModerationQueueItem,
) {
    let caption = format_moderation_caption(item, None);
    let kb = moderation_keyboard(item.id, None);
    let stub = meme_from_moderation_item(item, item.id.max(0) as u64, false);

    for &admin_id in &app_config.admin_ids {
        if let Err(e) = send_meme_with_markup(
            bot,
            ChatId(admin_id),
            &stub,
            caption.clone(),
            kb.clone(),
        )
        .await
        {
            warn!(
                admin_id,
                queue_id = item.id,
                error = %e,
                "Failed to send moderation item to admin"
            );
        }
    }
}

pub async fn cmd_admin(
    bot: Bot,
    msg: Message,
    state: Arc<AppState>,
    app_config: &'static AppConfig,
) -> ResponseResult<()> {
    let user_id = msg.from.as_ref().map(|u| u.id.0 as i64).unwrap_or(msg.chat.id.0);
    if !app_config.is_admin(user_id) {
        bot.send_message(msg.chat.id, "⛔ Доступ лише для адміністратора.")
            .await?;
        return Ok(());
    }

    let summary = {
        let store = state.meme_store.lock().await;
        store.get_admin_stats()
    };
    let quota = state.vlm_router.quota_snapshot().await;
    let (text, kb) = format_admin_dashboard(&summary, &quota);

    bot.send_message(msg.chat.id, text).reply_markup(kb).await?;
    Ok(())
}

async fn send_db_backup(bot: &Bot, chat_id: ChatId, state: Arc<AppState>) {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let filename = format!("mnemosynem-backup-{}.db", ts);
    let backup_path = std::env::temp_dir().join(&filename);

    let snapshot_res = {
        let store = state.meme_store.lock().await;
        store.backup_to(&backup_path)
    };

    if let Err(e) = snapshot_res {
        error!(chat_id = chat_id.0, error = %e, "Failed to create SQLite backup");
        let _ = bot
            .send_message(chat_id, format!("❌ Не вдалося створити бекап БД: {}", e))
            .await;
        let _ = std::fs::remove_file(&backup_path);
        return;
    }

    let size_mb = std::fs::metadata(&backup_path)
        .map(|m| m.len() as f64 / (1024.0 * 1024.0))
        .unwrap_or(0.0);

    let doc = InputFile::file(&backup_path).file_name(filename.clone());
    match bot
        .send_document(chat_id, doc)
        .caption(format!("💾 Бекап БД ({:.2} MB)", size_mb))
        .await
    {
        Ok(_) => info!(chat_id = chat_id.0, size_mb, "Sent SQLite backup to admin"),
        Err(e) => {
            error!(chat_id = chat_id.0, error = %e, "Failed to send SQLite backup document");
            let _ = bot
                .send_message(
                    chat_id,
                    format!("❌ Бекап створено ({:.2} MB), але не вдалося надіслати в Telegram: {}", size_mb, e),
                )
                .await;
        }
    }

    let _ = std::fs::remove_file(&backup_path);
}

pub async fn cmd_backup(
    bot: Bot,
    msg: Message,
    state: Arc<AppState>,
    app_config: &'static AppConfig,
) -> ResponseResult<()> {
    let user_id = msg.from.as_ref().map(|u| u.id.0 as i64).unwrap_or(msg.chat.id.0);
    if !app_config.is_admin(user_id) {
        bot.send_message(msg.chat.id, "⛔ Доступ лише для адміністратора.")
            .await?;
        return Ok(());
    }

    bot.send_message(msg.chat.id, "💾 Створюю знімок БД...")
        .await?;
    send_db_backup(&bot, msg.chat.id, state).await;
    Ok(())
}

pub async fn cmd_restore(
    bot: Bot,
    msg: Message,
    app_config: &'static AppConfig,
) -> ResponseResult<()> {
    let user_id = msg.from.as_ref().map(|u| u.id.0 as i64).unwrap_or(msg.chat.id.0);
    if !app_config.is_admin(user_id) {
        bot.send_message(msg.chat.id, "⛔ Доступ лише для адміністратора.")
            .await?;
        return Ok(());
    }

    let doc = msg
        .document()
        .or_else(|| msg.reply_to_message().and_then(|r| r.document()));

    let Some(doc) = doc else {
        bot.send_message(
            msg.chat.id,
            "📥 Надішли `.db` файл із підписом `/restore` або відповідай `/restore` на повідомлення з `.db` файлом.",
        )
        .await?;
        return Ok(());
    };

    bot.send_message(msg.chat.id, "⏳ Завантажую та перевіряю БД...")
        .await?;

    let bytes = match download_telegram_file(&bot, &doc.file.id.0, 3).await {
        Ok(b) => b,
        Err(e) => {
            bot.send_message(msg.chat.id, format!("❌ Не вдалося завантажити файл: {}", e))
                .await?;
            return Ok(());
        }
    };

    if !bytes.starts_with(b"SQLite format 3\0") {
        bot.send_message(msg.chat.id, "❌ Файл не є валідною SQLite-базою.")
            .await?;
        return Ok(());
    }

    let tmp_path = format!("{}.incoming.tmp", app_config.db_path);
    let incoming_path = format!("{}.incoming", app_config.db_path);

    if let Err(e) = std::fs::write(&tmp_path, &bytes) {
        bot.send_message(msg.chat.id, format!("❌ Не вдалося записати файл: {}", e))
            .await?;
        return Ok(());
    }

    let meme_count: Result<i64, String> = (|| {
        let conn = rusqlite::Connection::open_with_flags(
            &tmp_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(|e| e.to_string())?;
        conn.query_row("SELECT COUNT(*) FROM memes", [], |row| row.get(0))
            .map_err(|e| e.to_string())
    })();

    let count = match meme_count {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp_path);
            bot.send_message(
                msg.chat.id,
                format!("❌ Перевірка БД не пройшла (таблиця memes): {}", e),
            )
            .await?;
            return Ok(());
        }
    };

    if let Err(e) = std::fs::rename(&tmp_path, &incoming_path) {
        let _ = std::fs::remove_file(&tmp_path);
        bot.send_message(msg.chat.id, format!("❌ Не вдалося підготувати БД: {}", e))
            .await?;
        return Ok(());
    }

    let size_mb = bytes.len() as f64 / (1024.0 * 1024.0);
    info!(
        admin_id = user_id,
        meme_count = count,
        size_mb,
        "Database restore staged; restarting process to apply"
    );

    let _ = bot
        .send_message(
            msg.chat.id,
            format!(
                "✅ БД перевірено ({} мемів, {:.2} MB). Перезапускаю бота для застосування...",
                count, size_mb
            ),
        )
        .await;

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    std::process::exit(1);
}

async fn send_or_edit_admin_media(
    bot: &Bot,
    msg: &Message,
    meme: &Meme,
    caption: String,
    kb: InlineKeyboardMarkup,
) {
    if msg.photo().is_some() || msg.animation().is_some() {
        if let Err(e) = bot
            .edit_message_media(msg.chat.id, msg.id, build_input_media(meme, caption))
            .reply_markup(kb)
            .await
            && !e.to_string().contains("message is not modified")
        {
            warn!(chat_id = msg.chat.id.0, meme_id = meme.id, error = %e, "Failed to edit admin media message");
        }
    } else if let Err(e) = send_meme_with_markup(bot, msg.chat.id, meme, caption, kb).await {
        warn!(chat_id = msg.chat.id.0, meme_id = meme.id, error = %e, "Failed to send admin media card");
    }
}

pub async fn handle_admin_callback(
    bot: Bot,
    callback_query: CallbackQuery,
    state: Arc<AppState>,
    app_config: &'static AppConfig,
    data: &str,
) -> ResponseResult<()> {
    let admin_id = callback_query.from.id.0 as i64;
    let callback_id = callback_query.id.clone();

    if !app_config.is_admin(admin_id) {
        let _ = bot
            .answer_callback_query(callback_id)
            .text("⛔ Доступ лише для адміністратора.")
            .await;
        return Ok(());
    }

    if data == "admin:noop" {
        let _ = bot.answer_callback_query(callback_id).await;
        return Ok(());
    }

    let parts: Vec<&str> = data.split(':').collect();
    match parts.as_slice() {
        ["admin", "backup"] => {
            let _ = bot
                .answer_callback_query(callback_id)
                .text("💾 Створюю бекап БД...")
                .await;
            if let Some(msg) = callback_query.regular_message() {
                send_db_backup(&bot, msg.chat.id, state).await;
            }
        }
        ["admin", "home"] => {
            let summary = {
                let store = state.meme_store.lock().await;
                store.get_admin_stats()
            };
            let quota = state.vlm_router.quota_snapshot().await;
            let (text, kb) = format_admin_dashboard(&summary, &quota);
            let _ = bot.answer_callback_query(callback_id).await;
            if let Some(msg) = callback_query.regular_message()
                && let Err(e) = bot
                    .edit_message_text(msg.chat.id, msg.id, text)
                    .reply_markup(kb)
                    .await
                && !e.to_string().contains("message is not modified")
            {
                warn!(admin_id, error = %e, "Failed to refresh admin dashboard");
            }
        }
        ["admin", "mod", action, id_str] if matches!(*action, "pub" | "priv" | "rej") => {
            let Some(queue_id) = id_str.parse::<i64>().ok() else {
                let _ = bot.answer_callback_query(callback_id).await;
                return Ok(());
            };

            let target_status = match *action {
                "pub" => "approved_public",
                "priv" => "approved_private",
                _ => "rejected",
            };

            enum ClaimResult {
                Claimed(Box<ModerationQueueItem>),
                AlreadyHandled(String),
                Missing,
            }

            let claim = {
                let store = state.meme_store.lock().await;
                match store.get_moderation_item(queue_id) {
                    None => ClaimResult::Missing,
                    Some(item) if item.status != "pending" => {
                        ClaimResult::AlreadyHandled(item.status)
                    }
                    Some(item) => match store.update_moderation_status(queue_id, target_status) {
                        Ok(true) => ClaimResult::Claimed(Box::new(item)),
                        Ok(false) => ClaimResult::AlreadyHandled("processed".to_string()),
                        Err(e) => {
                            error!(admin_id, queue_id, error = %e, "Failed to claim moderation item");
                            ClaimResult::Missing
                        }
                    },
                }
            };

            let item = match claim {
                ClaimResult::Claimed(item) => *item,
                ClaimResult::AlreadyHandled(st) => {
                    let _ = bot
                        .answer_callback_query(callback_id)
                        .text(format!("ℹ️ Вже опрацьовано ({})", st))
                        .await;
                    if let Some(msg) = callback_query.regular_message() {
                        let _ = bot
                            .edit_message_reply_markup(msg.chat.id, msg.id)
                            .reply_markup(InlineKeyboardMarkup::default())
                            .await;
                    }
                    return Ok(());
                }
                ClaimResult::Missing => {
                    let _ = bot
                        .answer_callback_query(callback_id)
                        .text("ℹ️ Запис не знайдено")
                        .await;
                    return Ok(());
                }
            };

            if *action == "rej" {
                let _ = bot
                    .answer_callback_query(callback_id)
                    .text("⛔ Відхилено")
                    .await;
                if let Some(msg) = callback_query.regular_message() {
                    let _ = bot
                        .edit_message_caption(msg.chat.id, msg.id)
                        .caption(format!(
                            "⛔ Відхилено (#{}) адміном {}",
                            queue_id, admin_id
                        ))
                        .reply_markup(InlineKeyboardMarkup::default())
                        .await;
                }
                notify_uploader(
                    &bot,
                    &item,
                    "⛔ Модератор відхилив мем як неприйнятний контент.",
                )
                .await;
                info!(admin_id, queue_id, uploader_id = item.user_id, "Moderation item rejected");
                return Ok(());
            }

            let make_public = *action == "pub";
            let _ = bot
                .answer_callback_query(callback_id)
                .text("⏳ Індексую схвалений мем...")
                .await;

            let mime_type = match item.media_type {
                MediaType::Photo => "image/jpeg",
                MediaType::Animation => "video/mp4",
            };

            let analysis = match download_telegram_file(&bot, &item.file_tg_id, 3).await {
                Ok(bytes) => {
                    let vlm_res = state
                        .vlm_router
                        .describe_meme_with_policy(&bytes, &app_config.gemini_api_key, mime_type, true)
                        .await;
                    crate::persist_dirty_vlm_quotas(&state).await;
                    match vlm_res {
                        Ok(mut a) => {
                            a.is_nsfw = false;
                            a
                        }
                        Err(e) => {
                            warn!(
                                admin_id,
                                queue_id,
                                error = ?e,
                                "VLM failed on admin-approved meme; using caption fallback"
                            );
                            MemeAnalysis {
                                is_nsfw: false,
                                ocr_text: item.user_caption.clone(),
                                uk_description: if item.user_caption.trim().is_empty() {
                                    "Схвалений модератором мем".to_string()
                                } else {
                                    item.user_caption.clone()
                                },
                                en_description: if item.user_caption.trim().is_empty() {
                                    "Moderator approved meme".to_string()
                                } else {
                                    item.user_caption.clone()
                                },
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!(admin_id, queue_id, error = %e, "Failed to download approved meme; using caption fallback");
                    MemeAnalysis {
                        is_nsfw: false,
                        ocr_text: item.user_caption.clone(),
                        uk_description: "Схвалений модератором мем".to_string(),
                        en_description: "Moderator approved meme".to_string(),
                    }
                }
            };

            let meme_id = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            let save_res = {
                let mut engine = state.engine.lock().await;
                let mut store = state.meme_store.lock().await;
                store.add_from_analysis(
                    NewMeme {
                        id: meme_id,
                        title: item.user_caption.clone(),
                        analysis: &analysis,
                        file_tg_id: item.file_tg_id.clone(),
                        file_tg_unique_id: item.file_tg_unique_id.clone(),
                        user_id: item.user_id,
                        source_title: item.source_title.clone(),
                        source_url: item.source_url.clone(),
                        phash: item.phash,
                        media_type: item.media_type,
                        thumbnail_tg_id: item.thumbnail_tg_id.clone(),
                        vlm_version: constants::CURRENT_VLM_VERSION,
                        is_public_override: Some(make_public),
                    },
                    &mut engine,
                )
            };

            match save_res {
                Ok((saved_id, _)) => {
                    let taxonomy = {
                        let store = state.meme_store.lock().await;
                        store.get_active_taxonomy()
                    };
                    if let Ok(tag_res) = tagger::classify_meme_tags(
                        &app_config.gemini_api_key,
                        &analysis.ocr_text,
                        &analysis.uk_description,
                        &analysis.en_description,
                        &item.user_caption,
                        &taxonomy,
                    )
                    .await
                    {
                        let ver = if tag_res.is_undetermined {
                            constants::TAGS_STATE_UNDETERMINED
                        } else {
                            constants::CURRENT_TAGS_VERSION
                        };
                        let mut store = state.meme_store.lock().await;
                        let _ = store.set_meme_tags(saved_id, &tag_res.tags, ver);
                    }

                    let vis_label = if make_public {
                        "🌐 Публічний"
                    } else {
                        "🔒 Приватний"
                    };
                    if let Some(msg) = callback_query.regular_message() {
                        let _ = bot
                            .edit_message_caption(msg.chat.id, msg.id)
                            .caption(format!(
                                "✅ Схвалено (#{}, meme_id={}) · {}",
                                queue_id, saved_id, vis_label
                            ))
                            .reply_markup(InlineKeyboardMarkup::default())
                            .await;
                    }
                    let uploader_msg = if make_public {
                        "✅ Модератор схвалив мем — тепер у загальному каталозі!"
                    } else {
                        "✅ Модератор схвалив мем у твою приватну колекцію!"
                    };
                    notify_uploader(&bot, &item, uploader_msg).await;
                    info!(
                        admin_id,
                        queue_id,
                        meme_id = saved_id,
                        make_public,
                        "Moderation item approved and indexed"
                    );
                }
                Err(e) => {
                    error!(admin_id, queue_id, error = %e, "Failed to save approved moderation meme");
                    if let Some(msg) = callback_query.regular_message() {
                        let _ = bot
                            .edit_message_caption(msg.chat.id, msg.id)
                            .caption(format!("⚠️ Помилка збереження схваленого мема: {}", e))
                            .reply_markup(InlineKeyboardMarkup::default())
                            .await;
                    }
                }
            }
        }
        ["admin", "queue", idx_str] => {
            let target_idx = idx_str.parse::<usize>().unwrap_or(0);
            let pending = {
                let store = state.meme_store.lock().await;
                store.get_pending_moderation_items()
            };

            if pending.is_empty() {
                let _ = bot
                    .answer_callback_query(callback_id)
                    .text("✅ Черга модерації порожня!")
                    .await;
                if let Some(msg) = callback_query.regular_message()
                    && (msg.photo().is_some() || msg.animation().is_some())
                {
                    let _ = bot
                        .edit_message_caption(msg.chat.id, msg.id)
                        .caption("✅ Черга модерації порожня.")
                        .reply_markup(InlineKeyboardMarkup::default())
                        .await;
                }
                return Ok(());
            }

            let _ = bot.answer_callback_query(callback_id).await;
            let idx = target_idx.min(pending.len() - 1);
            let item = &pending[idx];
            let stub = meme_from_moderation_item(item, item.id.max(0) as u64, false);
            let cap = format_moderation_caption(item, Some((idx, pending.len())));
            let kb = moderation_keyboard(item.id, Some((idx, pending.len())));

            if let Some(msg) = callback_query.regular_message() {
                send_or_edit_admin_media(&bot, msg, &stub, cap, kb).await;
            }
        }
        ["admin", "recent", idx_str] => {
            let target_idx = idx_str.parse::<usize>().unwrap_or(0);
            let recent = {
                let store = state.meme_store.lock().await;
                store.get_admin_recent_memes(ADMIN_RECENT_LIMIT)
            };

            if recent.is_empty() {
                let _ = bot
                    .answer_callback_query(callback_id)
                    .text("📭 Каталог порожній.")
                    .await;
                return Ok(());
            }

            let _ = bot.answer_callback_query(callback_id).await;
            let idx = target_idx.min(recent.len() - 1);
            let meme = &recent[idx];
            let cap = format_admin_recent_caption(meme, idx, recent.len());
            let kb = admin_recent_keyboard(meme, idx, recent.len(), false);
            if let Some(msg) = callback_query.regular_message() {
                send_or_edit_admin_media(&bot, msg, meme, cap, kb).await;
            }
        }
        ["admin", "recent_vis", id_str, mode, idx_str] if matches!(*mode, "pub" | "priv") => {
            let Some(meme_id) = id_str.parse::<u64>().ok() else {
                let _ = bot.answer_callback_query(callback_id).await;
                return Ok(());
            };
            let idx_hint = idx_str.parse::<usize>().unwrap_or(0);
            let make_public = *mode == "pub";

            let (changed, recent) = {
                let mut store = state.meme_store.lock().await;
                let changed = store.admin_toggle_meme_public(meme_id, make_public);
                let recent = store.get_admin_recent_memes(ADMIN_RECENT_LIMIT);
                (changed, recent)
            };

            let toast = match changed {
                Ok(true) if make_public => "🌐 Зроблено публічним",
                Ok(true) => "🔒 Зроблено приватним",
                Ok(false) => "ℹ️ Мем не знайдено",
                Err(ref e) => {
                    error!(admin_id, meme_id, error = %e, "Admin visibility toggle failed");
                    "⚠️ Помилка оновлення"
                }
            };
            let _ = bot.answer_callback_query(callback_id).text(toast).await;

            if let Some(msg) = callback_query.regular_message()
                && !recent.is_empty()
            {
                let pos = recent
                    .iter()
                    .position(|m| m.id == meme_id)
                    .unwrap_or_else(|| idx_hint.min(recent.len() - 1));
                let meme = &recent[pos];
                let _ = bot
                    .edit_message_caption(msg.chat.id, msg.id)
                    .caption(format_admin_recent_caption(meme, pos, recent.len()))
                    .parse_mode(ParseMode::Html)
                    .reply_markup(admin_recent_keyboard(meme, pos, recent.len(), false))
                    .await;
            }
        }
        ["admin", "purge_ask", id_str, idx_str] | ["admin", "purge_cancel", id_str, idx_str] => {
            let Some(meme_id) = id_str.parse::<u64>().ok() else {
                let _ = bot.answer_callback_query(callback_id).await;
                return Ok(());
            };
            let idx_hint = idx_str.parse::<usize>().unwrap_or(0);
            let confirm = parts[1] == "purge_ask";
            let recent = {
                let store = state.meme_store.lock().await;
                store.get_admin_recent_memes(ADMIN_RECENT_LIMIT)
            };
            let _ = bot.answer_callback_query(callback_id).await;

            if let Some(msg) = callback_query.regular_message()
                && let Some(pos) = recent.iter().position(|m| m.id == meme_id)
            {
                let _ = bot
                    .edit_message_reply_markup(msg.chat.id, msg.id)
                    .reply_markup(admin_recent_keyboard(
                        &recent[pos],
                        pos.min(idx_hint.max(pos)),
                        recent.len(),
                        confirm,
                    ))
                    .await;
            }
        }
        ["admin", "purge_confirm", id_str, idx_str] => {
            let Some(meme_id) = id_str.parse::<u64>().ok() else {
                let _ = bot.answer_callback_query(callback_id).await;
                return Ok(());
            };
            let idx_hint = idx_str.parse::<usize>().unwrap_or(0);

            let (deleted, recent) = {
                let mut store = state.meme_store.lock().await;
                let deleted = store.admin_delete_meme(meme_id);
                let recent = store.get_admin_recent_memes(ADMIN_RECENT_LIMIT);
                (deleted, recent)
            };

            let toast = match deleted {
                Ok(true) => "🗑 Видалено назавжди",
                Ok(false) => "ℹ️ Мем вже видалено",
                Err(ref e) => {
                    error!(admin_id, meme_id, error = %e, "Admin delete meme failed");
                    "⚠️ Помилка видалення"
                }
            };
            let _ = bot.answer_callback_query(callback_id).text(toast).await;

            if let Some(msg) = callback_query.regular_message() {
                if recent.is_empty() {
                    let _ = bot
                        .edit_message_caption(msg.chat.id, msg.id)
                        .caption("📭 Каталог порожній.")
                        .reply_markup(InlineKeyboardMarkup::default())
                        .await;
                } else {
                    let pos = idx_hint.min(recent.len() - 1);
                    let meme = &recent[pos];
                    let _ = bot
                        .edit_message_media(
                            msg.chat.id,
                            msg.id,
                            build_input_media(
                                meme,
                                format_admin_recent_caption(meme, pos, recent.len()),
                            ),
                        )
                        .reply_markup(admin_recent_keyboard(meme, pos, recent.len(), false))
                        .await;
                }
            }
        }
        ["admin", "tags", page_str] => {
            let page = page_str.parse::<usize>().unwrap_or(0);
            let tags = {
                let store = state.meme_store.lock().await;
                store.get_admin_tags()
            };
            let _ = bot.answer_callback_query(callback_id).await;
            let (text, kb) = format_admin_tags_panel(&tags, page);
            if let Some(msg) = callback_query.regular_message()
                && let Err(e) = bot
                    .edit_message_text(msg.chat.id, msg.id, text)
                    .reply_markup(kb)
                    .await
                && !e.to_string().contains("message is not modified")
            {
                warn!(admin_id, error = %e, "Failed to show admin tags panel");
            }
        }
        ["admin", "tag_toggle", id_str, flag_str, page_str] => {
            let Some(tag_id) = id_str.parse::<i64>().ok() else {
                let _ = bot.answer_callback_query(callback_id).await;
                return Ok(());
            };
            let make_active = *flag_str == "1";
            let page = page_str.parse::<usize>().unwrap_or(0);

            let (res, tags) = {
                let store = state.meme_store.lock().await;
                let res = store.set_tag_active(tag_id, make_active);
                let tags = store.get_admin_tags();
                (res, tags)
            };

            let toast = match res {
                Ok(true) if make_active => "✅ Категорію увімкнено",
                Ok(true) => "⏸ Категорію вимкнено",
                Ok(false) => "ℹ️ Категорію не знайдено",
                Err(ref e) => {
                    error!(admin_id, tag_id, error = %e, "Failed to toggle tag active state");
                    "⚠️ Помилка оновлення категорії"
                }
            };
            let _ = bot.answer_callback_query(callback_id).text(toast).await;

            let (text, kb) = format_admin_tags_panel(&tags, page);
            if let Some(msg) = callback_query.regular_message()
                && let Err(e) = bot
                    .edit_message_text(msg.chat.id, msg.id, text)
                    .reply_markup(kb)
                    .await
                && !e.to_string().contains("message is not modified")
            {
                warn!(admin_id, error = %e, "Failed to update admin tags panel");
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
    use crate::vlm::ModelQuotaSnapshot;
    use teloxide::types::InlineKeyboardButtonKind;

    #[test]
    fn format_admin_dashboard_shows_counts_and_buttons() {
        let summary = AdminStatsSummary {
            total_memes: 1558,
            public_memes: 1550,
            private_memes: 8,
            total_users: 4,
            pending_moderation: 2,
            undetermined_memes: 8,
            blocked_tag_memes: 0,
            active_tags: 25,
            total_tags: 25,
        };
        let quota = VlmQuotaSummary {
            pt_day_key: "2026-11-24".to_string(),
            tier1_used: 12,
            tier1_limit: 1000,
            tier2_used: 1,
            tier2_limit: 80,
            models: vec![ModelQuotaSnapshot {
                id: "gemini-3.5-flash-lite",
                tier: ModelTier::Tier1,
                used_today: 7,
                rpd_limit: 500,
                rpm_used: 0,
                rpm_limit: 15,
                cooldown_remaining_secs: 0,
                last_reason: String::new(),
            }],
        };

        let (text, kb) = format_admin_dashboard(&summary, &quota);
        assert!(text.contains("👑 Панель адміністратора"));
        assert!(text.contains("Черга модерації: 2"));
        assert!(text.contains("T1 12/1000"));
        assert_eq!(kb.inline_keyboard.len(), 3);
        assert_eq!(
            kb.inline_keyboard[0][0].kind,
            InlineKeyboardButtonKind::CallbackData("admin:queue:0".to_string())
        );
    }

    #[test]
    fn moderation_caption_and_keyboard_escape_html_and_include_actions() {
        let item = ModerationQueueItem {
            id: 42,
            user_id: 123,
            chat_id: 123,
            username: "bad<user>".to_string(),
            message_id: 7,
            file_tg_id: "tg1".to_string(),
            file_tg_unique_id: "u1".to_string(),
            media_type: MediaType::Photo,
            thumbnail_tg_id: String::new(),
            user_caption: "caption & <script>".to_string(),
            source_title: String::new(),
            source_url: String::new(),
            reason: "Explicit NSFW > 0.85".to_string(),
            safety_score: 0.12,
            phash: 99,
            status: "pending".to_string(),
            created_at: 1000,
        };

        let cap = format_moderation_caption(&item, Some((0, 3)));
        assert!(cap.contains("Черга (1/3) · #42"));
        assert!(cap.contains("@bad&lt;user&gt;"));
        assert!(cap.contains("caption &amp; &lt;script&gt;"));
        assert!(cap.contains("12%"));

        let kb = moderation_keyboard(42, Some((0, 3)));
        assert_eq!(kb.inline_keyboard.len(), 2);
        assert_eq!(
            kb.inline_keyboard[1][0].kind,
            InlineKeyboardButtonKind::CallbackData("admin:mod:pub:42".to_string())
        );
        assert_eq!(
            kb.inline_keyboard[1][2].kind,
            InlineKeyboardButtonKind::CallbackData("admin:mod:rej:42".to_string())
        );
    }

    #[test]
    fn format_admin_tags_panel_renders_toggle_buttons_and_back() {
        let tags = vec![
            AdminTagInfo {
                id: 1,
                name: "робота".to_string(),
                emoji: "💼".to_string(),
                is_active: true,
                meme_count: 457,
            },
            AdminTagInfo {
                id: 2,
                name: "абсурд".to_string(),
                emoji: "🤡".to_string(),
                is_active: false,
                meme_count: 12,
            },
        ];

        let (text, kb) = format_admin_tags_panel(&tags, 0);
        assert!(text.contains("1/40 активних"));
        assert_eq!(kb.inline_keyboard.len(), 2);
        assert_eq!(
            kb.inline_keyboard[0][0].kind,
            InlineKeyboardButtonKind::CallbackData("admin:tag_toggle:1:0:0".to_string())
        );
        assert_eq!(
            kb.inline_keyboard[0][1].kind,
            InlineKeyboardButtonKind::CallbackData("admin:tag_toggle:2:1:0".to_string())
        );
        assert_eq!(
            kb.inline_keyboard[1][0].kind,
            InlineKeyboardButtonKind::CallbackData("admin:home".to_string())
        );
    }
}
