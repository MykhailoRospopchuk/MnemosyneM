use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use teloxide::Bot;
use teloxide::RequestError;
use teloxide::prelude::*;
use teloxide::types::{
    ChatId, InlineKeyboardButton, InlineKeyboardMarkup, KeyboardButton, KeyboardMarkup, Message,
    MessageId,
};
use teloxide::utils::command::BotCommands;
use tokio::sync::mpsc::Sender;
use tracing::{error, warn};

use crate::AppState;
use crate::config::AppConfig;
use crate::database::entity::meme::Meme;
use crate::database::entity::user_setting::UserSetting;
use crate::database::enums::command_enum::Command;
use crate::meme_store::UserReactionCounts;
use crate::worker::MemeJob;

pub const BTN_TINDER: &str = "🔥 Тіндер";
pub const BTN_LIKED: &str = "❤️ Лайки";
pub const BTN_TAGS: &str = "🏷️ Теги";
pub const BTN_DRIP: &str = "🎁 Підписка";
pub const BTN_RANDOM: &str = "🎲 Рандом";
pub const BTN_STATS: &str = "📊 Стата";
pub const BTN_SETTINGS: &str = "⚙️ Опції";
pub const BTN_HELP: &str = "❓ Інфо";

pub fn match_menu_button(trimmed: &str) -> Option<Command> {
    match trimmed {
        BTN_TINDER | "🔥 Мем-Тіндер" => Some(Command::Tinder),
        BTN_RANDOM | "🎲 Випадковий мем" => Some(Command::Random),
        BTN_LIKED | "❤️ Вподобані" => Some(Command::Liked),
        BTN_TAGS => Some(Command::Tags),
        BTN_DRIP => Some(Command::Drip),
        BTN_STATS | "📊 Статистика" => Some(Command::Stats),
        BTN_SETTINGS | "⚙️ Налаштування" => Some(Command::Settings),
        BTN_HELP | "❓ Допомога" => Some(Command::Help),
        _ => None,
    }
}

pub const RESET_CODE_TTL: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingResetTarget {
    Likes,
    All,
}

#[derive(Debug, Clone)]
pub struct PendingReactionReset {
    pub code: String,
    pub target: PendingResetTarget,
    pub settings_message: Option<(ChatId, MessageId)>,
    pub expires_at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodeInputDecision {
    Expired,
    Matched,
    WrongNumber,
    NonNumericCancel,
}

enum PendingAction {
    Keep,
    Clear,
    Set(PendingReactionReset),
}

pub fn main_keyboard() -> KeyboardMarkup {
    KeyboardMarkup::new(vec![
        vec![
            KeyboardButton::new(BTN_TINDER),
            KeyboardButton::new(BTN_RANDOM),
            KeyboardButton::new(BTN_LIKED),
            KeyboardButton::new(BTN_TAGS),
        ],
        vec![
            KeyboardButton::new(BTN_DRIP),
            KeyboardButton::new(BTN_STATS),
            KeyboardButton::new(BTN_SETTINGS),
            KeyboardButton::new(BTN_HELP),
        ],
    ])
    .resize_keyboard()
    .one_time_keyboard()
}

pub async fn tags(bot: Bot, msg: Message, state: Arc<AppState>) -> Result<Message, RequestError> {
    let user_id = msg.chat.id.0;
    let (tag_counts, tagged, total) = {
        let store = state.meme_store.lock().await;
        let counts = store.get_tag_counts(user_id);
        let (tagged, total) = store.get_tagging_progress(crate::constants::CURRENT_TAGS_VERSION);
        (counts, tagged, total)
    };

    if tag_counts.is_empty() {
        return bot
            .send_message(
                msg.chat.id,
                format!(
                    "🏷️ Теги ще формуються у фоні ({}/{}).\nЗазирни сюди через кілька хвилин!",
                    tagged, total
                ),
            )
            .reply_markup(main_keyboard())
            .await;
    }

    let mut text = String::from(
        "🏷️ Категорії мемів\n\nНатисни на тег, щоб відкрити меми. Можна комбінувати кілька тегів і текст (наприклад: #мавпи #айті або #робота дедлайн).",
    );
    if tagged < total {
        text.push_str(&format!("\n\n⏳ Проіндексовано тегів: {}/{}", tagged, total));
    }

    let rows: Vec<Vec<InlineKeyboardButton>> = tag_counts
        .chunks(2)
        .map(|pair| {
            pair.iter()
                .map(|(name, emoji, count)| {
                    InlineKeyboardButton::switch_inline_query_current_chat(
                        format!("{} #{} ({})", emoji, name, count),
                        format!("#{} ", name),
                    )
                })
                .collect()
        })
        .collect();

    bot.send_message(msg.chat.id, text)
        .reply_markup(InlineKeyboardMarkup::new(rows))
        .await
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
    let ((public, private), liked) = {
        let meme_store = state.meme_store.lock().await;
        (
            meme_store.len(msg.chat.id.0),
            meme_store.count_user_likes(msg.chat.id.0),
        )
    };

    let quota = state.vlm_router.quota_snapshot().await;
    let search_snap = state.search_metrics.snapshot();

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

    let tier1_detail: Vec<String> = quota
        .models
        .iter()
        .filter(|m| m.tier == crate::vlm::ModelTier::Tier1)
        .map(|m| {
            let short = m
                .id
                .strip_prefix("gemini-")
                .unwrap_or(m.id)
                .replace("-flash-lite", "-lite");
            if m.cooldown_remaining_secs > 90 {
                format!("{}: {}/{} (paused)", short, m.used_today, m.rpd_limit)
            } else if m.cooldown_remaining_secs > 0 {
                format!(
                    "{}: {}/{} ({}s cd)",
                    short, m.used_today, m.rpd_limit, m.cooldown_remaining_secs
                )
            } else {
                format!("{}: {}/{}", short, m.used_today, m.rpd_limit)
            }
        })
        .collect();

    let search_line = if search_snap.count > 0 {
        format!(
            "{}ms (embed {}ms · stage1 {}ms · rerank {}ms · {} reqs)",
            search_snap.avg_total_ms,
            search_snap.avg_embed_ms,
            search_snap.avg_stage1_ms,
            search_snap.avg_rerank_ms,
            search_snap.count
        )
    } else {
        "N/A (no searches yet)".to_string()
    };

    let text = format!(
        "📊 MnemosyneM Stats\n\n\
        🗂 Public memes: {}\n\
        🔒 Your private memes: {}\n\
        ❤️ Liked memes: {}\n\
        ⏱ Uptime: {}\n\
        🧠 RAM (RSS): {}\n\
        💾 SQLite DB: {}\n\
        📥 Upload queue: {}/{}\n\
        🎯 Stage-2 Reranker: {}\n\
        ⚡ Gemini VLM ({} PT): T1 {}/{} ({}) | T2 {}/{}\n\
        🔍 Avg Search: {}",
        public,
        private,
        liked,
        uptime,
        rss_label,
        db_size_label,
        queued,
        max_queue,
        reranker_label,
        quota.pt_day_key,
        quota.tier1_used,
        quota.tier1_limit,
        tier1_detail.join(", "),
        quota.tier2_used,
        quota.tier2_limit,
        search_line
    );

    let message = bot
        .send_message(msg.chat.id, text)
        .reply_markup(main_keyboard())
        .await?;

    Ok(message)
}

fn random_caption(meme: &Meme) -> String {
    format!(
        "🎲 Випадковий мем{}",
        crate::bot::tinder::format_source_html(meme)
    )
}

fn random_keyboard(meme_id: u64, liked: bool) -> InlineKeyboardMarkup {
    let like_btn = if liked {
        InlineKeyboardButton::callback("💖 Вподобано", format!("random:unlike:{}", meme_id))
    } else {
        InlineKeyboardButton::callback("❤️ Лайк", format!("random:like:{}", meme_id))
    };

    InlineKeyboardMarkup::new(vec![vec![
        like_btn,
        InlineKeyboardButton::callback("👎", format!("random:dislike:{}", meme_id)),
        InlineKeyboardButton::callback("🎲 Ще", format!("random:next:{}", meme_id)),
    ]])
}

pub async fn random(bot: Bot, msg: Message, state: Arc<AppState>) -> Result<Message, RequestError> {
    let user_id = msg.chat.id.0;
    let (meme_opt, liked) = {
        let meme_store = state.meme_store.lock().await;
        let meme = meme_store.random_meme(user_id).cloned();
        let liked = meme
            .as_ref()
            .is_some_and(|m| meme_store.is_meme_liked(user_id, m.id));
        (meme, liked)
    };

    if let Some(meme) = meme_opt {
        crate::bot::tinder::send_meme_with_markup(
            &bot,
            msg.chat.id,
            &meme,
            random_caption(&meme),
            random_keyboard(meme.id, liked),
        )
        .await
    } else {
        bot.send_message(
            msg.chat.id,
            "🎲 Немає доступних мемів (база порожня або всі меми в поточному режимі дизлайкнуті — перевір /settings).",
        )
        .await
    }
}

pub async fn handle_random_callback(
    bot: Bot,
    callback_query: CallbackQuery,
    state: Arc<AppState>,
    data: &str,
) -> ResponseResult<()> {
    let user_id = callback_query.from.id.0 as i64;
    let callback_id = callback_query.id.clone();

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

    match action {
        "like" | "unlike" => {
            let set_liked = action == "like";
            {
                let store = state.meme_store.lock().await;
                let res = if set_liked {
                    store.record_reaction(user_id, meme_id, "like")
                } else {
                    store.remove_reaction(user_id, meme_id)
                };
                if let Err(e) = res {
                    error!(user_id, meme_id, action, error = %e, "Failed to update random meme like reaction");
                }
            }

            if let Err(e) = bot.answer_callback_query(callback_id).await {
                warn!(user_id, meme_id, error = %e, "Failed to answer random like callback");
            }

            if let Some(msg) = callback_query.regular_message()
                && let Err(e) = bot
                    .edit_message_reply_markup(msg.chat.id, msg.id)
                    .reply_markup(random_keyboard(meme_id, set_liked))
                    .await
                && !e.to_string().contains("message is not modified")
            {
                warn!(user_id, meme_id, error = %e, "Failed to refresh random meme markup");
            }
        }
        "dislike" | "next" => {
            let (next_meme, liked) = {
                let store = state.meme_store.lock().await;
                if action == "dislike"
                    && let Err(e) = store.record_reaction(user_id, meme_id, "dislike")
                {
                    error!(user_id, meme_id, error = %e, "Failed to record random dislike reaction");
                }
                let next = store
                    .random_meme_excluding(user_id, Some(meme_id))
                    .cloned();
                let liked = next
                    .as_ref()
                    .is_some_and(|m| store.is_meme_liked(user_id, m.id));
                (next, liked)
            };

            if let Err(e) = bot.answer_callback_query(callback_id).await {
                warn!(user_id, meme_id, error = %e, "Failed to answer random navigation callback");
            }

            if let Some(msg) = callback_query.regular_message() {
                match next_meme {
                    Some(meme) => {
                        if let Err(e) = bot
                            .edit_message_media(
                                msg.chat.id,
                                msg.id,
                                crate::bot::tinder::build_input_media(
                                    &meme,
                                    random_caption(&meme),
                                ),
                            )
                            .reply_markup(random_keyboard(meme.id, liked))
                            .await
                            && !e.to_string().contains("message is not modified")
                        {
                            warn!(user_id, meme_id = meme.id, error = %e, "Failed to edit random media");
                        }
                    }
                    None => {
                        if let Err(e) = bot
                            .edit_message_caption(msg.chat.id, msg.id)
                            .caption("🎲 Більше немає доступних мемів — усі в поточній вибірці дизлайкнуті. Можеш скинути дизлайки в /settings.")
                            .reply_markup(InlineKeyboardMarkup::default())
                            .await
                            && !e.to_string().contains("message is not modified")
                        {
                            warn!(user_id, error = %e, "Failed to edit exhausted random caption");
                        }
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
    let user_id = msg.chat.id.0;
    let (text, keyboard) = {
        let meme_store = state.meme_store.lock().await;
        let s = meme_store.get_user_settings(user_id);
        let counts = meme_store.get_user_reaction_counts(user_id);

        format_settings(&s, counts)
    };

    bot.send_message(msg.chat.id, text)
        .reply_markup(keyboard)
        .await
}

pub fn format_settings(
    settings: &UserSetting,
    counts: UserReactionCounts,
) -> (String, InlineKeyboardMarkup) {
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

    let text = format!(
        "⚙️ Налаштування\n\n\
         🔥 Реакції Мем-Тіндера:\n\
         ❤️ Лайки: {} | 👎 Дізлайки: {} | ⏭️ Скіпи: {}",
        counts.likes, counts.dislikes, counts.skips
    );

    let keyboard = InlineKeyboardMarkup::new(vec![
        vec![InlineKeyboardButton::callback(
            format!("Uploading: {}", upload_label),
            "toggle_upload",
        )],
        vec![InlineKeyboardButton::callback(
            format!("Search: {}", scope_label),
            "cycle_scope",
        )],
        vec![InlineKeyboardButton::callback(
            format!("⏭️ Скинути скіпи ({})", counts.skips),
            "settings:reset_ask:skip",
        )],
        vec![InlineKeyboardButton::callback(
            format!("👎 Скинути дізлайки ({})", counts.dislikes),
            "settings:reset_ask:dislike",
        )],
        vec![InlineKeyboardButton::callback(
            format!("💔 Скинути лайки ({})", counts.likes),
            "settings:reset_ask:like",
        )],
        vec![InlineKeyboardButton::callback(
            format!("🗑 Скинути всі реакції ({})", counts.total()),
            "settings:reset_ask:all",
        )],
    ]);

    (text, keyboard)
}

fn format_reset_confirmation(
    target: &str,
    counts: UserReactionCounts,
) -> Option<(String, InlineKeyboardMarkup)> {
    let text = match target {
        "skip" => format!(
            "⚠️ Скинути пропущені меми ({})?\n\nУсі скіпнуті меми знову з’являться у /tinder.",
            counts.skips
        ),
        "dislike" => format!(
            "⚠️ Скинути дізлайки ({})?\n\nУсі дізлайкнуті меми знову з’являться у /tinder.",
            counts.dislikes
        ),
        "like" => format!(
            "⚠️ Скинути лайки ({})?\n\nСписок /liked буде очищено, а ці меми знову з’являться у /tinder.\n(Меми, збережені у твою колекцію, залишаться.)",
            counts.likes
        ),
        "all" => format!(
            "⚠️ Скинути ВСІ реакції Тіндера ({})?\n\n❤️ Лайки: {} | 👎 Дізлайки: {} | ⏭️ Скіпи: {}\n\nСписок /liked буде очищено, і /tinder почнеться заново.",
            counts.total(),
            counts.likes,
            counts.dislikes,
            counts.skips
        ),
        _ => return None,
    };

    let keyboard = InlineKeyboardMarkup::new(vec![vec![
        InlineKeyboardButton::callback(
            "✅ Так, скинути",
            format!("settings:reset_confirm:{}", target),
        ),
        InlineKeyboardButton::callback("❌ Скасувати", "settings:back"),
    ]]);

    Some((text, keyboard))
}

fn generate_reset_code() -> String {
    let mut hasher = RandomState::new().build_hasher();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    hasher.write_u64(nanos);
    let code = 1000 + (hasher.finish() % 9000);
    format!("{:04}", code)
}

fn evaluate_reset_code_input(
    pending: &PendingReactionReset,
    input: &str,
    now: Instant,
) -> CodeInputDecision {
    if pending.expires_at <= now {
        return CodeInputDecision::Expired;
    }

    let trimmed = input.trim();
    if trimmed == pending.code {
        return CodeInputDecision::Matched;
    }

    if !trimmed.is_empty() && trimmed.chars().all(|c| c.is_ascii_digit()) {
        CodeInputDecision::WrongNumber
    } else {
        CodeInputDecision::NonNumericCancel
    }
}

fn format_code_reset_prompt(
    target: PendingResetTarget,
    counts: UserReactionCounts,
    code: &str,
) -> (String, InlineKeyboardMarkup) {
    let text = match target {
        PendingResetTarget::Likes => format!(
            "🔐 Підтвердження скидання лайків ({})\n\n\
             Список /liked буде повністю очищено, а ці меми знову з’являться у /tinder.\n\
             (Меми, збережені у твою колекцію, залишаться.)\n\n\
             Щоб підтвердити, надішли у чат число: {}\n\
             (діє 2 хв)",
            counts.likes, code
        ),
        PendingResetTarget::All => format!(
            "🔐 Підтвердження скидання ВСІХ реакцій ({})\n\n\
             ❤️ Лайки: {} | 👎 Дізлайки: {} | ⏭️ Скіпи: {}\n\
             Список /liked буде повністю очищено, і /tinder почнеться заново.\n\n\
             Щоб підтвердити, надішли у чат число: {}\n\
             (діє 2 хв)",
            counts.total(),
            counts.likes,
            counts.dislikes,
            counts.skips,
            code
        ),
    };

    let keyboard = InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::callback(
        "❌ Скасувати",
        "settings:back",
    )]]);

    (text, keyboard)
}

fn begin_code_reset(
    target: PendingResetTarget,
    counts: UserReactionCounts,
    settings_message: Option<(ChatId, MessageId)>,
) -> (PendingAction, (String, InlineKeyboardMarkup)) {
    let code = generate_reset_code();
    let prompt = format_code_reset_prompt(target, counts, &code);
    let pending = PendingReactionReset {
        code,
        target,
        settings_message,
        expires_at: Instant::now() + RESET_CODE_TTL,
    };
    (PendingAction::Set(pending), prompt)
}

pub async fn clear_pending_reset(state: &Arc<AppState>, user_id: i64) {
    state.pending_reaction_resets.lock().await.remove(&user_id);
}

/// Checks whether the user currently has a pending 4-digit reset confirmation.
/// Returns `Ok(true)` if the message was consumed as a confirmation attempt (matched or wrong number),
/// or `Ok(false)` if there was no active challenge or it expired / was cancelled by non-numeric text.
pub async fn try_handle_pending_reset(
    bot: &Bot,
    msg: &Message,
    text: &str,
    state: &Arc<AppState>,
) -> ResponseResult<bool> {
    let user_id = msg
        .from
        .as_ref()
        .map(|u| u.id.0 as i64)
        .unwrap_or(msg.chat.id.0);

    enum Outcome {
        NotHandled,
        WrongNumber,
        Matched(PendingReactionReset),
    }

    let outcome = {
        let mut pending_map = state.pending_reaction_resets.lock().await;
        match pending_map
            .get(&user_id)
            .map(|pending| evaluate_reset_code_input(pending, text, Instant::now()))
        {
            None => Outcome::NotHandled,
            Some(CodeInputDecision::Expired | CodeInputDecision::NonNumericCancel) => {
                pending_map.remove(&user_id);
                Outcome::NotHandled
            }
            Some(CodeInputDecision::WrongNumber) => Outcome::WrongNumber,
            Some(CodeInputDecision::Matched) => match pending_map.remove(&user_id) {
                Some(pending) => Outcome::Matched(pending),
                None => Outcome::NotHandled,
            },
        }
    };

    match outcome {
        Outcome::NotHandled => Ok(false),
        Outcome::WrongNumber => {
            bot.send_message(
                msg.chat.id,
                "❌ Невірне число. Спробуй ще раз або натисни «❌ Скасувати».",
            )
            .await?;
            Ok(true)
        }
        Outcome::Matched(pending) => {
            let (reply_text, refreshed) = {
                let meme_store = state.meme_store.lock().await;
                let filter = match pending.target {
                    PendingResetTarget::Likes => Some("like"),
                    PendingResetTarget::All => None,
                };
                let res = meme_store.reset_user_reactions(user_id, filter);
                let settings = meme_store.get_user_settings(user_id);
                let counts = meme_store.get_user_reaction_counts(user_id);
                let refreshed = format_settings(&settings, counts);

                let reply_text = match res {
                    Ok(deleted) => match pending.target {
                        PendingResetTarget::Likes => {
                            format!("✅ Скинуто лайків: {}. Список /liked очищено.", deleted)
                        }
                        PendingResetTarget::All => {
                            format!("✅ Скинуто всі реакції ({}). /tinder почнеться заново.", deleted)
                        }
                    },
                    Err(e) => {
                        error!(user_id, error = %e, "Failed to reset user reactions");
                        "❌ Помилка при скиданні реакцій.".to_string()
                    }
                };
                (reply_text, refreshed)
            };

            bot.send_message(msg.chat.id, reply_text)
                .reply_markup(main_keyboard())
                .await?;

            if let Some((settings_chat_id, settings_msg_id)) = pending.settings_message {
                let _ = bot
                    .edit_message_text(settings_chat_id, settings_msg_id, refreshed.0)
                    .reply_markup(refreshed.1)
                    .await;
            }

            Ok(true)
        }
    }
}

pub async fn handle_settings_callback(
    bot: Bot,
    callback_query: CallbackQuery,
    state: Arc<AppState>,
    data: &str,
) -> ResponseResult<()> {
    let user_id = callback_query.from.id.0 as i64;
    let callback_id = callback_query.id.clone();
    let settings_message = callback_query
        .regular_message()
        .map(|msg| (msg.chat.id, msg.id));

    let (toast, edit, pending_action) = {
        let mut meme_store = state.meme_store.lock().await;

        if data == "toggle_upload" {
            if let Err(e) = meme_store.toggle_upload_mode(user_id) {
                error!(user_id, error = %e, "toggle_upload_mode failed");
            }
            let settings = meme_store.get_user_settings(user_id);
            let counts = meme_store.get_user_reaction_counts(user_id);
            (
                None,
                Some(format_settings(&settings, counts)),
                PendingAction::Clear,
            )
        } else if data == "cycle_scope" {
            if let Err(e) = meme_store.cycle_search_scope(user_id) {
                error!(user_id, error = %e, "cycle_search_scope failed");
            }
            let settings = meme_store.get_user_settings(user_id);
            let counts = meme_store.get_user_reaction_counts(user_id);
            (
                None,
                Some(format_settings(&settings, counts)),
                PendingAction::Clear,
            )
        } else if data == "settings:back" {
            let settings = meme_store.get_user_settings(user_id);
            let counts = meme_store.get_user_reaction_counts(user_id);
            (
                None,
                Some(format_settings(&settings, counts)),
                PendingAction::Clear,
            )
        } else if let Some(target) = data.strip_prefix("settings:reset_ask:") {
            let counts = meme_store.get_user_reaction_counts(user_id);
            let target_count = match target {
                "skip" => Some(counts.skips),
                "dislike" => Some(counts.dislikes),
                "like" => Some(counts.likes),
                "all" => Some(counts.total()),
                _ => None,
            };

            match target_count {
                Some(0) => {
                    let settings = meme_store.get_user_settings(user_id);
                    (
                        Some("ℹ️ Немає записів для скидання".to_string()),
                        Some(format_settings(&settings, counts)),
                        PendingAction::Clear,
                    )
                }
                Some(_) if target == "like" || (target == "all" && counts.likes > 0) => {
                    let pending_target = if target == "like" {
                        PendingResetTarget::Likes
                    } else {
                        PendingResetTarget::All
                    };
                    let (action, prompt) =
                        begin_code_reset(pending_target, counts, settings_message);
                    (None, Some(prompt), action)
                }
                Some(_) => (
                    None,
                    format_reset_confirmation(target, counts),
                    PendingAction::Clear,
                ),
                None => (None, None, PendingAction::Keep),
            }
        } else if let Some(target) = data.strip_prefix("settings:reset_confirm:") {
            let counts = meme_store.get_user_reaction_counts(user_id);
            if target == "like" || (target == "all" && counts.likes > 0) {
                let pending_target = if target == "like" {
                    PendingResetTarget::Likes
                } else {
                    PendingResetTarget::All
                };
                let (action, prompt) =
                    begin_code_reset(pending_target, counts, settings_message);
                (
                    Some("🔐 Для скидання лайків введи число з повідомлення".to_string()),
                    Some(prompt),
                    action,
                )
            } else {
                let filter = match target {
                    "skip" => Some(Some("skip")),
                    "dislike" => Some(Some("dislike")),
                    "all" => Some(None),
                    _ => None,
                };

                if let Some(kind) = filter {
                    let toast = match meme_store.reset_user_reactions(user_id, kind) {
                        Ok(deleted) => match target {
                            "skip" => format!("✅ Скинуто скіпів: {}", deleted),
                            "dislike" => format!("✅ Скинуто дізлайків: {}", deleted),
                            _ => format!("✅ Скинуто всі реакції ({})", deleted),
                        },
                        Err(e) => {
                            error!(user_id, error = %e, "Failed to reset user reactions");
                            "❌ Помилка при скиданні".to_string()
                        }
                    };
                    let settings = meme_store.get_user_settings(user_id);
                    let counts = meme_store.get_user_reaction_counts(user_id);
                    (
                        Some(toast),
                        Some(format_settings(&settings, counts)),
                        PendingAction::Clear,
                    )
                } else {
                    (None, None, PendingAction::Keep)
                }
            }
        } else {
            (None, None, PendingAction::Keep)
        }
    };

    match pending_action {
        PendingAction::Clear => {
            state.pending_reaction_resets.lock().await.remove(&user_id);
        }
        PendingAction::Set(pending) => {
            state
                .pending_reaction_resets
                .lock()
                .await
                .insert(user_id, pending);
        }
        PendingAction::Keep => {}
    }

    let answer_req = bot.answer_callback_query(callback_id);
    let answer_res = if let Some(text) = toast {
        answer_req.text(text).await
    } else {
        answer_req.await
    };
    if let Err(e) = answer_res {
        warn!(user_id, error = %e, "Failed to answer settings callback query");
    }

    if let Some((text, keyboard)) = edit
        && let Some((chat_id, msg_id)) = settings_message
        && let Err(e) = bot
            .edit_message_text(chat_id, msg_id, text)
            .reply_markup(keyboard)
            .await
        && !e.to_string().contains("message is not modified")
    {
        warn!(user_id, error = %e, "Failed to edit settings message");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use teloxide::types::InlineKeyboardButtonKind;

    #[test]
    fn generate_reset_code_produces_four_digits_with_entropy() {
        let mut codes = Vec::with_capacity(20);
        for _ in 0..20 {
            let code = generate_reset_code();
            assert_eq!(code.len(), 4);
            assert!(code.chars().all(|c| c.is_ascii_digit()));
            let num: u32 = code.parse().unwrap();
            assert!((1000..=9999).contains(&num));
            codes.push(code);
        }
        let unique: HashSet<&String> = codes.iter().collect();
        assert!(unique.len() > 1);
    }

    #[test]
    fn evaluate_reset_code_input_handles_all_cases() {
        let now = Instant::now();
        let pending = PendingReactionReset {
            code: "4827".to_string(),
            target: PendingResetTarget::Likes,
            settings_message: None,
            expires_at: now + RESET_CODE_TTL,
        };

        assert_eq!(
            evaluate_reset_code_input(&pending, "4827", now),
            CodeInputDecision::Matched
        );
        assert_eq!(
            evaluate_reset_code_input(&pending, " 4827\n", now),
            CodeInputDecision::Matched
        );
        assert_eq!(
            evaluate_reset_code_input(&pending, "1234", now),
            CodeInputDecision::WrongNumber
        );
        assert_eq!(
            evaluate_reset_code_input(&pending, "кіт мем", now),
            CodeInputDecision::NonNumericCancel
        );
        assert_eq!(
            evaluate_reset_code_input(&pending, "4827", now + RESET_CODE_TTL),
            CodeInputDecision::Expired
        );
    }

    #[test]
    fn code_reset_prompt_displays_code_and_only_cancel_button() {
        let counts = UserReactionCounts {
            likes: 5,
            dislikes: 2,
            skips: 3,
        };

        for target in [PendingResetTarget::Likes, PendingResetTarget::All] {
            let (text, kb) = format_code_reset_prompt(target, counts, "9153");
            assert!(text.contains("9153"));
            assert_eq!(kb.inline_keyboard.len(), 1);
            assert_eq!(kb.inline_keyboard[0].len(), 1);
            assert_eq!(
                kb.inline_keyboard[0][0].kind,
                InlineKeyboardButtonKind::CallbackData("settings:back".to_string())
            );
        }
    }

    #[test]
    fn main_keyboard_is_compact_two_by_four_and_auto_hiding() {
        let kb = main_keyboard();
        assert_eq!(kb.keyboard.len(), 2);
        assert_eq!(kb.keyboard[0].len(), 4);
        assert_eq!(kb.keyboard[1].len(), 4);
        assert!(kb.resize_keyboard);
        assert!(kb.one_time_keyboard);
        assert!(!kb.is_persistent);
    }

    #[test]
    fn match_menu_button_accepts_short_and_legacy_labels() {
        assert!(matches!(match_menu_button(BTN_TINDER), Some(Command::Tinder)));
        assert!(matches!(match_menu_button("🔥 Мем-Тіндер"), Some(Command::Tinder)));
        assert!(matches!(match_menu_button(BTN_RANDOM), Some(Command::Random)));
        assert!(matches!(match_menu_button("🎲 Випадковий мем"), Some(Command::Random)));
        assert!(matches!(match_menu_button(BTN_LIKED), Some(Command::Liked)));
        assert!(matches!(match_menu_button("❤️ Вподобані"), Some(Command::Liked)));
        assert!(matches!(match_menu_button(BTN_TAGS), Some(Command::Tags)));
        assert!(matches!(match_menu_button(BTN_DRIP), Some(Command::Drip)));
        assert!(matches!(match_menu_button(BTN_STATS), Some(Command::Stats)));
        assert!(matches!(match_menu_button("📊 Статистика"), Some(Command::Stats)));
        assert!(matches!(match_menu_button(BTN_SETTINGS), Some(Command::Settings)));
        assert!(matches!(match_menu_button("⚙️ Налаштування"), Some(Command::Settings)));
        assert!(matches!(match_menu_button(BTN_HELP), Some(Command::Help)));
        assert!(matches!(match_menu_button("❓ Допомога"), Some(Command::Help)));
        assert!(match_menu_button("#коти мем").is_none());
    }
}
