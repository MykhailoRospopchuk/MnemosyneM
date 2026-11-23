use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

use crate::database::enums::vlm_error_enum::VlmError;

const DECISIONS_API_URL: &str = "https://generativelanguage.googleapis.com/v1beta/decisions";
const DECISIONS_MODEL: &str = "gemini-decision-1";
const DAILY_COOLDOWN_BUFFER_SECS: i64 = 300;
const RPM_WAIT_CAP_SECS: u64 = 65;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ModelTier {
    Tier1 = 1,
    Tier2 = 2,
}

#[derive(Debug, Clone, Copy)]
pub struct ModelSpec {
    pub id: &'static str,
    pub tier: ModelTier,
    pub rpm_limit: u32,
    pub rpd_limit: u32,
}

pub const MODEL_SPECS: &[ModelSpec] = &[
    ModelSpec {
        id: "gemini-3.5-flash-lite",
        tier: ModelTier::Tier1,
        rpm_limit: 15,
        rpd_limit: 500,
    },
    ModelSpec {
        id: "gemini-3.1-flash-lite",
        tier: ModelTier::Tier1,
        rpm_limit: 15,
        rpd_limit: 500,
    },
    ModelSpec {
        id: "gemini-3-flash-preview",
        tier: ModelTier::Tier2,
        rpm_limit: 5,
        rpd_limit: 20,
    },
    ModelSpec {
        id: "gemini-3.6-flash",
        tier: ModelTier::Tier2,
        rpm_limit: 5,
        rpd_limit: 20,
    },
    ModelSpec {
        id: "gemini-3.8-flash",
        tier: ModelTier::Tier2,
        rpm_limit: 5,
        rpd_limit: 20,
    },
    ModelSpec {
        id: "gemini-4-hydrogen",
        tier: ModelTier::Tier2,
        rpm_limit: 5,
        rpd_limit: 20,
    },
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedModelQuota {
    pub model_id: String,
    pub pt_day_key: String,
    pub used_today: u32,
    pub cooldown_until: i64,
    pub last_reason: String,
}

#[derive(Debug, Clone)]
struct ModelQuotaState {
    pt_day_key: String,
    used_today: u32,
    recent_calls: VecDeque<Instant>,
    cooldown_until_unix: i64,
    last_reason: String,
    dirty: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelQuotaSnapshot {
    pub id: &'static str,
    pub tier: ModelTier,
    pub used_today: u32,
    pub rpd_limit: u32,
    pub rpm_used: usize,
    pub rpm_limit: u32,
    pub cooldown_remaining_secs: i64,
    pub last_reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VlmQuotaSummary {
    pub pt_day_key: String,
    pub tier1_used: u32,
    pub tier1_limit: u32,
    pub tier2_used: u32,
    pub tier2_limit: u32,
    pub models: Vec<ModelQuotaSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeminiFailureKind {
    DailyQuotaExhausted,
    MinuteRateLimited(Duration),
    ModelUnavailable,
    Overloaded,
    FatalPayload(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum PreSafetyDecision {
    Allow { safety_score: f32 },
    Reject { reason: String, safety_score: f32 },
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemeAnalysis {
    #[serde(default, deserialize_with = "deserialize_flexible_bool")]
    pub is_nsfw: bool,
    #[serde(alias = "text", alias = "extracted_text", default)]
    pub ocr_text: String,
    #[serde(alias = "description_ukrainian", alias = "uk", default)]
    pub uk_description: String,
    #[serde(
        alias = "description_english",
        alias = "description_en",
        alias = "en",
        default
    )]
    pub en_description: String,
}

fn deserialize_flexible_bool<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Value::deserialize(deserializer)? {
        Value::Bool(b) => Ok(b),
        Value::Number(n) => Ok(n.as_i64().unwrap_or(0) != 0),
        Value::String(s) => match s.trim().to_lowercase().as_str() {
            "true" | "1" | "yes" => Ok(true),
            "false" | "0" | "no" | "" => Ok(false),
            other => Err(serde::de::Error::custom(format!(
                "Invalid boolean string: {}",
                other
            ))),
        },
        Value::Null => Ok(false),
        other => Err(serde::de::Error::custom(format!(
            "Expected bool, number, or string, got: {}",
            other
        ))),
    }
}

const VLM_PROMPT: &str = r#"
You are a meme indexing engine.

SAFETY RULES:
1. ART, DRAWINGS & SATIRE ARE ALWAYS SAFE:
   Medieval illuminations, historical paintings, frescoes, statues, cartoons, comics, sketches, caricatures, and drawn art are CULTURAL HERITAGE, ART & HUMOR.
   Set "is_nsfw": false for ALL drawings, medieval art, paintings, and caricatures regardless of any anatomical humor.
2. REAL-LIFE PHOTOGRAPHIC NUDITY ONLY:
   Set "is_nsfw": true ONLY for REAL-LIFE PHOTOGRAPHS depicting explicit human genitalia or direct real-life sexual acts.
3. Erotic jokes, wordplay, double entendres, underwear, and suggestive memes are SAFE ("is_nsfw": false).

TASK:
Return a single JSON object (no Markdown fences, no extra prose) with these exact fields:
- "is_nsfw": boolean.
- "ocr_text": exact verbatim text written inside the meme image ONLY ("" if none).
  * STRICT: Plain text only. Do NOT include Markdown formatting (*, **, _), bullet points, or layout/position labels such as "Top text:", "Bottom text:", "Left:", "Right:", "Caption:", "Panel 1:".
- "uk_description": concise Ukrainian search summary (25-45 words total).
  * STRICT: Do NOT use framing boilerplate ("На зображенні...", "На фото...", "Мем складається з...", "Скріншот...", "Ключові слова:"). No Markdown.
  * Write in 3 compact parts separated by spaces/commas:
    1) Scene, recognizable template name (if any), characters, and facial expression/action (<= 15 words).
    2) Core joke punchline and the real-life situation when someone sends this meme (<= 18 words).
    3) 6-10 dense Ukrainian search keywords, slang, and synonyms separated by commas.
- "en_description": concise English search summary (25-40 words total).
  * STRICT: Do NOT use framing boilerplate ("The image shows...", "This meme consists of...", "A screenshot of...", "Keywords:"). No Markdown.
  * Include: meme template name (if known), 1 short sentence describing the scene and punchline, and 5-8 English search keywords separated by commas.
"#;

fn unix_now_i64() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = i64::from(y) - if m <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let m = u64::from(m);
    let d = u64::from(d);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + (doe as i64) - 719_468
}

fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = (y + if m <= 2 { 1 } else { 0 }) as i32;
    (y, m, d)
}

fn nth_sunday_of_month(year: i32, month: u32, nth: u32) -> i64 {
    let first_day = days_from_civil(year, month, 1);
    let weekday = (first_day + 4).rem_euclid(7); // 0 = Sunday
    let to_first_sun = (7 - weekday).rem_euclid(7);
    first_day + to_first_sun + 7 * (i64::from(nth) - 1)
}

fn pt_utc_offset_secs(utc_unix: i64) -> i64 {
    let utc_days = utc_unix.div_euclid(86_400);
    let (year, _, _) = civil_from_days(utc_days);
    // US DST: 2nd Sunday in March at 02:00 PST (10:00 UTC) .. 1st Sunday in November at 02:00 PDT (09:00 UTC)
    let dst_start_utc = nth_sunday_of_month(year, 3, 2) * 86_400 + 10 * 3600;
    let dst_end_utc = nth_sunday_of_month(year, 11, 1) * 86_400 + 9 * 3600;
    if utc_unix >= dst_start_utc && utc_unix < dst_end_utc {
        -7 * 3600
    } else {
        -8 * 3600
    }
}

pub fn pt_day_key_and_next_midnight(now_unix: i64) -> (String, i64) {
    let offset = pt_utc_offset_secs(now_unix);
    let pt_local = now_unix + offset;
    let pt_days = pt_local.div_euclid(86_400);
    let (py, pm, pd) = civil_from_days(pt_days);
    let key = format!("{:04}-{:02}-{:02}", py, pm, pd);

    let approx_midnight = (pt_days + 1) * 86_400 - offset;
    let next_offset = pt_utc_offset_secs(approx_midnight);
    let next_midnight_utc = (pt_days + 1) * 86_400 - next_offset;
    (key, next_midnight_utc)
}

fn parse_retry_delay_secs(raw: &str) -> Option<u64> {
    let s = raw.trim();
    if let Some(ms_str) = s.strip_suffix("ms") {
        let ms: f64 = ms_str.trim().parse().ok()?;
        let secs = (ms / 1000.0).ceil().max(1.0) as u64;
        return Some(secs.clamp(5, 90));
    }
    if let Some(sec_str) = s.strip_suffix('s') {
        let secs: f64 = sec_str.trim().parse().ok()?;
        return Some((secs.ceil() as u64).clamp(5, 90));
    }
    None
}

pub fn parse_gemini_error(status: StatusCode, body: &Value) -> GeminiFailureKind {
    let err_msg = body["error"]["message"]
        .as_str()
        .unwrap_or("Unknown Gemini API error")
        .to_string();

    match status {
        StatusCode::BAD_REQUEST => GeminiFailureKind::FatalPayload(err_msg),
        StatusCode::NOT_FOUND | StatusCode::FORBIDDEN => GeminiFailureKind::ModelUnavailable,
        StatusCode::TOO_MANY_REQUESTS => {
            let mut is_daily = false;
            let mut retry_delay: Option<u64> = None;

            if let Some(details) = body["error"]["details"].as_array() {
                for detail in details {
                    let kind = detail["@type"].as_str().unwrap_or("");
                    if kind.ends_with("QuotaFailure")
                        && let Some(violations) = detail["violations"].as_array()
                    {
                        for v in violations {
                            let quota_id = v["quotaId"].as_str().unwrap_or("").to_lowercase();
                            let quota_metric =
                                v["quotaMetric"].as_str().unwrap_or("").to_lowercase();
                            if quota_id.contains("perday")
                                || quota_id.contains("daily")
                                || quota_metric.contains("per_day")
                                || quota_metric.contains("daily")
                            {
                                is_daily = true;
                            }
                        }
                    } else if kind.ends_with("RetryInfo")
                        && let Some(raw_delay) = detail["retryDelay"].as_str()
                    {
                        retry_delay = parse_retry_delay_secs(raw_delay);
                    }
                }
            }

            if is_daily {
                return GeminiFailureKind::DailyQuotaExhausted;
            }

            if retry_delay.is_none() {
                let lower = err_msg.to_lowercase();
                if lower.contains("per day") || lower.contains("daily limit") {
                    return GeminiFailureKind::DailyQuotaExhausted;
                }
                if let Some(pos) = lower.find("retry in ") {
                    let after = &lower[pos + "retry in ".len()..];
                    let token: String = after
                        .chars()
                        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == 's')
                        .collect();
                    retry_delay = parse_retry_delay_secs(&token);
                }
            }

            GeminiFailureKind::MinuteRateLimited(Duration::from_secs(retry_delay.unwrap_or(60)))
        }
        _ if status.is_server_error() => GeminiFailureKind::Overloaded,
        _ => GeminiFailureKind::Overloaded,
    }
}

struct SelectionResult {
    ordered: Vec<(usize, &'static ModelSpec)>,
    wait_for_rpm: Option<Duration>,
    all_daily_exhausted: bool,
}

fn rollover_and_prune(state: &mut ModelQuotaState, current_pt_day: &str, now_instant: Instant) {
    if state.pt_day_key != current_pt_day {
        state.pt_day_key = current_pt_day.to_string();
        state.used_today = 0;
        if state.cooldown_until_unix > 0 {
            state.cooldown_until_unix = 0;
            state.last_reason.clear();
        }
        state.dirty = true;
    }

    while let Some(&front) = state.recent_calls.front() {
        if now_instant.saturating_duration_since(front) >= Duration::from_secs(60) {
            state.recent_calls.pop_front();
        } else {
            break;
        }
    }
}

fn select_candidates_inner(
    states: &mut HashMap<&'static str, ModelQuotaState>,
    now_unix: i64,
    now_instant: Instant,
) -> SelectionResult {
    let (pt_day, _) = pt_day_key_and_next_midnight(now_unix);
    let mut eligible: Vec<(usize, &'static ModelSpec, bool, u64, usize)> = Vec::new();
    let mut min_rpm_wait_secs: Option<u64> = None;
    let mut daily_blocked_count = 0usize;

    for (idx, spec) in MODEL_SPECS.iter().enumerate() {
        let st = states.entry(spec.id).or_insert_with(|| ModelQuotaState {
            pt_day_key: pt_day.clone(),
            used_today: 0,
            recent_calls: VecDeque::new(),
            cooldown_until_unix: 0,
            last_reason: String::new(),
            dirty: false,
        });

        rollover_and_prune(st, &pt_day, now_instant);

        let cooldown_left = (st.cooldown_until_unix - now_unix).max(0) as u64;
        let rpm_used = st.recent_calls.len();

        if cooldown_left == 0 && rpm_used < spec.rpm_limit as usize {
            let over_soft_rpd = st.used_today >= spec.rpd_limit;
            let rem_ratio_bp =
                (u64::from(spec.rpd_limit.saturating_sub(st.used_today)) * 10_000) / u64::from(spec.rpd_limit.max(1));
            eligible.push((idx, spec, over_soft_rpd, rem_ratio_bp, rpm_used));
        } else if cooldown_left > 0 && cooldown_left <= 90 {
            min_rpm_wait_secs = Some(min_rpm_wait_secs.map_or(cooldown_left, |m| m.min(cooldown_left)));
        } else if cooldown_left == 0 && rpm_used >= spec.rpm_limit as usize {
            let secs = st
                .recent_calls
                .front()
                .map(|&oldest| {
                    60u64
                        .saturating_sub(now_instant.saturating_duration_since(oldest).as_secs())
                        .max(1)
                })
                .unwrap_or(5);
            min_rpm_wait_secs = Some(min_rpm_wait_secs.map_or(secs, |m| m.min(secs)));
        } else if cooldown_left > 90 {
            daily_blocked_count += 1;
        }
    }

    eligible.sort_by(|a, b| {
        a.2.cmp(&b.2)
            .then_with(|| a.1.tier.cmp(&b.1.tier))
            .then_with(|| b.3.cmp(&a.3))
            .then_with(|| a.4.cmp(&b.4))
            .then_with(|| a.0.cmp(&b.0))
    });

    let ordered = eligible
        .into_iter()
        .map(|(idx, spec, _, _, _)| (idx, spec))
        .collect();

    let wait_for_rpm = min_rpm_wait_secs.map(|s| Duration::from_secs(s.clamp(1, RPM_WAIT_CAP_SECS)));
    let all_daily_exhausted = daily_blocked_count == MODEL_SPECS.len();

    SelectionResult {
        ordered,
        wait_for_rpm,
        all_daily_exhausted,
    }
}

pub fn evaluate_safety_response(resp: &Value) -> PreSafetyDecision {
    let answers = &resp["answers"];
    if !answers.is_object() {
        return PreSafetyDecision::Skipped;
    }

    let nudity = answers["real_photo_explicit_nudity"]["noul"]
        .as_f64()
        .unwrap_or(0.0) as f32;
    let art = answers["is_art_comic_or_meme"]["noul"]
        .as_f64()
        .unwrap_or(0.0) as f32;
    let gore = answers["extreme_gore_or_shock"]["noul"]
        .as_f64()
        .unwrap_or(0.0) as f32;

    if nudity >= 0.85 && art < 0.50 {
        return PreSafetyDecision::Reject {
            reason: "Explicit real-life NSFW/nudity detected (pre-ingestion gate)".to_string(),
            safety_score: (1.0 - nudity).clamp(0.0, 1.0),
        };
    }

    if gore >= 0.90 && art < 0.40 {
        return PreSafetyDecision::Reject {
            reason: "Graphic gore/shock content detected (pre-ingestion gate)".to_string(),
            safety_score: (1.0 - gore).clamp(0.0, 1.0),
        };
    }

    let safety_score = (1.0 - nudity.max(gore)).clamp(0.0, 1.0);
    PreSafetyDecision::Allow { safety_score }
}

pub struct VlmRouter {
    client: reqwest::Client,
    states: Mutex<HashMap<&'static str, ModelQuotaState>>,
}

impl VlmRouter {
    pub fn new(initial_quotas: Vec<PersistedModelQuota>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("Failed to build shared reqwest Client for VlmRouter");

        let (current_pt_day, _) = pt_day_key_and_next_midnight(unix_now_i64());
        let now_unix = unix_now_i64();
        let mut persisted_map: HashMap<String, PersistedModelQuota> = initial_quotas
            .into_iter()
            .map(|q| (q.model_id.clone(), q))
            .collect();

        let mut states = HashMap::with_capacity(MODEL_SPECS.len());
        for spec in MODEL_SPECS {
            let state = if let Some(saved) = persisted_map.remove(spec.id) {
                if saved.pt_day_key == current_pt_day {
                    let keep_cd = saved.cooldown_until > now_unix;
                    ModelQuotaState {
                        pt_day_key: saved.pt_day_key,
                        used_today: saved.used_today,
                        recent_calls: VecDeque::new(),
                        cooldown_until_unix: if keep_cd { saved.cooldown_until } else { 0 },
                        last_reason: if keep_cd { saved.last_reason } else { String::new() },
                        dirty: false,
                    }
                } else {
                    ModelQuotaState {
                        pt_day_key: current_pt_day.clone(),
                        used_today: 0,
                        recent_calls: VecDeque::new(),
                        cooldown_until_unix: 0,
                        last_reason: String::new(),
                        dirty: true,
                    }
                }
            } else {
                ModelQuotaState {
                    pt_day_key: current_pt_day.clone(),
                    used_today: 0,
                    recent_calls: VecDeque::new(),
                    cooldown_until_unix: 0,
                    last_reason: String::new(),
                    dirty: false,
                }
            };
            states.insert(spec.id, state);
        }

        Self {
            client,
            states: Mutex::new(states),
        }
    }

    pub async fn check_pre_ingestion_safety(
        &self,
        api_key: &str,
        image_bytes: &[u8],
        mime_type: &str,
    ) -> PreSafetyDecision {
        if image_bytes.is_empty() || !mime_type.starts_with("image/") {
            return PreSafetyDecision::Skipped;
        }

        let base64_image = STANDARD.encode(image_bytes);
        let body = json!({
            "model": DECISIONS_MODEL,
            "input": [{
                "type": "image",
                "mime_type": mime_type,
                "data": base64_image,
            }],
            "questions": {
                "real_photo_explicit_nudity": {
                    "type": "noul",
                    "instructions": "Is this a real-life photograph showing explicit human genitalia or direct real-life sexual acts (NOT drawings, cartoons, anime, medieval art, or statues)?"
                },
                "is_art_comic_or_meme": {
                    "type": "noul",
                    "instructions": "Is this a drawing, comic, cartoon, anime, historical painting, meme template, or text-based joke rather than an explicit real-life photo?"
                },
                "extreme_gore_or_shock": {
                    "type": "noul",
                    "instructions": "Does this show extreme real-life graphic gore, severe dismemberment, or real-life shock violence (NOT video games, cartoons, or slapstick memes)?"
                }
            }
        });

        let res = match self
            .client
            .post(DECISIONS_API_URL)
            .header("x-goog-api-key", api_key)
            .json(&body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "Pre-ingestion safety gate request failed; falling back to VLM");
                return PreSafetyDecision::Skipped;
            }
        };

        let status = res.status();
        let payload: Value = match res.json().await {
            Ok(v) => v,
            Err(e) => {
                warn!(status = %status, error = %e, "Failed to parse pre-ingestion safety gate response");
                return PreSafetyDecision::Skipped;
            }
        };

        if !status.is_success() {
            let err_msg = payload["error"]["message"]
                .as_str()
                .unwrap_or("Unknown Decision API error");
            warn!(status = %status, error = err_msg, "Pre-ingestion safety gate returned error");
            return PreSafetyDecision::Skipped;
        }

        let decision = evaluate_safety_response(&payload);
        debug!(decision = ?decision, "Pre-ingestion safety gate evaluated");
        decision
    }

    pub async fn describe_meme(
        &self,
        image_bytes: &[u8],
        api_key: &str,
        mime_type: &str,
    ) -> Result<MemeAnalysis, VlmError> {
        let base64_image = STANDARD.encode(image_bytes);
        let size_kb = image_bytes.len() / 1024;

        let body = json!({
            "contents": [{
                "parts": [
                    { "text": VLM_PROMPT },
                    {
                        "inline_data": {
                            "mime_type": mime_type,
                            "data": base64_image
                        }
                    }
                ]
            }],
            "generationConfig": {
                "responseMimeType": "application/json"
            },
            "safetySettings": [
                {
                    "category": "HARM_CATEGORY_SEXUALLY_EXPLICIT",
                    "threshold": "BLOCK_ONLY_HIGH"
                },
                {
                    "category": "HARM_CATEGORY_DANGEROUS_CONTENT",
                    "threshold": "BLOCK_MEDIUM_AND_ABOVE"
                }
            ]
        });

        let mut selection = {
            let mut states = self.states.lock().await;
            select_candidates_inner(&mut states, unix_now_i64(), Instant::now())
        };

        if selection.ordered.is_empty() {
            if let Some(wait_dur) = selection.wait_for_rpm {
                warn!(
                    wait_secs = wait_dur.as_secs(),
                    "All VLM models temporarily RPM-limited; waiting before retry"
                );
                tokio::time::sleep(wait_dur).await;
                let mut states = self.states.lock().await;
                selection = select_candidates_inner(&mut states, unix_now_i64(), Instant::now());
            } else if selection.all_daily_exhausted {
                return Err(VlmError::DailyQuotaExhausted(
                    "All VLM models exhausted daily RPD quota".to_string(),
                ));
            }
        }

        if selection.ordered.is_empty() {
            return Err(if selection.all_daily_exhausted {
                VlmError::DailyQuotaExhausted(
                    "All VLM models exhausted daily RPD quota".to_string(),
                )
            } else {
                VlmError::Transient("No eligible VLM models available right now".to_string())
            });
        }

        let mut last_error = String::new();

        for (_, spec) in selection.ordered {
            let used_before = {
                let mut states = self.states.lock().await;
                if let Some(st) = states.get_mut(spec.id) {
                    st.recent_calls.push_back(Instant::now());
                    st.used_today
                } else {
                    0
                }
            };

            debug!(
                model = spec.id,
                tier = ?spec.tier,
                used_today = used_before,
                rpd_limit = spec.rpd_limit,
                mime_type,
                size_kb,
                "Sending media to Gemini VLM"
            );

            let url = format!(
                "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent",
                spec.id
            );

            let res = match self
                .client
                .post(&url)
                .header("x-goog-api-key", api_key)
                .json(&body)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    last_error = e.to_string();
                    let now_unix = unix_now_i64();
                    let mut states = self.states.lock().await;
                    if let Some(st) = states.get_mut(spec.id) {
                        st.cooldown_until_unix = now_unix + 20;
                        st.last_reason = "network_error".to_string();
                    }
                    warn!(model = spec.id, error = %last_error, "VLM network error; failing over");
                    continue;
                }
            };

            let status = res.status();
            debug!(model = spec.id, status = %status, "Received Gemini HTTP response");

            let json_response: Value = match res.json().await {
                Ok(v) => v,
                Err(e) => {
                    last_error = format!("HTTP {} invalid JSON: {}", status, e);
                    let now_unix = unix_now_i64();
                    let mut states = self.states.lock().await;
                    if let Some(st) = states.get_mut(spec.id) {
                        st.cooldown_until_unix = now_unix + 20;
                        st.last_reason = "bad_json".to_string();
                    }
                    continue;
                }
            };

            if !status.is_success() {
                let failure = parse_gemini_error(status, &json_response);
                let err_msg = json_response["error"]["message"]
                    .as_str()
                    .unwrap_or("Unknown Gemini API error")
                    .to_string();
                let now_unix = unix_now_i64();
                let (_, next_pt_midnight) = pt_day_key_and_next_midnight(now_unix);

                let should_abort = {
                    let mut states = self.states.lock().await;
                    if let Some(st) = states.get_mut(spec.id) {
                        match &failure {
                            GeminiFailureKind::DailyQuotaExhausted => {
                                st.cooldown_until_unix =
                                    next_pt_midnight + DAILY_COOLDOWN_BUFFER_SECS;
                                st.used_today = st.used_today.max(spec.rpd_limit);
                                st.last_reason = "daily_quota".to_string();
                                st.dirty = true;
                                warn!(
                                    model = spec.id,
                                    cooldown_until = st.cooldown_until_unix,
                                    "Gemini model exhausted daily RPD quota; cooled down until PT midnight"
                                );
                                false
                            }
                            GeminiFailureKind::MinuteRateLimited(delay) => {
                                st.cooldown_until_unix = now_unix + delay.as_secs() as i64;
                                st.last_reason = "rpm_limit".to_string();
                                st.dirty = true;
                                warn!(
                                    model = spec.id,
                                    delay_secs = delay.as_secs(),
                                    "Gemini model hit RPM/TPM limit; failing over"
                                );
                                false
                            }
                            GeminiFailureKind::ModelUnavailable => {
                                st.cooldown_until_unix = now_unix + 86_400;
                                st.last_reason = "unavailable".to_string();
                                st.dirty = true;
                                warn!(
                                    model = spec.id,
                                    status = %status,
                                    "Gemini model unavailable (403/404); disabled for 24h"
                                );
                                false
                            }
                            GeminiFailureKind::Overloaded => {
                                st.cooldown_until_unix = now_unix + 20;
                                st.last_reason = "overloaded".to_string();
                                st.dirty = true;
                                warn!(
                                    model = spec.id,
                                    status = %status,
                                    "Gemini model overloaded/server error; failing over"
                                );
                                false
                            }
                            GeminiFailureKind::FatalPayload(msg) => {
                                error!(model = spec.id, status = %status, error = %msg, "Gemini rejected request payload (400)");
                                true
                            }
                        }
                    } else {
                        matches!(failure, GeminiFailureKind::FatalPayload(_))
                    }
                };

                if should_abort {
                    return Err(VlmError::Transient(format!(
                        "Google API {}: {}",
                        status, err_msg
                    )));
                }

                last_error = format!("Google API {} ({}): {}", status, spec.id, err_msg);
                continue;
            }

            let used_after = {
                let mut states = self.states.lock().await;
                if let Some(st) = states.get_mut(spec.id) {
                    st.used_today += 1;
                    st.last_reason.clear();
                    st.dirty = true;
                    st.used_today
                } else {
                    used_before + 1
                }
            };

            if let Some(candidate) = json_response["candidates"].get(0)
                && candidate["finishReason"].as_str() == Some("SAFETY")
            {
                warn!(model = spec.id, "Gemini safety filter blocked media");
                debug!(model = spec.id, response = %json_response, "Full Gemini safety response");
                return Err(VlmError::InappropriateContent(
                    "Inappropriate content detected (Gemini Safety Filter Explicit/NSFW)"
                        .to_string(),
                ));
            }

            let raw_text = json_response["candidates"][0]["content"]["parts"][0]["text"]
                .as_str()
                .ok_or_else(|| VlmError::Transient("Missing text in Gemini response".to_string()))?;

            let analysis = parse_meme_analysis(raw_text)?;
            if analysis.is_nsfw {
                warn!(
                    model = spec.id,
                    "Explicit/NSFW content detected in VLM analysis"
                );
                debug!(model = spec.id, response = %json_response, "Full Gemini NSFW response");
                return Err(VlmError::InappropriateContent(
                    "NSFW explicit content detected. Inappropriate content detected: NSFW/Adult content is not allowed!".to_string(),
                ));
            }

            info!(
                model = spec.id,
                tier = ?spec.tier,
                used_today = used_after,
                rpd_limit = spec.rpd_limit,
                mime_type,
                "Gemini VLM analysis succeeded"
            );
            return Ok(analysis);
        }

        let all_daily_now = {
            let mut states = self.states.lock().await;
            select_candidates_inner(&mut states, unix_now_i64(), Instant::now()).all_daily_exhausted
        };

        if all_daily_now {
            Err(VlmError::DailyQuotaExhausted(format!(
                "All VLM models exhausted daily quota. Last error: {}",
                last_error
            )))
        } else {
            Err(VlmError::Transient(format!(
                "All VLM models failed. Last error: {}",
                last_error
            )))
        }
    }

    pub async fn drain_dirty_quotas(&self) -> Vec<PersistedModelQuota> {
        let mut states = self.states.lock().await;
        let mut out = Vec::new();
        for (&model_id, st) in states.iter_mut() {
            if st.dirty {
                out.push(PersistedModelQuota {
                    model_id: model_id.to_string(),
                    pt_day_key: st.pt_day_key.clone(),
                    used_today: st.used_today,
                    cooldown_until: st.cooldown_until_unix,
                    last_reason: st.last_reason.clone(),
                });
                st.dirty = false;
            }
        }
        out
    }

    pub async fn quota_snapshot(&self) -> VlmQuotaSummary {
        let now_unix = unix_now_i64();
        let now_instant = Instant::now();
        let (pt_day, _) = pt_day_key_and_next_midnight(now_unix);

        let mut states = self.states.lock().await;
        let mut models = Vec::with_capacity(MODEL_SPECS.len());
        let mut tier1_used = 0u32;
        let mut tier1_limit = 0u32;
        let mut tier2_used = 0u32;
        let mut tier2_limit = 0u32;

        for spec in MODEL_SPECS {
            let st = states.entry(spec.id).or_insert_with(|| ModelQuotaState {
                pt_day_key: pt_day.clone(),
                used_today: 0,
                recent_calls: VecDeque::new(),
                cooldown_until_unix: 0,
                last_reason: String::new(),
                dirty: false,
            });
            rollover_and_prune(st, &pt_day, now_instant);

            match spec.tier {
                ModelTier::Tier1 => {
                    tier1_used += st.used_today;
                    tier1_limit += spec.rpd_limit;
                }
                ModelTier::Tier2 => {
                    tier2_used += st.used_today;
                    tier2_limit += spec.rpd_limit;
                }
            }

            models.push(ModelQuotaSnapshot {
                id: spec.id,
                tier: spec.tier,
                used_today: st.used_today,
                rpd_limit: spec.rpd_limit,
                rpm_used: st.recent_calls.len(),
                rpm_limit: spec.rpm_limit,
                cooldown_remaining_secs: (st.cooldown_until_unix - now_unix).max(0),
                last_reason: st.last_reason.clone(),
            });
        }

        VlmQuotaSummary {
            pt_day_key: pt_day,
            tier1_used,
            tier1_limit,
            tier2_used,
            tier2_limit,
            models,
        }
    }

    pub async fn recommended_worker_sleep(&self) -> Duration {
        let now_unix = unix_now_i64();
        let now_instant = Instant::now();
        let (pt_day, _) = pt_day_key_and_next_midnight(now_unix);

        let mut states = self.states.lock().await;
        let mut active_tier1 = 0usize;

        for spec in MODEL_SPECS.iter().filter(|s| s.tier == ModelTier::Tier1) {
            if let Some(st) = states.get_mut(spec.id) {
                rollover_and_prune(st, &pt_day, now_instant);
                if st.cooldown_until_unix <= now_unix && st.used_today < spec.rpd_limit {
                    active_tier1 += 1;
                }
            } else {
                active_tier1 += 1;
            }
        }

        if active_tier1 >= 2 {
            Duration::from_secs(2)
        } else if active_tier1 == 1 {
            Duration::from_secs(4)
        } else {
            Duration::from_secs(8)
        }
    }
}

fn unwrap_vlm_value(value: Value) -> Result<Value, String> {
    match value {
        Value::Array(mut arr) => {
            if arr.is_empty() {
                return Err("Empty JSON array from VLM".to_string());
            }
            unwrap_vlm_value(arr.remove(0))
        }
        Value::Object(map) => {
            const KNOWN_KEYS: &[&str] = &[
                "is_nsfw",
                "ocr_text",
                "text",
                "extracted_text",
                "uk_description",
                "description_ukrainian",
                "uk",
                "en_description",
                "description_english",
                "description_en",
                "en",
            ];
            if KNOWN_KEYS.iter().any(|k| map.contains_key(*k)) {
                return Ok(Value::Object(map));
            }

            if map.len() == 1
                && map
                    .values()
                    .next()
                    .is_some_and(|v| v.is_object() || v.is_array())
            {
                let (_, inner) = map.into_iter().next().unwrap();
                return unwrap_vlm_value(inner);
            }

            Ok(Value::Object(map))
        }
        other => Err(format!("Unexpected JSON shape from VLM: {}", other)),
    }
}

fn strip_markdown_fences(raw_text: &str) -> &str {
    let trimmed = raw_text.trim();
    let without_prefix = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed);
    without_prefix
        .strip_suffix("```")
        .unwrap_or(without_prefix)
        .trim()
}

fn extract_json_slice(text: &str) -> &str {
    let obj_start = text.find('{');
    let arr_start = text.find('[');

    match (obj_start, arr_start) {
        (Some(o), Some(a)) if a < o => {
            if let Some(end) = text.rfind(']')
                && end >= a
            {
                return &text[a..=end];
            }
        }
        (Some(o), _) => {
            if let Some(end) = text.rfind('}')
                && end >= o
            {
                return &text[o..=end];
            }
        }
        (None, Some(a)) => {
            if let Some(end) = text.rfind(']')
                && end >= a
            {
                return &text[a..=end];
            }
        }
        (None, None) => {}
    }

    text
}

fn strip_layout_prefix(line: &str) -> &str {
    const PREFIXES: &[&str] = &[
        "top text:",
        "bottom text:",
        "left text:",
        "left side:",
        "left:",
        "right text:",
        "right side:",
        "right:",
        "caption:",
        "panel 1:",
        "panel 2:",
        "panel 3:",
        "panel 4:",
    ];

    let mut s = line.trim();
    // Strip markdown bullet/emphasis markers wrapping layout labels like "*Top text:*"
    if let Some(rest) = s.strip_prefix("- ").or_else(|| s.strip_prefix("* ")) {
        s = rest.trim_start();
    }
    let unwrapped = s.trim_start_matches(['*', '_']).trim_start();

    for prefix in PREFIXES {
        if unwrapped
            .get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
        {
            return unwrapped[prefix.len()..]
                .trim_start_matches(['*', '_', ' '])
                .trim();
        }
    }

    s
}

fn clean_ocr_text(raw: &str) -> String {
    raw.lines()
        .map(strip_layout_prefix)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn clean_description(raw: &str) -> String {
    let mut cleaned = raw.trim().to_string();
    for marker in ["Ключові слова:", "ключові слова:", "Keywords:", "keywords:"]
    {
        cleaned = cleaned.replace(marker, "");
    }
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn sanitize_analysis(mut analysis: MemeAnalysis) -> MemeAnalysis {
    analysis.ocr_text = clean_ocr_text(&analysis.ocr_text);
    analysis.uk_description = clean_description(&analysis.uk_description);
    analysis.en_description = clean_description(&analysis.en_description);
    analysis
}

pub fn parse_meme_analysis(raw_text: &str) -> Result<MemeAnalysis, VlmError> {
    let cleaned = strip_markdown_fences(raw_text);
    let json_slice = extract_json_slice(cleaned);

    let value: Value = serde_json::from_str(json_slice)
        .map_err(|e| VlmError::Transient(format!("JSON parse error: {} | Raw: {}", e, raw_text)))?;

    let unwrapped = unwrap_vlm_value(value).map_err(|e| {
        VlmError::Transient(format!("JSON unwrap error: {} | Raw: {}", e, raw_text))
    })?;

    let analysis: MemeAnalysis = serde_json::from_value(unwrapped).map_err(|e| {
        VlmError::Transient(format!("JSON decode error: {} | Raw: {}", e, raw_text))
    })?;

    Ok(sanitize_analysis(analysis))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_standard_fenced_object() {
        let raw = r#"```json
        {
            "is_nsfw": false,
            "ocr_text": "hello",
            "uk_description": "привіт",
            "en_description": "hello meme"
        }
        ```"#;

        let parsed = parse_meme_analysis(raw).unwrap();
        assert!(!parsed.is_nsfw);
        assert_eq!(parsed.ocr_text, "hello");
        assert_eq!(parsed.uk_description, "привіт");
        assert_eq!(parsed.en_description, "hello meme");
    }

    #[test]
    fn parses_array_wrapped_and_nested_wrapper() {
        let raw = r#"Sure, here is the analysis:
        [{"result": {"text": "test", "uk": "тест", "en": "test en", "is_nsfw": "false"}}]"#;

        let parsed = parse_meme_analysis(raw).unwrap();
        assert!(!parsed.is_nsfw);
        assert_eq!(parsed.ocr_text, "test");
        assert_eq!(parsed.uk_description, "тест");
        assert_eq!(parsed.en_description, "test en");
    }

    #[test]
    fn parses_numeric_and_string_booleans() {
        let raw_zero = r#"{"is_nsfw": 0, "ocr_text": "a"}"#;
        let raw_one = r#"{"is_nsfw": 1, "ocr_text": "b"}"#;
        let raw_str_true = r#"{"is_nsfw": "TRUE", "ocr_text": "c"}"#;

        assert!(!parse_meme_analysis(raw_zero).unwrap().is_nsfw);
        assert!(parse_meme_analysis(raw_one).unwrap().is_nsfw);
        assert!(parse_meme_analysis(raw_str_true).unwrap().is_nsfw);
    }

    #[test]
    fn strips_layout_labels_and_keyword_boilerplate_without_breaking_profanity() {
        let raw = r#"{
            "is_nsfw": false,
            "ocr_text": "*Top text:* Коли дедлайн вчора\nBottom text: f*ck this",
            "uk_description": "Кіт у паніці дивиться на монітор. Ключові слова: кіт, дедлайн, паніка",
            "en_description": "Panicked cat staring at monitor. Keywords: cat, deadline, panic"
        }"#;

        let parsed = parse_meme_analysis(raw).unwrap();
        assert_eq!(parsed.ocr_text, "Коли дедлайн вчора\nf*ck this");
        assert_eq!(
            parsed.uk_description,
            "Кіт у паніці дивиться на монітор. кіт, дедлайн, паніка"
        );
        assert_eq!(
            parsed.en_description,
            "Panicked cat staring at monitor. cat, deadline, panic"
        );
    }

    #[test]
    fn pt_day_key_and_next_midnight_handles_pst_pdt_and_dst_transitions() {
        // 2026-01-15 07:30 UTC -> PST (-8h) = 2026-01-14 23:30 PST; next midnight = 2026-01-15 08:00 UTC
        let winter_utc = days_from_civil(2026, 1, 15) * 86_400 + 7 * 3600 + 1800;
        let (winter_key, winter_next) = pt_day_key_and_next_midnight(winter_utc);
        assert_eq!(winter_key, "2026-01-14");
        assert_eq!(winter_next, days_from_civil(2026, 1, 15) * 86_400 + 8 * 3600);

        // 2026-07-15 08:00 UTC -> PDT (-7h) = 2026-07-15 01:00 PDT; next midnight = 2026-07-16 07:00 UTC
        let summer_utc = days_from_civil(2026, 7, 15) * 86_400 + 8 * 3600;
        let (summer_key, summer_next) = pt_day_key_and_next_midnight(summer_utc);
        assert_eq!(summer_key, "2026-07-15");
        assert_eq!(summer_next, days_from_civil(2026, 7, 16) * 86_400 + 7 * 3600);

        // 2026 US DST starts 2nd Sunday of March = March 8 at 10:00 UTC.
        // At 2026-03-08 08:30 UTC (00:30 PST), next PT midnight is 2026-03-09 07:00 UTC (PDT).
        let spring_early = days_from_civil(2026, 3, 8) * 86_400 + 8 * 3600 + 1800;
        let (spring_key, spring_next) = pt_day_key_and_next_midnight(spring_early);
        assert_eq!(spring_key, "2026-03-08");
        assert_eq!(spring_next, days_from_civil(2026, 3, 9) * 86_400 + 7 * 3600);
    }

    #[test]
    fn parse_gemini_error_distinguishes_daily_rpm_unavailable_overloaded_and_fatal() {
        let daily_json = json!({
            "error": {
                "message": "Quota exceeded",
                "details": [{
                    "@type": "type.googleapis.com/google.rpc.QuotaFailure",
                    "violations": [{"quotaId": "GenerateRequestsPerDayPerProjectPerModel-FreeTier"}]
                }]
            }
        });
        assert_eq!(
            parse_gemini_error(StatusCode::TOO_MANY_REQUESTS, &daily_json),
            GeminiFailureKind::DailyQuotaExhausted
        );

        let rpm_json = json!({
            "error": {
                "message": "Too many requests",
                "details": [
                    {
                        "@type": "type.googleapis.com/google.rpc.QuotaFailure",
                        "violations": [{"quotaId": "GenerateRequestsPerMinutePerProjectPerModel-FreeTier"}]
                    },
                    {
                        "@type": "type.googleapis.com/google.rpc.RetryInfo",
                        "retryDelay": "16.98s"
                    }
                ]
            }
        });
        assert_eq!(
            parse_gemini_error(StatusCode::TOO_MANY_REQUESTS, &rpm_json),
            GeminiFailureKind::MinuteRateLimited(Duration::from_secs(17))
        );

        assert_eq!(
            parse_gemini_error(StatusCode::NOT_FOUND, &json!({})),
            GeminiFailureKind::ModelUnavailable
        );
        assert_eq!(
            parse_gemini_error(StatusCode::SERVICE_UNAVAILABLE, &json!({})),
            GeminiFailureKind::Overloaded
        );
        assert!(matches!(
            parse_gemini_error(
                StatusCode::BAD_REQUEST,
                &json!({"error": {"message": "bad image"}})
            ),
            GeminiFailureKind::FatalPayload(_)
        ));
    }

    #[test]
    fn router_balances_tier1_before_tier2_and_rolls_over_at_pt_midnight() {
        let now_unix = days_from_civil(2026, 11, 23) * 86_400 + 18 * 3600;
        let (pt_day, _) = pt_day_key_and_next_midnight(now_unix);
        let now_instant = Instant::now();

        let mut states = HashMap::new();
        states.insert(
            "gemini-3.5-flash-lite",
            ModelQuotaState {
                pt_day_key: pt_day.clone(),
                used_today: 120,
                recent_calls: VecDeque::new(),
                cooldown_until_unix: 0,
                last_reason: String::new(),
                dirty: false,
            },
        );
        states.insert(
            "gemini-3.1-flash-lite",
            ModelQuotaState {
                pt_day_key: pt_day,
                used_today: 40,
                recent_calls: VecDeque::new(),
                cooldown_until_unix: 0,
                last_reason: String::new(),
                dirty: false,
            },
        );

        let sel = select_candidates_inner(&mut states, now_unix, now_instant);
        // Tier-1 with fewer used_today (higher remaining %) comes first, then other Tier-1, then Tier-2
        assert_eq!(sel.ordered[0].1.id, "gemini-3.1-flash-lite");
        assert_eq!(sel.ordered[1].1.id, "gemini-3.5-flash-lite");
        assert_eq!(sel.ordered[2].1.tier, ModelTier::Tier2);

        // Next PT day resets used_today and daily cooldown
        states.get_mut("gemini-3.5-flash-lite").unwrap().cooldown_until_unix = now_unix + 50_000;
        let next_day_unix = now_unix + 86_400;
        let sel_next = select_candidates_inner(&mut states, next_day_unix, now_instant);
        assert_eq!(states["gemini-3.5-flash-lite"].used_today, 0);
        assert_eq!(states["gemini-3.5-flash-lite"].cooldown_until_unix, 0);
        assert_eq!(sel_next.ordered[0].1.tier, ModelTier::Tier1);
    }

    #[test]
    fn evaluate_safety_response_allows_memes_and_art_while_rejecting_real_nsfw_and_gore() {
        let drawn_art = json!({
            "answers": {
                "real_photo_explicit_nudity": { "noul": 0.90 },
                "is_art_comic_or_meme": { "noul": 0.85 },
                "extreme_gore_or_shock": { "noul": 0.05 }
            }
        });
        assert!(matches!(
            evaluate_safety_response(&drawn_art),
            PreSafetyDecision::Allow { .. }
        ));

        let real_nudity = json!({
            "answers": {
                "real_photo_explicit_nudity": { "noul": 0.92 },
                "is_art_comic_or_meme": { "noul": 0.12 },
                "extreme_gore_or_shock": { "noul": 0.04 }
            }
        });
        assert!(matches!(
            evaluate_safety_response(&real_nudity),
            PreSafetyDecision::Reject { .. }
        ));

        let real_gore = json!({
            "answers": {
                "real_photo_explicit_nudity": { "noul": 0.02 },
                "is_art_comic_or_meme": { "noul": 0.10 },
                "extreme_gore_or_shock": { "noul": 0.95 }
            }
        });
        assert!(matches!(
            evaluate_safety_response(&real_gore),
            PreSafetyDecision::Reject { .. }
        ));
    }
}
