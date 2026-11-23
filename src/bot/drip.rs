use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use teloxide::Bot;
use teloxide::prelude::*;
use teloxide::types::{
    ChatId, FileId, InlineKeyboardButton, InlineKeyboardMarkup, InputFile, Message, ParseMode,
};
use tokio::time::{MissedTickBehavior, interval};
use tracing::{info, warn};

use crate::AppState;
use crate::database::entity::media_type::MediaType;
use crate::database::entity::meme::Meme;
use crate::meme_store::DripSubscription;

pub const DEFAULT_MEMES_PER_DAY: u8 = 3;
pub const DEFAULT_TZ_OFFSET_HOURS: i8 = 2;
pub const DEFAULT_QUIET_START_HOUR: u8 = 23;
pub const DEFAULT_QUIET_END_HOUR: u8 = 9;

const MIN_INTERVAL_SECS: u64 = 1800;
const ALLOWED_RATES: &[u8] = &[1, 3, 5];
const ALLOWED_TZ_OFFSETS: &[i8] = &[0, 1, 2, 3];

fn unix_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn is_in_quiet_hours(local_hour: u8, quiet_start: u8, quiet_end: u8) -> bool {
    if quiet_start == quiet_end {
        return false;
    }
    if quiet_start > quiet_end {
        local_hour >= quiet_start || local_hour < quiet_end
    } else {
        local_hour >= quiet_start && local_hour < quiet_end
    }
}

pub fn compute_next_delivery_at_with_samples(
    now_secs: u64,
    memes_per_day: u8,
    tz_offset_hours: i8,
    quiet_start: u8,
    quiet_end: u8,
    jitter_unit: f64,
    morning_jitter_secs: u64,
) -> u64 {
    let quiet_hours = if quiet_start > quiet_end {
        (24 - quiet_start as i64) + quiet_end as i64
    } else {
        (quiet_end as i64) - (quiet_start as i64)
    };
    let active_secs = ((24 - quiet_hours).max(1) * 3600) as u64;
    let avg_interval = active_secs / (memes_per_day.clamp(1, 10) as u64);
    let min_delay = (avg_interval / 2).max(MIN_INTERVAL_SECS);
    let max_delay = ((avg_interval * 3) / 2).max(min_delay + 60);

    let jitter_clamped = jitter_unit.clamp(0.0, 1.0);
    let delay = min_delay + (((max_delay - min_delay) as f64) * jitter_clamped).round() as u64;
    let candidate = now_secs + delay;

    let offset_secs = (tz_offset_hours as i64) * 3600;
    let local_secs = candidate as i64 + offset_secs;
    let secs_since_midnight = local_secs.rem_euclid(86_400);
    let local_hour = (secs_since_midnight / 3600) as u8;

    if is_in_quiet_hours(local_hour, quiet_start, quiet_end) {
        let day_start_local = local_secs - secs_since_midnight;
        let quiet_end_secs = (quiet_end as i64) * 3600;
        let next_morning_local = if secs_since_midnight < quiet_end_secs {
            day_start_local + quiet_end_secs
        } else {
            day_start_local + 86_400 + quiet_end_secs
        };
        let utc_morning = (next_morning_local - offset_secs).max(0) as u64;
        utc_morning + morning_jitter_secs.clamp(300, 2700)
    } else {
        candidate
    }
}

pub fn compute_next_delivery_at(
    now_secs: u64,
    memes_per_day: u8,
    tz_offset_hours: i8,
    quiet_start: u8,
    quiet_end: u8,
) -> u64 {
    let mut hasher = RandomState::new().build_hasher();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    hasher.write_u64(nanos);
    let r1 = hasher.finish();
    hasher.write_u64(r1 ^ 0x9e37_79b9_7f4a_7c15);
    let r2 = hasher.finish();

    let jitter_unit = ((r1 % 10_001) as f64) / 10_000.0;
    let morning_jitter_secs = 300 + (r2 % 2401);

    compute_next_delivery_at_with_samples(
        now_secs,
        memes_per_day,
        tz_offset_hours,
        quiet_start,
        quiet_end,
        jitter_unit,
        morning_jitter_secs,
    )
}

fn default_subscription(
    user_id: i64,
    chat_id: i64,
    is_active: bool,
    next_delivery_at: u64,
) -> DripSubscription {
    DripSubscription {
        user_id,
        chat_id,
        memes_per_day: DEFAULT_MEMES_PER_DAY,
        tz_offset_hours: DEFAULT_TZ_OFFSET_HOURS,
        quiet_start_hour: DEFAULT_QUIET_START_HOUR,
        quiet_end_hour: DEFAULT_QUIET_END_HOUR,
        next_delivery_at,
        is_active,
    }
}

fn format_eta(next_delivery_at: u64, now_secs: u64) -> String {
    if next_delivery_at <= now_secs {
        return "незабаром".to_string();
    }

    let diff_mins = (next_delivery_at - now_secs).div_ceil(60);
    if diff_mins < 60 {
        format!("~{} хв", diff_mins)
    } else {
        let hours = diff_mins / 60;
        let mins = diff_mins % 60;
        if mins == 0 {
            format!("~{} год", hours)
        } else {
            format!("~{} год {} хв", hours, mins)
        }
    }
}

fn format_tz_label(offset: i8) -> String {
    if offset >= 0 {
        format!("UTC+{}", offset)
    } else {
        format!("UTC{}", offset)
    }
}

fn format_drip_panel(
    sub: Option<&DripSubscription>,
    delivered_count: usize,
    now_secs: u64,
) -> (String, InlineKeyboardMarkup) {
    let rate = sub.map(|s| s.memes_per_day).unwrap_or(DEFAULT_MEMES_PER_DAY);
    let tz = sub
        .map(|s| s.tz_offset_hours)
        .unwrap_or(DEFAULT_TZ_OFFSET_HOURS);
    let quiet_start = sub
        .map(|s| s.quiet_start_hour)
        .unwrap_or(DEFAULT_QUIET_START_HOUR);
    let quiet_end = sub
        .map(|s| s.quiet_end_hour)
        .unwrap_or(DEFAULT_QUIET_END_HOUR);
    let is_active = sub.is_some_and(|s| s.is_active);

    let status_line = match sub {
        Some(s) if s.is_active => "✅ Активна",
        Some(_) => "⏸ На паузі",
        None => "🔕 Не налаштована",
    };

    let next_line = if let Some(s) = sub
        && s.is_active
    {
        format!(
            "\n🎲 Наступний сюрприз: через {}",
            format_eta(s.next_delivery_at, now_secs)
        )
    } else {
        String::new()
    };

    let text = format!(
        "🎁 Підписка на меми\n\n\
         Статус: {}\n\
         Частота: {} мем(ів)/день (випадковий час)\n\
         Тихі години: {:02}:00–{:02}:00 ({})\n\
         Надіслано раніше: {}{}",
        status_line,
        rate,
        quiet_start,
        quiet_end,
        format_tz_label(tz),
        delivered_count,
        next_line
    );

    let toggle_btn = if is_active {
        InlineKeyboardButton::callback("⏸ Поставити на паузу", "drip:toggle")
    } else {
        InlineKeyboardButton::callback("▶️ Увімкнути підписку", "drip:toggle")
    };

    let rate_row: Vec<InlineKeyboardButton> = ALLOWED_RATES
        .iter()
        .map(|&r| {
            let label = if r == rate {
                format!("✅ {}/день", r)
            } else {
                format!("{}/день", r)
            };
            InlineKeyboardButton::callback(label, format!("drip:rate:{}", r))
        })
        .collect();

    let mut rows = vec![
        vec![toggle_btn],
        rate_row,
        vec![InlineKeyboardButton::callback(
            format!("🕒 Часовий пояс: {}", format_tz_label(tz)),
            "drip:tz",
        )],
    ];

    if delivered_count > 0 {
        rows.push(vec![InlineKeyboardButton::callback(
            format!("🔄 Скинути історію доставок ({})", delivered_count),
            "drip:reset_history",
        )]);
    }

    (text, InlineKeyboardMarkup::new(rows))
}

fn drip_meme_caption(meme: &Meme) -> String {
    format!(
        "🎲 Мем-сюрприз{}",
        crate::bot::tinder::format_source_html(meme)
    )
}

fn drip_meme_keyboard(meme_id: u64) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![vec![
        InlineKeyboardButton::callback("❤️ Лайк", format!("drip:like:{}", meme_id)),
        InlineKeyboardButton::callback("👎", format!("drip:dislike:{}", meme_id)),
    ]])
}

async fn send_drip_meme(
    bot: &Bot,
    chat_id: ChatId,
    meme: &Meme,
) -> Result<Message, teloxide::RequestError> {
    let file = InputFile::file_id(FileId(meme.file_tg_id.clone()));
    let caption = drip_meme_caption(meme);
    let keyboard = drip_meme_keyboard(meme.id);

    match meme.media_type {
        MediaType::Photo => {
            bot.send_photo(chat_id, file)
                .caption(caption)
                .parse_mode(ParseMode::Html)
                .reply_markup(keyboard)
                .await
        }
        MediaType::Animation => {
            bot.send_animation(chat_id, file)
                .caption(caption)
                .parse_mode(ParseMode::Html)
                .reply_markup(keyboard)
                .await
        }
    }
}

pub async fn cmd_drip(bot: Bot, msg: Message, state: Arc<AppState>) -> ResponseResult<()> {
    let user_id = msg.chat.id.0;
    let now_secs = unix_now_secs();
    let (sub, delivered_count) = {
        let store = state.meme_store.lock().await;
        (
            store.get_drip_subscription(user_id),
            store.count_user_delivered_memes(user_id),
        )
    };

    let (text, keyboard) = format_drip_panel(sub.as_ref(), delivered_count, now_secs);
    bot.send_message(msg.chat.id, text)
        .reply_markup(keyboard)
        .await?;
    Ok(())
}

pub async fn handle_drip_callback(
    bot: Bot,
    callback_query: CallbackQuery,
    state: Arc<AppState>,
    data: &str,
) -> ResponseResult<()> {
    let user_id = callback_query.from.id.0 as i64;
    let chat_id = callback_query
        .regular_message()
        .map(|m| m.chat.id.0)
        .unwrap_or(user_id);
    let callback_id = callback_query.id.clone();

    if data == "drip:noop" {
        let _ = bot.answer_callback_query(callback_id).await;
        return Ok(());
    }

    if let Some(meme_id) = data
        .strip_prefix("drip:like:")
        .and_then(|s| s.parse::<u64>().ok())
    {
        let toast = {
            let store = state.meme_store.lock().await;
            match store.record_reaction(user_id, meme_id, "like") {
                Ok(()) => "❤️ Додано у /liked",
                Err(e) => {
                    warn!(user_id, meme_id, error = %e, "Failed to record drip like");
                    "⚠️ Помилка збереження"
                }
            }
        };

        let _ = bot.answer_callback_query(callback_id).text(toast).await;
        if let Some(msg) = callback_query.regular_message() {
            let liked_kb = InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::callback(
                "✅ Вподобано",
                "drip:noop",
            )]]);
            if let Err(e) = bot
                .edit_message_reply_markup(msg.chat.id, msg.id)
                .reply_markup(liked_kb)
                .await
                && !e.to_string().contains("message is not modified")
            {
                warn!(user_id, meme_id, error = %e, "Failed to update drip meme markup");
            }
        }
        return Ok(());
    }

    if let Some(meme_id) = data
        .strip_prefix("drip:dislike:")
        .and_then(|s| s.parse::<u64>().ok())
    {
        let toast = {
            let store = state.meme_store.lock().await;
            match store.record_reaction(user_id, meme_id, "dislike") {
                Ok(()) => "👎 Приховано",
                Err(e) => {
                    warn!(user_id, meme_id, error = %e, "Failed to record drip dislike");
                    "⚠️ Помилка збереження"
                }
            }
        };

        let _ = bot.answer_callback_query(callback_id).text(toast).await;
        if let Some(msg) = callback_query.regular_message()
            && let Err(e) = bot
                .edit_message_reply_markup(msg.chat.id, msg.id)
                .reply_markup(InlineKeyboardMarkup::default())
                .await
            && !e.to_string().contains("message is not modified")
        {
            warn!(user_id, meme_id, error = %e, "Failed to clear drip meme markup");
        }
        return Ok(());
    }

    let now_secs = unix_now_secs();
    let (toast, panel) = {
        let store = state.meme_store.lock().await;
        let existing = store.get_drip_subscription(user_id);

        let toast: Option<String> = if data == "drip:toggle" {
            let mut sub = existing.unwrap_or_else(|| default_subscription(user_id, chat_id, false, 0));
            sub.chat_id = chat_id;
            sub.is_active = !sub.is_active;
            if sub.is_active {
                sub.next_delivery_at = compute_next_delivery_at(
                    now_secs,
                    sub.memes_per_day,
                    sub.tz_offset_hours,
                    sub.quiet_start_hour,
                    sub.quiet_end_hour,
                );
            }
            if let Err(e) = store.upsert_drip_subscription(&sub) {
                warn!(user_id, error = %e, "Failed to toggle drip subscription");
                Some("❌ Помилка збереження".to_string())
            } else if sub.is_active {
                Some("✅ Підписку увімкнено!".to_string())
            } else {
                Some("⏸ Підписку поставлено на паузу".to_string())
            }
        } else if let Some(rate) = data
            .strip_prefix("drip:rate:")
            .and_then(|s| s.parse::<u8>().ok())
            .filter(|r| ALLOWED_RATES.contains(r))
        {
            let mut sub = existing.unwrap_or_else(|| default_subscription(user_id, chat_id, true, 0));
            sub.chat_id = chat_id;
            sub.memes_per_day = rate;
            if sub.is_active {
                sub.next_delivery_at = compute_next_delivery_at(
                    now_secs,
                    sub.memes_per_day,
                    sub.tz_offset_hours,
                    sub.quiet_start_hour,
                    sub.quiet_end_hour,
                );
            }
            if let Err(e) = store.upsert_drip_subscription(&sub) {
                warn!(user_id, error = %e, "Failed to update drip rate");
                Some("❌ Помилка збереження".to_string())
            } else {
                Some(format!("✅ Частота: {} мем(ів)/день", rate))
            }
        } else if data == "drip:tz" {
            let mut sub = existing.unwrap_or_else(|| default_subscription(user_id, chat_id, false, 0));
            sub.chat_id = chat_id;
            let cur_pos = ALLOWED_TZ_OFFSETS
                .iter()
                .position(|&o| o == sub.tz_offset_hours)
                .unwrap_or(0);
            sub.tz_offset_hours = ALLOWED_TZ_OFFSETS[(cur_pos + 1) % ALLOWED_TZ_OFFSETS.len()];
            if sub.is_active {
                sub.next_delivery_at = compute_next_delivery_at(
                    now_secs,
                    sub.memes_per_day,
                    sub.tz_offset_hours,
                    sub.quiet_start_hour,
                    sub.quiet_end_hour,
                );
            }
            if let Err(e) = store.upsert_drip_subscription(&sub) {
                warn!(user_id, error = %e, "Failed to update drip timezone");
                Some("❌ Помилка збереження".to_string())
            } else {
                Some(format!("🕒 Часовий пояс: {}", format_tz_label(sub.tz_offset_hours)))
            }
        } else if data == "drip:reset_history" {
            match store.reset_user_delivered_memes(user_id) {
                Ok(deleted) => Some(format!("🔄 Історію доставок очищено ({})", deleted)),
                Err(e) => {
                    warn!(user_id, error = %e, "Failed to reset drip delivery history");
                    Some("❌ Помилка очищення".to_string())
                }
            }
        } else {
            None
        };

        let updated_sub = store.get_drip_subscription(user_id);
        let delivered = store.count_user_delivered_memes(user_id);
        (toast, format_drip_panel(updated_sub.as_ref(), delivered, now_secs))
    };

    let answer_req = bot.answer_callback_query(callback_id);
    let answer_res = if let Some(text) = toast {
        answer_req.text(text).await
    } else {
        answer_req.await
    };
    if let Err(e) = answer_res {
        warn!(user_id, error = %e, "Failed to answer drip callback query");
    }

    if let Some(msg) = callback_query.regular_message()
        && let Err(e) = bot
            .edit_message_text(msg.chat.id, msg.id, panel.0)
            .reply_markup(panel.1)
            .await
        && !e.to_string().contains("message is not modified")
    {
        warn!(user_id, error = %e, "Failed to edit drip settings panel");
    }

    Ok(())
}

pub fn start_drip_scheduler(state: Arc<AppState>, bot: Bot) {
    tokio::spawn(async move {
        info!("Background Meme Drip scheduler started");

        let mut timer = interval(Duration::from_secs(60));
        timer.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            timer.tick().await;
            let now_secs = unix_now_secs();

            let due = {
                let store = state.meme_store.lock().await;
                store.get_due_drip_subscriptions(now_secs)
            };

            for sub in due {
                let meme_opt = {
                    let store = state.meme_store.lock().await;
                    store.pick_next_drip_meme(sub.user_id)
                };

                let Some(meme) = meme_opt else {
                    {
                        let store = state.meme_store.lock().await;
                        if let Err(e) = store.set_drip_active(sub.user_id, false) {
                            warn!(user_id = sub.user_id, error = %e, "Failed to pause exhausted drip subscription");
                        }
                    }
                    info!(
                        user_id = sub.user_id,
                        "No unseen memes left for drip; pausing subscription"
                    );
                    let _ = bot
                        .send_message(
                            ChatId(sub.chat_id),
                            "🎲 Усі доступні меми вже надіслано! Поставив підписку на паузу — можеш скинути історію доставок у /drip.",
                        )
                        .await;
                    continue;
                };

                match send_drip_meme(&bot, ChatId(sub.chat_id), &meme).await {
                    Ok(_) => {
                        let next_at = compute_next_delivery_at(
                            now_secs,
                            sub.memes_per_day,
                            sub.tz_offset_hours,
                            sub.quiet_start_hour,
                            sub.quiet_end_hour,
                        );
                        {
                            let store = state.meme_store.lock().await;
                            if let Err(e) =
                                store.record_delivered_meme(sub.user_id, meme.id, now_secs)
                            {
                                warn!(user_id = sub.user_id, meme_id = meme.id, error = %e, "Failed to record delivered drip meme");
                            }
                            if let Err(e) = store.update_drip_next_delivery(sub.user_id, next_at) {
                                warn!(user_id = sub.user_id, error = %e, "Failed to update next drip delivery");
                            }
                        }
                        info!(
                            user_id = sub.user_id,
                            meme_id = meme.id,
                            next_delivery_at = next_at,
                            "Delivered drip meme"
                        );
                    }
                    Err(e) => {
                        let err_str = e.to_string().to_lowercase();
                        if err_str.contains("blocked")
                            || err_str.contains("chat not found")
                            || err_str.contains("user is deactivated")
                        {
                            let store = state.meme_store.lock().await;
                            let _ = store.set_drip_active(sub.user_id, false);
                            warn!(
                                user_id = sub.user_id,
                                error = %e,
                                "User unreachable; pausing drip subscription"
                            );
                        } else {
                            let store = state.meme_store.lock().await;
                            let _ = store.update_drip_next_delivery(sub.user_id, now_secs + 600);
                            warn!(
                                user_id = sub.user_id,
                                error = %e,
                                "Failed to deliver drip meme; retrying in 10m"
                            );
                        }
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_hours_wrap_around_and_daytime_checks() {
        assert!(!is_in_quiet_hours(22, 23, 9));
        assert!(is_in_quiet_hours(23, 23, 9));
        assert!(is_in_quiet_hours(0, 23, 9));
        assert!(is_in_quiet_hours(8, 23, 9));
        assert!(!is_in_quiet_hours(9, 23, 9));
        assert!(!is_in_quiet_hours(15, 23, 9));
        assert!(!is_in_quiet_hours(3, 9, 9));
    }

    #[test]
    fn schedule_shifts_quiet_hours_candidate_to_morning_with_jitter() {
        // 20:30:00 UTC = 22:30:00 local (UTC+2).
        // With 3/day over 14 active hours -> avg_interval=16800s -> min_delay=8400s (2h20m).
        // Minimum candidate lands at 00:50 local (quiet hours 23..9), so it must shift to 09:05..09:45 local.
        let midnight_utc = (1_795_392_000_u64 / 86_400) * 86_400;
        let now_2230_local_utc2 = midnight_utc + 20 * 3600 + 1800;
        let tz = 2;
        let scheduled = compute_next_delivery_at_with_samples(
            now_2230_local_utc2,
            3,
            tz,
            23,
            9,
            0.0,
            900,
        );

        let local_secs = scheduled as i64 + (tz as i64) * 3600;
        let secs_since_midnight = local_secs.rem_euclid(86_400);
        assert_eq!(secs_since_midnight, 9 * 3600 + 900);
        assert!(scheduled > now_2230_local_utc2);
    }

    #[test]
    fn schedule_keeps_daytime_candidate_within_jitter_bounds() {
        // Choose a timestamp at exactly 10:00:00 UTC+2 -> 08:00:00 UTC
        let day_base_utc: u64 = 1_795_392_000; // multiple of 86_400? Let's verify cleanly:
        let midnight_utc = (day_base_utc / 86_400) * 86_400;
        let ten_am_utc2 = midnight_utc + 8 * 3600; // 10:00 local UTC+2

        // 3/day over 14 active hours = 50400s -> avg=16800s -> min=8400s (~2h20m), max=25200s (7h)
        // Both 12:20 local and 17:00 local are strictly outside quiet hours (23..9).
        let at_min = compute_next_delivery_at_with_samples(ten_am_utc2, 3, 2, 23, 9, 0.0, 600);
        let at_max = compute_next_delivery_at_with_samples(ten_am_utc2, 3, 2, 23, 9, 1.0, 600);
        let at_mid = compute_next_delivery_at_with_samples(ten_am_utc2, 3, 2, 23, 9, 0.5, 600);

        assert_eq!(at_min - ten_am_utc2, 8400);
        assert_eq!(at_max - ten_am_utc2, 25200);
        assert!(at_mid > at_min && at_mid < at_max);
    }
}
