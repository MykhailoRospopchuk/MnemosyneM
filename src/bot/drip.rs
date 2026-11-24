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
use crate::meme_store::{DripSubscription, MemeStore};

pub const DEFAULT_MEMES_PER_DAY: u8 = 3;
pub const DEFAULT_TZ_OFFSET_HOURS: i8 = 2;
pub const DEFAULT_QUIET_START_HOUR: u8 = 23;
pub const DEFAULT_QUIET_END_HOUR: u8 = 9;

const END_BUFFER_SECS: u64 = 300;
const MIN_LEAD_SECS: u64 = 120;
const MIN_SPACING_SECS: u64 = 900;
const ALLOWED_RATES: &[u8] = &[1, 3, 5];
const ALLOWED_TZ_OFFSETS: &[i8] = &[0, 1, 2, 3];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActiveWindow {
    pub start_utc: u64,
    pub end_utc: u64,
    pub is_active_now: bool,
}

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

pub fn is_timestamp_in_quiet_hours(
    ts_secs: u64,
    tz_offset_hours: i8,
    quiet_start: u8,
    quiet_end: u8,
) -> bool {
    let offset_secs = (tz_offset_hours as i64) * 3600;
    let local_secs = ts_secs as i64 + offset_secs;
    let local_hour = (local_secs.rem_euclid(86_400) / 3600) as u8;
    is_in_quiet_hours(local_hour, quiet_start, quiet_end)
}

pub fn active_window_for(
    now_secs: u64,
    tz_offset_hours: i8,
    quiet_start: u8,
    quiet_end: u8,
) -> ActiveWindow {
    let offset = (tz_offset_hours as i64) * 3600;
    let local_now = now_secs as i64 + offset;
    let tod = local_now.rem_euclid(86_400);
    let day_start = local_now - tod;
    let qs = (quiet_start as i64) * 3600;
    let qe = (quiet_end as i64) * 3600;

    let (start_local, end_local, is_active_now) = if quiet_start == quiet_end {
        (day_start, day_start + 86_400, true)
    } else if quiet_start > quiet_end {
        if tod >= qe && tod < qs {
            (day_start + qe, day_start + qs, true)
        } else if tod < qe {
            (day_start + qe, day_start + qs, false)
        } else {
            (day_start + 86_400 + qe, day_start + 86_400 + qs, false)
        }
    } else if tod >= qe {
        (day_start + qe, day_start + 86_400 + qs, true)
    } else if tod < qs {
        (day_start - 86_400 + qe, day_start + qs, true)
    } else {
        (day_start + qe, day_start + 86_400 + qs, false)
    };

    ActiveWindow {
        start_utc: (start_local - offset).max(0) as u64,
        end_utc: (end_local - offset).max(0) as u64,
        is_active_now,
    }
}

pub fn compute_next_delivery_at_with_sample(
    now_secs: u64,
    memes_per_day: u8,
    delivered_in_window: usize,
    tz_offset_hours: i8,
    quiet_start: u8,
    quiet_end: u8,
    jitter_unit: f64,
) -> u64 {
    let rate = memes_per_day.clamp(1, 10) as usize;
    let jitter = jitter_unit.clamp(0.0, 1.0);
    let win = active_window_for(now_secs, tz_offset_hours, quiet_start, quiet_end);

    let pick_in_span = |eff_start: u64, eff_end: u64, slots: usize, after_delivery: bool| -> u64 {
        let span = eff_end.saturating_sub(eff_start);
        if span == 0 {
            return eff_start;
        }
        let slot_len = (span / (slots.max(1) as u64)).max(1);
        let (low_frac, high_frac) = if after_delivery {
            (0.45, 0.85)
        } else {
            (0.15, 0.85)
        };
        let mut low = ((slot_len as f64) * low_frac).round() as u64;
        if eff_start == now_secs {
            low = low.max(MIN_LEAD_SECS.min(span));
        }
        let high = (((slot_len as f64) * high_frac).round() as u64)
            .max(low)
            .min(span);
        let delay = low + (((high - low) as f64) * jitter).round() as u64;
        eff_start + delay
    };

    if win.is_active_now {
        let remaining = rate.saturating_sub(delivered_in_window);
        let eff_start = now_secs.max(win.start_utc);
        let eff_end = win.end_utc.saturating_sub(END_BUFFER_SECS);
        let usable = eff_end.saturating_sub(eff_start);
        let has_room = if delivered_in_window == 0 {
            usable >= MIN_LEAD_SECS
        } else {
            usable >= MIN_SPACING_SECS
        };

        if remaining > 0 && has_room {
            let max_fit = ((usable / MIN_SPACING_SECS) as usize).max(1);
            let effective_slots = remaining.min(max_fit);
            pick_in_span(eff_start, eff_end, effective_slots, delivered_in_window > 0)
        } else {
            let next_start = win.start_utc + 86_400;
            let next_end = (win.end_utc + 86_400).saturating_sub(END_BUFFER_SECS);
            pick_in_span(next_start, next_end, rate, false)
        }
    } else {
        let eff_end = win.end_utc.saturating_sub(END_BUFFER_SECS);
        pick_in_span(win.start_utc, eff_end, rate, false)
    }
}

pub fn compute_next_delivery_at(
    now_secs: u64,
    memes_per_day: u8,
    delivered_in_window: usize,
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
    let jitter_unit = ((r1 % 10_001) as f64) / 10_000.0;

    compute_next_delivery_at_with_sample(
        now_secs,
        memes_per_day,
        delivered_in_window,
        tz_offset_hours,
        quiet_start,
        quiet_end,
        jitter_unit,
    )
}

fn delivered_in_current_window(
    store: &MemeStore,
    sub: &DripSubscription,
    now_secs: u64,
) -> (ActiveWindow, usize) {
    let win = active_window_for(
        now_secs,
        sub.tz_offset_hours,
        sub.quiet_start_hour,
        sub.quiet_end_hour,
    );
    let delivered = if win.is_active_now {
        store.count_user_delivered_in_window(sub.user_id, win.start_utc, win.end_utc)
    } else {
        0
    };
    (win, delivered)
}

fn schedule_next_for_store(store: &MemeStore, sub: &DripSubscription, now_secs: u64) -> u64 {
    let (_win, delivered) = delivered_in_current_window(store, sub, now_secs);
    compute_next_delivery_at(
        now_secs,
        sub.memes_per_day,
        delivered,
        sub.tz_offset_hours,
        sub.quiet_start_hour,
        sub.quiet_end_hour,
    )
}

pub fn needs_reconcile(
    sub: &DripSubscription,
    delivered_in_window: usize,
    now_secs: u64,
) -> bool {
    let win = active_window_for(
        now_secs,
        sub.tz_offset_hours,
        sub.quiet_start_hour,
        sub.quiet_end_hour,
    );
    let rate = sub.memes_per_day.clamp(1, 10) as usize;

    if win.is_active_now {
        let remaining = rate.saturating_sub(delivered_in_window);
        let eff_end = win.end_utc.saturating_sub(END_BUFFER_SECS);
        let usable = eff_end.saturating_sub(now_secs.max(win.start_utc));
        let has_room = if delivered_in_window == 0 {
            usable >= MIN_LEAD_SECS
        } else {
            usable >= MIN_SPACING_SECS
        };

        (remaining > 0 && has_room && sub.next_delivery_at >= win.end_utc)
            || (remaining == 0 && sub.next_delivery_at < win.end_utc)
            || (sub.next_delivery_at > now_secs
                && is_timestamp_in_quiet_hours(
                    sub.next_delivery_at,
                    sub.tz_offset_hours,
                    sub.quiet_start_hour,
                    sub.quiet_end_hour,
                ))
    } else {
        sub.next_delivery_at < win.start_utc
            || is_timestamp_in_quiet_hours(
                sub.next_delivery_at,
                sub.tz_offset_hours,
                sub.quiet_start_hour,
                sub.quiet_end_hour,
            )
    }
}

fn reconcile_active_drip_schedules(store: &MemeStore, now_secs: u64) {
    for sub in store.get_active_drip_subscriptions() {
        let (_win, delivered) = delivered_in_current_window(store, &sub, now_secs);
        if needs_reconcile(&sub, delivered, now_secs) {
            let new_next = compute_next_delivery_at(
                now_secs,
                sub.memes_per_day,
                delivered,
                sub.tz_offset_hours,
                sub.quiet_start_hour,
                sub.quiet_end_hour,
            );
            if let Err(e) = store.update_drip_next_delivery(sub.user_id, new_next) {
                warn!(
                    user_id = sub.user_id,
                    error = %e,
                    "Failed to reconcile drip schedule"
                );
            } else {
                info!(
                    user_id = sub.user_id,
                    old_next_delivery_at = sub.next_delivery_at,
                    new_next_delivery_at = new_next,
                    delivered_in_window = delivered,
                    memes_per_day = sub.memes_per_day,
                    "Reconciled drip delivery schedule"
                );
            }
        }
    }
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
                sub.next_delivery_at = schedule_next_for_store(&store, &sub, now_secs);
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
                sub.next_delivery_at = schedule_next_for_store(&store, &sub, now_secs);
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
                sub.next_delivery_at = schedule_next_for_store(&store, &sub, now_secs);
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
        {
            let store = state.meme_store.lock().await;
            reconcile_active_drip_schedules(&store, unix_now_secs());
        }

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
                let (win, delivered_in_win) = {
                    let store = state.meme_store.lock().await;
                    delivered_in_current_window(&store, &sub, now_secs)
                };

                if !win.is_active_now || delivered_in_win >= sub.memes_per_day.clamp(1, 10) as usize
                {
                    let next_at = compute_next_delivery_at(
                        now_secs,
                        sub.memes_per_day,
                        delivered_in_win,
                        sub.tz_offset_hours,
                        sub.quiet_start_hour,
                        sub.quiet_end_hour,
                    );
                    {
                        let store = state.meme_store.lock().await;
                        if let Err(e) = store.update_drip_next_delivery(sub.user_id, next_at) {
                            warn!(user_id = sub.user_id, error = %e, "Failed to defer drip delivery outside active window");
                        }
                    }
                    info!(
                        user_id = sub.user_id,
                        next_delivery_at = next_at,
                        is_active_window = win.is_active_now,
                        delivered_in_window = delivered_in_win,
                        "Deferred drip delivery to active window"
                    );
                    continue;
                }

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
                        let next_at = {
                            let store = state.meme_store.lock().await;
                            if let Err(e) =
                                store.record_delivered_meme(sub.user_id, meme.id, now_secs)
                            {
                                warn!(user_id = sub.user_id, meme_id = meme.id, error = %e, "Failed to record delivered drip meme");
                            }
                            let next_at = schedule_next_for_store(&store, &sub, now_secs);
                            if let Err(e) = store.update_drip_next_delivery(sub.user_id, next_at) {
                                warn!(user_id = sub.user_id, error = %e, "Failed to update next drip delivery");
                            }
                            next_at
                        };
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

    fn local_utc2_ts(day_offset: u64, hour: u64, min: u64, sec: u64) -> u64 {
        let base_midnight_utc = (1_795_392_000_u64 / 86_400) * 86_400;
        let local_secs = base_midnight_utc + day_offset * 86_400 + hour * 3600 + min * 60 + sec;
        local_secs - 2 * 3600
    }

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
    fn active_window_handles_overnight_daytime_and_disabled_quiet_hours() {
        // Overnight quiet hours 23..9 (UTC+2)
        let morning_0200 = local_utc2_ts(0, 2, 0, 0);
        let win_early = active_window_for(morning_0200, 2, 23, 9);
        assert!(!win_early.is_active_now);
        assert_eq!(win_early.start_utc, local_utc2_ts(0, 9, 0, 0));
        assert_eq!(win_early.end_utc, local_utc2_ts(0, 23, 0, 0));

        let afternoon_1530 = local_utc2_ts(0, 15, 30, 0);
        let win_day = active_window_for(afternoon_1530, 2, 23, 9);
        assert!(win_day.is_active_now);
        assert_eq!(win_day.start_utc, local_utc2_ts(0, 9, 0, 0));
        assert_eq!(win_day.end_utc, local_utc2_ts(0, 23, 0, 0));

        let late_2330 = local_utc2_ts(0, 23, 30, 0);
        let win_late = active_window_for(late_2330, 2, 23, 9);
        assert!(!win_late.is_active_now);
        assert_eq!(win_late.start_utc, local_utc2_ts(1, 9, 0, 0));
        assert_eq!(win_late.end_utc, local_utc2_ts(1, 23, 0, 0));

        // Daytime quiet hours 12..16 (UTC+2)
        let pre_noon = local_utc2_ts(1, 8, 0, 0);
        let win_dt_early = active_window_for(pre_noon, 2, 12, 16);
        assert!(win_dt_early.is_active_now);
        assert_eq!(win_dt_early.start_utc, local_utc2_ts(0, 16, 0, 0));
        assert_eq!(win_dt_early.end_utc, local_utc2_ts(1, 12, 0, 0));

        let mid_quiet = local_utc2_ts(1, 13, 0, 0);
        let win_dt_quiet = active_window_for(mid_quiet, 2, 12, 16);
        assert!(!win_dt_quiet.is_active_now);
        assert_eq!(win_dt_quiet.start_utc, local_utc2_ts(1, 16, 0, 0));
        assert_eq!(win_dt_quiet.end_utc, local_utc2_ts(2, 12, 0, 0));

        let evening = local_utc2_ts(1, 17, 0, 0);
        let win_dt_after = active_window_for(evening, 2, 12, 16);
        assert!(win_dt_after.is_active_now);
        assert_eq!(win_dt_after.start_utc, local_utc2_ts(1, 16, 0, 0));
        assert_eq!(win_dt_after.end_utc, local_utc2_ts(2, 12, 0, 0));

        // Disabled quiet hours 9..9
        let win_disabled = active_window_for(afternoon_1530, 2, 9, 9);
        assert!(win_disabled.is_active_now);
        assert_eq!(win_disabled.start_utc, local_utc2_ts(0, 0, 0, 0));
        assert_eq!(win_disabled.end_utc, local_utc2_ts(1, 0, 0, 0));
    }

    #[test]
    fn midday_single_meme_subscriber_receives_today_outside_quiet_hours() {
        // Reproduces Kate R: 1/day subscribed at 15:32:39 UTC+2, 0 delivered today
        let now = local_utc2_ts(0, 15, 32, 39);
        let today_eff_end = local_utc2_ts(0, 22, 55, 0);
        let tomorrow_start = local_utc2_ts(1, 9, 0, 0);
        let tomorrow_eff_end = local_utc2_ts(1, 22, 55, 0);

        for jitter in [0.0, 0.5, 1.0] {
            let first = compute_next_delivery_at_with_sample(now, 1, 0, 2, 23, 9, jitter);
            assert!(
                first > now && first <= today_eff_end,
                "Expected today in (15:32:39, 22:55], got {} for jitter {}",
                first,
                jitter
            );
            assert!(!is_timestamp_in_quiet_hours(first, 2, 23, 9));

            // After delivery #1 today, next delivery rolls over to tomorrow's window
            let second = compute_next_delivery_at_with_sample(first, 1, 1, 2, 23, 9, jitter);
            assert!(
                second >= tomorrow_start && second <= tomorrow_eff_end,
                "Expected tomorrow [09:00, 22:55], got {}",
                second
            );
            assert!(!is_timestamp_in_quiet_hours(second, 2, 23, 9));
        }
    }

    #[test]
    fn midday_three_memes_subscriber_gets_all_three_today() {
        // Reproduces Tanya: 3/day subscribed at 15:42:29 UTC+2, 0 delivered today
        let sub_time = local_utc2_ts(0, 15, 42, 29);
        let today_end = local_utc2_ts(0, 23, 0, 0);
        let tomorrow_start = local_utc2_ts(1, 9, 0, 0);
        let tomorrow_end = local_utc2_ts(1, 23, 0, 0);

        for jitter in [0.0, 1.0] {
            let mut cursor = sub_time;
            let mut times = Vec::new();
            for delivered in 0..3 {
                let next = compute_next_delivery_at_with_sample(
                    cursor, 3, delivered, 2, 23, 9, jitter,
                );
                assert!(
                    next > cursor && next < today_end,
                    "Delivery #{} with jitter {} should land today before 23:00 (cursor={}, next={})",
                    delivered + 1,
                    jitter,
                    cursor,
                    next
                );
                assert!(!is_timestamp_in_quiet_hours(next, 2, 23, 9));
                times.push(next);
                cursor = next;
            }
            assert!(times.windows(2).all(|w| w[0] < w[1]));

            let fourth = compute_next_delivery_at_with_sample(cursor, 3, 3, 2, 23, 9, jitter);
            assert!(
                fourth >= tomorrow_start && fourth < tomorrow_end,
                "4th delivery should roll to tomorrow [09:00, 23:00), got {}",
                fourth
            );
        }
    }

    #[test]
    fn full_day_simulation_delivers_exact_quota_within_active_window() {
        let day1_start = local_utc2_ts(0, 9, 0, 0);
        let day1_end = local_utc2_ts(0, 23, 0, 0);
        let day2_start = local_utc2_ts(1, 9, 0, 0);
        let day2_end = local_utc2_ts(1, 23, 0, 0);
        let night_start = local_utc2_ts(0, 2, 0, 0);

        for rate in [1u8, 3, 5] {
            for jitter in [0.0, 0.5, 1.0] {
                let mut cursor = night_start;
                for delivered in 0..(rate as usize) {
                    let next = compute_next_delivery_at_with_sample(
                        cursor, rate, delivered, 2, 23, 9, jitter,
                    );
                    assert!(
                        next >= day1_start && next < day1_end,
                        "rate={}, jitter={}, delivered={}: expected in Day1 active window",
                        rate,
                        jitter,
                        delivered
                    );
                    assert!(!is_timestamp_in_quiet_hours(next, 2, 23, 9));
                    assert!(next > cursor);
                    cursor = next;
                }

                let next_day = compute_next_delivery_at_with_sample(
                    cursor,
                    rate,
                    rate as usize,
                    2,
                    23,
                    9,
                    jitter,
                );
                assert!(
                    next_day >= day2_start && next_day < day2_end,
                    "rate={}, jitter={}: expected rollover to Day2 active window",
                    rate,
                    jitter
                );
            }
        }
    }

    #[test]
    fn late_evening_subscription_avoids_spam_and_rolls_remainder_to_tomorrow() {
        let today_eff_end = local_utc2_ts(0, 22, 55, 0);
        let tomorrow_start = local_utc2_ts(1, 9, 0, 0);
        let tomorrow_end = local_utc2_ts(1, 23, 0, 0);

        // Subscribed at 22:40 UTC+2 (15m before 22:55 cutoff) -> fits 1 meme safely, then rolls over
        let at_2240 = local_utc2_ts(0, 22, 40, 0);
        let first = compute_next_delivery_at_with_sample(at_2240, 3, 0, 2, 23, 9, 0.5);
        assert!(first >= at_2240 + MIN_LEAD_SECS && first <= today_eff_end);

        // After delivery at ~22:47 (< 15m left before 22:55), remaining quota rolls to tomorrow
        let second = compute_next_delivery_at_with_sample(first, 3, 1, 2, 23, 9, 0.5);
        assert!(second >= tomorrow_start && second < tomorrow_end);

        // Subscribed at 22:54 UTC+2 (< 120s left before 22:55) -> goes straight to tomorrow
        let at_2254 = local_utc2_ts(0, 22, 54, 0);
        let direct_tomorrow = compute_next_delivery_at_with_sample(at_2254, 3, 0, 2, 23, 9, 0.5);
        assert!(direct_tomorrow >= tomorrow_start && direct_tomorrow < tomorrow_end);
    }

    #[test]
    fn reconcile_repairs_stale_next_day_schedule_and_is_idempotent() {
        let now_1555 = local_utc2_ts(0, 15, 55, 0);

        // Kate R: 1/day, 0 delivered today, stuck at tomorrow 09:35:12
        let kate = DripSubscription {
            user_id: 321_884_251,
            chat_id: 321_884_251,
            memes_per_day: 1,
            tz_offset_hours: 2,
            quiet_start_hour: 23,
            quiet_end_hour: 9,
            next_delivery_at: local_utc2_ts(1, 9, 35, 12),
            is_active: true,
        };
        assert!(needs_reconcile(&kate, 0, now_1555));

        let repaired = DripSubscription {
            next_delivery_at: compute_next_delivery_at_with_sample(
                now_1555, 1, 0, 2, 23, 9, 0.5,
            ),
            ..kate
        };
        assert!(!needs_reconcile(&repaired, 0, now_1555));

        // Mykhailo: 5/day, 4 delivered today, scheduled at today 17:49:04 -> keep untouched
        let mykhailo = DripSubscription {
            user_id: 422_124_421,
            chat_id: 422_124_421,
            memes_per_day: 5,
            tz_offset_hours: 2,
            quiet_start_hour: 23,
            quiet_end_hour: 9,
            next_delivery_at: local_utc2_ts(0, 17, 49, 4),
            is_active: true,
        };
        assert!(!needs_reconcile(&mykhailo, 4, now_1555));
    }
}
