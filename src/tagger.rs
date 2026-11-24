use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use std::time::Duration;

use reqwest::StatusCode;
use serde_json::{Map, Value, json};
use tracing::{debug, info, warn};

const DECISIONS_API_URL: &str = "https://generativelanguage.googleapis.com/v1beta/decisions";
const DECISIONS_MODEL: &str = "gemini-decision-1";
const DISCOVERY_MODELS: &[&str] = &[
    "gemini-3.5-flash-lite",
    "gemini-3.1-flash-lite",
    "gemini-3.6-flash",
];

pub const TAG_CONFIDENCE_THRESHOLD: f32 = 0.72;
pub const TAG_FALLBACK_THRESHOLD: f32 = 0.45;
pub const OTHER_TAG_MIN_SCORE: f32 = 0.55;
pub const MAX_TAGS_PER_MEME: usize = 5;
pub const MAX_ACTIVE_TAGS: usize = 40;
pub const OTHER_TAG_SLUG: &str = "інше";
const OTHER_QUESTION_ID: &str = "other";
pub const OTHER_TAG_EMOJI: &str = "📦";
pub const OTHER_TAG_INSTRUCTION: &str = "Does this meme NOT fit any of the other listed categories well, instead belonging to an unlisted topic or miscellaneous theme?";

pub struct TagDef {
    pub slug: &'static str,
    pub emoji: &'static str,
    pub instruction: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DynamicTagDef {
    pub slug: String,
    pub emoji: String,
    pub instruction: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TagClassification {
    pub tags: Vec<(String, f32)>,
    pub is_undetermined: bool,
}

#[derive(Debug, Clone)]
pub struct DiscoverySample {
    pub id: i64,
    pub ocr_text: String,
    pub description_uk: String,
    pub description_en: String,
    pub title: String,
}

#[derive(Debug)]
pub enum TagError {
    RateLimited(String),
    ContentBlocked(String),
    Transient(String),
}

pub const TAG_TAXONOMY: &[TagDef] = &[
    TagDef {
        slug: "робота",
        emoji: "💼",
        instruction: "Does this meme relate to work, office, deadlines, Monday, weekends, boss, colleagues, meetings, unemployment, or career?",
    },
    TagDef {
        slug: "абсурд",
        emoji: "🤡",
        instruction: "Is this meme absurd, surreal, chaotic, weird, cringe, shitpost, wordplay, or bizarre humor?",
    },
    TagDef {
        slug: "вигорання",
        emoji: "🔥",
        instruction: "Does this meme relate to burnout, stress, exhaustion, fatigue, anxiety, depression, mental health, or therapy?",
    },
    TagDef {
        slug: "реакція",
        emoji: "😮",
        instruction: "Is this a reaction meme showing strong emotion (shock, sarcasm, disgust, approval, confusion, anger, facepalm) meant to reply in chat?",
    },
    TagDef {
        slug: "коти",
        emoji: "🐱",
        instruction: "Does this meme feature or joke about cats or kittens?",
    },
    TagDef {
        slug: "сон",
        emoji: "😴",
        instruction: "Does this meme relate to sleep, insomnia, morning, alarm clock, waking up, laziness, or procrastination?",
    },
    TagDef {
        slug: "політика",
        emoji: "🏛️",
        instruction: "Does this meme relate to politics, Ukraine, news, politicians, government, elections, geopolitics, or satire on authorities?",
    },
    TagDef {
        slug: "тварини",
        emoji: "🦊",
        instruction: "Does this meme feature frogs, bears, pigs, ducks, birds, squirrels, or wild/farm animals (other than cats, dogs, or monkeys)?",
    },
    TagDef {
        slug: "айті",
        emoji: "💻",
        instruction: "Does this meme relate to IT, programming, coding, developers, bugs, production, servers, computers, laptops, or tech?",
    },
    TagDef {
        slug: "гроші",
        emoji: "💸",
        instruction: "Does this meme relate to money, salary, prices, rent, apartments, poverty, wealth, shopping, crypto, or finance?",
    },
    TagDef {
        slug: "кіно",
        emoji: "🎬",
        instruction: "Does this meme feature recognizable movies, TV series, cartoons, anime, actors, or celebrities?",
    },
    TagDef {
        slug: "друзі",
        emoji: "🍻",
        instruction: "Does this meme relate to friends, friendship, introverts, chats, texting, ghosting, parties, or socializing?",
    },
    TagDef {
        slug: "стосунки",
        emoji: "❤️",
        instruction: "Does this meme relate to love, dating, romance, marriage, family, partners, flirting, exes, or spicy/sexual humor?",
    },
    TagDef {
        slug: "побут",
        emoji: "🏠",
        instruction: "Does this meme relate to everyday errands, weather, seasons, traffic, public transport, shops, haircuts, repairs, or adulting?",
    },
    TagDef {
        slug: "чорний",
        emoji: "💀",
        instruction: "Does this meme contain dark humor, death, funerals, suicide jokes, apocalypse, or existential dread?",
    },
    TagDef {
        slug: "здоровя",
        emoji: "🏋️",
        instruction: "Does this meme relate to sports, gym, fitness, health, doctors, back/knee pain, aging, or 30+ age jokes?",
    },
    TagDef {
        slug: "собаки",
        emoji: "🐶",
        instruction: "Does this meme feature or joke about dogs or puppies?",
    },
    TagDef {
        slug: "їжа",
        emoji: "🍕",
        instruction: "Does this meme relate to food, eating, diet, weight loss, hunger, cooking, snacks, or coffee?",
    },
    TagDef {
        slug: "ші",
        emoji: "🤖",
        instruction: "Does this meme relate to AI, ChatGPT, LLMs, prompts, neural networks, Sam Altman, or AI replacing jobs?",
    },
    TagDef {
        slug: "алкоголь",
        emoji: "🍺",
        instruction: "Does this meme relate to alcohol, beer, wine, whiskey, vodka, getting drunk, hangovers, bars, or pubs?",
    },
    TagDef {
        slug: "війна",
        emoji: "🛡️",
        instruction: "Does this meme relate to war, air raid alarms, army, soldiers, drones, missiles, frontline, wartime Ukraine, or IPSO?",
    },
    TagDef {
        slug: "історія",
        emoji: "⚔️",
        instruction: "Does this meme feature medieval themes, knights, castles, classical paintings, ancient Rome, or historical jokes?",
    },
    TagDef {
        slug: "мавпи",
        emoji: "🐒",
        instruction: "Does this meme feature or joke about monkeys, apes, gorillas, chimpanzees, or orangutans?",
    },
    TagDef {
        slug: "навчання",
        emoji: "📚",
        instruction: "Does this meme relate to school, university, exams, students, teachers, studying, science, or math?",
    },
    TagDef {
        slug: "ігри",
        emoji: "🎮",
        instruction: "Does this meme relate to video games, gamers, consoles, GTA, Minecraft, Witcher, or gaming culture?",
    },
];

pub fn default_taxonomy() -> Vec<DynamicTagDef> {
    TAG_TAXONOMY
        .iter()
        .map(|def| DynamicTagDef {
            slug: def.slug.to_string(),
            emoji: def.emoji.to_string(),
            instruction: def.instruction.to_string(),
        })
        .collect()
}

pub fn tag_emoji(slug: &str) -> &'static str {
    if slug == OTHER_TAG_SLUG {
        return OTHER_TAG_EMOJI;
    }
    TAG_TAXONOMY
        .iter()
        .find(|def| def.slug == slug)
        .map(|def| def.emoji)
        .unwrap_or("🏷️")
}

pub fn normalize_tag_alias(raw: &str) -> String {
    let cleaned = raw.trim().trim_start_matches('#').to_lowercase();
    match cleaned.as_str() {
        "work" | "job" | "office" | "career" => "робота".to_string(),
        "absurd" | "surreal" | "chaos" | "cringe" | "крінж" | "хаос" | "сюр" => {
            "абсурд".to_string()
        }
        "burnout" | "stress" | "depression" | "sad" | "депресія" | "стрес" | "втома" => {
            "вигорання".to_string()
        }
        "reaction" | "react" => "реакція".to_string(),
        "cat" | "cats" | "кіт" | "котик" | "котики" => "коти".to_string(),
        "sleep" | "morning" | "lazy" | "ранок" | "лінь" | "прокрастинація" => {
            "сон".to_string()
        }
        "politics" | "politic" | "news" | "україна" | "новини" => "політика".to_string(),
        "animal" | "animals" | "frog" | "bear" | "жаби" | "звірі" => "тварини".to_string(),
        "it" | "іт" | "tech" | "code" | "dev" | "programming" | "програмування" => {
            "айті".to_string()
        }
        "money" | "finance" | "cash" | "зарплата" | "ціни" => "гроші".to_string(),
        "movie" | "movies" | "cinema" | "film" | "anime" | "аніме" | "мультики" => {
            "кіно".to_string()
        }
        "friend" | "friends" | "party" | "інтроверти" => "друзі".to_string(),
        "love" | "dating" | "relationship" | "relationships" | "sex" | "кохання" | "сім'я" => {
            "стосунки".to_string()
        }
        "everyday" | "life" | "weather" | "погода" | "транспорт" | "життєво" => {
            "побут".to_string()
        }
        "dark" | "dark_humor" | "чорний_гумор" | "смерть" => "чорний".to_string(),
        "health" | "sport" | "gym" | "fitness" | "спорт" | "здоров'я" | "здоров’я" => {
            "здоровя".to_string()
        }
        "dog" | "dogs" | "puppy" | "собака" | "пес" | "песики" => "собаки".to_string(),
        "food" | "diet" | "coffee" | "кава" | "дієта" => "їжа".to_string(),
        "ai" | "chatgpt" | "gpt" | "llm" | "нейромережі" | "нейромережа" => {
            "ші".to_string()
        }
        "alcohol" | "beer" | "wine" | "whiskey" | "drunk" | "пиво" | "вино" | "віскі" => {
            "алкоголь".to_string()
        }
        "war" | "army" | "military" | "зсу" | "армія" | "тривога" => "війна".to_string(),
        "history" | "medieval" | "art" | "середньовіччя" | "лицарі" | "мистецтво" => {
            "історія".to_string()
        }
        "monkey" | "monkeys" | "ape" | "gorilla" | "мавпа" | "горила" => {
            "мавпи".to_string()
        }
        "study" | "school" | "uni" | "university" | "школа" | "універ" | "наука" => {
            "навчання".to_string()
        }
        "game" | "games" | "gaming" | "геймінг" => "ігри".to_string(),
        "other" | "misc" | "uncategorized" | "невизначено" | "інше" => {
            OTHER_TAG_SLUG.to_string()
        }
        _ => cleaned,
    }
}

fn build_decision_questions(
    taxonomy: &[DynamicTagDef],
) -> (Map<String, Value>, HashMap<String, String>) {
    let mut questions = Map::with_capacity(taxonomy.len() + 1);
    let mut id_to_slug = HashMap::with_capacity(taxonomy.len());
    for (idx, def) in taxonomy.iter().enumerate() {
        if def.slug == OTHER_TAG_SLUG {
            continue;
        }
        let qid = format!("tag_{}", idx);
        questions.insert(
            qid.clone(),
            json!({
                "type": "noul",
                "instructions": def.instruction,
            }),
        );
        id_to_slug.insert(qid, def.slug.clone());
    }
    questions.insert(
        OTHER_QUESTION_ID.to_string(),
        json!({
            "type": "noul",
            "instructions": OTHER_TAG_INSTRUCTION,
        }),
    );
    (questions, id_to_slug)
}

pub fn select_tags_from_response(
    resp: &Value,
    id_to_slug: &HashMap<String, String>,
) -> Result<TagClassification, String> {
    let answers = resp["answers"]
        .as_object()
        .ok_or_else(|| "Missing 'answers' object in Decision API response".to_string())?;

    let other_score = answers
        .get(OTHER_QUESTION_ID)
        .and_then(|entry| entry["noul"].as_f64())
        .map(|score| score as f32);

    let mut scored: Vec<(String, f32)> = Vec::new();

    for (qid, entry) in answers {
        let Some(slug) = id_to_slug.get(qid) else {
            continue;
        };
        if let Some(score) = entry["noul"].as_f64() {
            scored.push((slug.clone(), score as f32));
        }
    }

    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let best_known_score = scored.first().map(|(_, s)| *s).unwrap_or(0.0);

    if let Some(other) = other_score
        && other > best_known_score
        && other >= OTHER_TAG_MIN_SCORE
    {
        return Ok(TagClassification {
            tags: vec![(OTHER_TAG_SLUG.to_string(), other)],
            is_undetermined: true,
        });
    }

    let confident: Vec<(String, f32)> = scored
        .iter()
        .filter(|(_, score)| *score >= TAG_CONFIDENCE_THRESHOLD)
        .take(MAX_TAGS_PER_MEME)
        .cloned()
        .collect();

    if !confident.is_empty() {
        return Ok(TagClassification {
            tags: confident,
            is_undetermined: false,
        });
    }

    if let Some((best_slug, best_score)) = scored.into_iter().next()
        && best_score >= TAG_FALLBACK_THRESHOLD
    {
        return Ok(TagClassification {
            tags: vec![(best_slug, best_score)],
            is_undetermined: false,
        });
    }

    Ok(TagClassification {
        tags: vec![(
            OTHER_TAG_SLUG.to_string(),
            other_score.unwrap_or(OTHER_TAG_MIN_SCORE),
        )],
        is_undetermined: true,
    })
}

fn shared_http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("Failed to build shared reqwest Client for tagger")
    })
}

fn build_tagger_input(
    ocr_text: &str,
    description_uk: &str,
    description_en: &str,
    title: &str,
) -> String {
    let mut parts: Vec<String> = Vec::new();

    let ocr_trimmed = ocr_text.trim();
    if !ocr_trimmed.is_empty() {
        parts.push(format!("OCR: {}", ocr_trimmed));
    }

    let desc_uk = description_uk.trim();
    let desc_en = description_en.trim();
    if !desc_uk.is_empty() {
        parts.push(format!("Опис: {}", desc_uk));
    } else if !desc_en.is_empty() {
        parts.push(format!("Опис: {}", desc_en));
    }

    let title_trimmed = title.trim();
    if !title_trimmed.is_empty() {
        parts.push(format!("Назва: {}", title_trimmed));
    }

    parts.join("\n")
}

fn is_input_blocked_error(status: StatusCode, err_msg: &str) -> bool {
    if status != StatusCode::BAD_REQUEST {
        return false;
    }
    let lower = err_msg.to_lowercase();
    lower.contains("input blocked")
        || lower.contains("prohibited use policy")
        || lower.contains("sensitive words")
}

async fn request_decision_tags(
    api_key: &str,
    input: &str,
    questions: &Map<String, Value>,
    id_to_slug: &HashMap<String, String>,
) -> Result<TagClassification, TagError> {
    let body = json!({
        "model": DECISIONS_MODEL,
        "input": input,
        "questions": Value::Object(questions.clone()),
    });

    let res = shared_http_client()
        .post(DECISIONS_API_URL)
        .header("x-goog-api-key", api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| TagError::Transient(e.to_string()))?;

    let status = res.status();
    if status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::SERVICE_UNAVAILABLE {
        let msg = format!("Decision API rate limited / busy: {}", status);
        warn!(status = %status, "Decision API busy during meme tagging");
        return Err(TagError::RateLimited(msg));
    }

    let payload: Value = res
        .json()
        .await
        .map_err(|e| TagError::Transient(e.to_string()))?;

    if !status.is_success() {
        let err_msg = payload["error"]["message"]
            .as_str()
            .unwrap_or("Unknown Decision API error");
        warn!(status = %status, error = err_msg, "Decision API returned error");
        let full_err = format!("Decision API {}: {}", status, err_msg);
        if is_input_blocked_error(status, err_msg) {
            return Err(TagError::ContentBlocked(full_err));
        }
        return Err(TagError::Transient(full_err));
    }

    let selected =
        select_tags_from_response(&payload, id_to_slug).map_err(TagError::Transient)?;
    debug!(
        tags_count = selected.tags.len(),
        is_undetermined = selected.is_undetermined,
        "Decision API classified meme tags"
    );
    Ok(selected)
}

pub async fn classify_meme_tags(
    api_key: &str,
    ocr_text: &str,
    description_uk: &str,
    description_en: &str,
    title: &str,
    taxonomy: &[DynamicTagDef],
) -> Result<TagClassification, TagError> {
    let input = build_tagger_input(ocr_text, description_uk, description_en, title);
    if input.is_empty() {
        return Ok(TagClassification {
            tags: Vec::new(),
            is_undetermined: false,
        });
    }

    let (questions, id_to_slug) = build_decision_questions(taxonomy);

    match request_decision_tags(api_key, &input, &questions, &id_to_slug).await {
        Ok(selected) => Ok(selected),
        Err(TagError::ContentBlocked(first_err)) => {
            let en_trimmed = description_en.trim();
            if !en_trimmed.is_empty() {
                let fallback_input = format!("Description: {}", en_trimmed);
                if fallback_input != input {
                    warn!("Decision API blocked full meme text; retrying with sanitized English description only");
                    match request_decision_tags(api_key, &fallback_input, &questions, &id_to_slug)
                        .await
                    {
                        Ok(selected) => {
                            info!(
                                tags_count = selected.tags.len(),
                                "Decision API classified meme via English description fallback"
                            );
                            return Ok(selected);
                        }
                        Err(TagError::ContentBlocked(_)) => {
                            return Err(TagError::ContentBlocked(first_err));
                        }
                        Err(other) => return Err(other),
                    }
                }
            }
            Err(TagError::ContentBlocked(first_err))
        }
        Err(other) => Err(other),
    }
}

fn sanitize_proposed_slug(raw: &str) -> Option<String> {
    let trimmed = raw
        .trim()
        .trim_start_matches('#')
        .to_lowercase()
        .replace([' ', '-'], "_");
    let char_count = trimmed.chars().count();
    if !(2..=24).contains(&char_count) {
        return None;
    }
    if !trimmed.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    let normalized = normalize_tag_alias(&trimmed);
    if matches!(
        normalized.as_str(),
        OTHER_TAG_SLUG | "невизначено" | "other" | "misc" | "uncategorized"
    ) {
        return None;
    }
    Some(normalized)
}

pub fn parse_proposed_categories(
    raw_json: &str,
    existing_slugs: &[String],
    max_to_add: usize,
) -> Result<Vec<DynamicTagDef>, String> {
    if max_to_add == 0 {
        return Ok(Vec::new());
    }

    let trimmed = raw_json.trim();
    let stripped = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .map(str::trim)
        .and_then(|s| s.strip_suffix("```"))
        .map(str::trim)
        .unwrap_or(trimmed);

    let value: Value = serde_json::from_str(stripped)
        .map_err(|e| format!("Failed to parse discovery JSON: {}", e))?;

    let items = if let Some(arr) = value["categories"].as_array() {
        arr
    } else if let Some(arr) = value.as_array() {
        arr
    } else {
        return Err("Discovery JSON missing 'categories' array".to_string());
    };

    let mut seen: HashSet<String> = existing_slugs
        .iter()
        .map(|s| normalize_tag_alias(s))
        .collect();
    let mut accepted = Vec::new();

    for item in items {
        if accepted.len() >= max_to_add {
            break;
        }
        let Some(raw_slug) = item["slug"].as_str() else {
            continue;
        };
        let Some(slug) = sanitize_proposed_slug(raw_slug) else {
            continue;
        };
        if seen.contains(&slug) {
            continue;
        }
        let Some(instruction_raw) = item["instruction"].as_str() else {
            continue;
        };
        let instruction = instruction_raw.trim();
        if instruction.chars().count() < 10 || instruction.chars().count() > 300 {
            continue;
        }
        let emoji = item["emoji"]
            .as_str()
            .map(str::trim)
            .filter(|e| !e.is_empty() && e.chars().count() <= 4)
            .unwrap_or("🏷️")
            .to_string();

        seen.insert(slug.clone());
        accepted.push(DynamicTagDef {
            slug,
            emoji,
            instruction: instruction.to_string(),
        });
    }

    Ok(accepted)
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    trimmed.chars().take(max_chars).collect()
}

pub async fn propose_new_categories(
    api_key: &str,
    samples: &[DiscoverySample],
    existing_tags: &[DynamicTagDef],
    max_to_add: usize,
) -> Result<Vec<DynamicTagDef>, TagError> {
    let cap = max_to_add.min(3);
    if samples.is_empty() || cap == 0 {
        return Ok(Vec::new());
    }

    let existing_list: Vec<&str> = existing_tags.iter().map(|t| t.slug.as_str()).collect();
    let existing_slugs: Vec<String> = existing_tags.iter().map(|t| t.slug.clone()).collect();

    let mut sample_lines = Vec::with_capacity(samples.len());
    for (idx, s) in samples.iter().enumerate() {
        let mut fields = Vec::new();
        if !s.ocr_text.trim().is_empty() {
            fields.push(format!("OCR: {}", truncate_chars(&s.ocr_text, 160)));
        }
        if !s.description_uk.trim().is_empty() {
            fields.push(format!(
                "Desc: {}",
                truncate_chars(&s.description_uk, 220)
            ));
        } else if !s.description_en.trim().is_empty() {
            fields.push(format!(
                "Desc: {}",
                truncate_chars(&s.description_en, 220)
            ));
        }
        if !s.title.trim().is_empty() {
            fields.push(format!("Title: {}", truncate_chars(&s.title, 80)));
        }
        if !fields.is_empty() {
            sample_lines.push(format!("{}. [id={}] {}", idx + 1, s.id, fields.join(" | ")));
        }
    }

    if sample_lines.is_empty() {
        return Ok(Vec::new());
    }

    let prompt = format!(
        "You maintain meme catalog categories for a Telegram bot.\n\
         Existing active categories (DO NOT duplicate or rephrase these): [{}]\n\n\
         Below are {} memes that did NOT fit cleanly into the existing categories:\n\
         {}\n\n\
         Task: Identify ONLY genuinely missing recurring topics that clearly appear in AT LEAST 3 of these memes.\n\
         If the memes are mostly unrelated one-offs, return an empty list.\n\
         Return strict JSON only:\n\
         {{\"categories\": [{{\"slug\": \"lowercase_ukrainian_word\", \"emoji\": \"single_emoji\", \"instruction\": \"Does this meme relate to ...?\"}}]}}\n\
         Rules:\n\
         - At most {} new categories.\n\
         - slug: short 1 Ukrainian word (or snake_case), lowercase Cyrillic/alphanumeric, 2-24 chars, never 'інше' or an existing category.\n\
         - instruction: concise English yes/no classification question for a decision model.",
        existing_list.join(", "),
        sample_lines.len(),
        sample_lines.join("\n"),
        cap,
    );

    let body = json!({
        "contents": [{ "parts": [{ "text": prompt }] }],
        "generationConfig": {
            "responseMimeType": "application/json",
            "temperature": 0.2
        }
    });

    let mut last_err = TagError::Transient("No discovery models configured".to_string());
    for model in DISCOVERY_MODELS {
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent",
            model
        );
        let res = match shared_http_client()
            .post(&url)
            .header("x-goog-api-key", api_key)
            .json(&body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                last_err = TagError::Transient(e.to_string());
                continue;
            }
        };

        let status = res.status();
        if status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::SERVICE_UNAVAILABLE {
            warn!(model = *model, status = %status, "Category discovery rate limited / busy");
            last_err = TagError::RateLimited(format!("Discovery {} rate limited: {}", model, status));
            continue;
        }

        let payload: Value = match res.json().await {
            Ok(v) => v,
            Err(e) => {
                last_err = TagError::Transient(e.to_string());
                continue;
            }
        };

        if !status.is_success() {
            let err_msg = payload["error"]["message"]
                .as_str()
                .unwrap_or("Unknown Gemini error");
            warn!(model = *model, status = %status, error = err_msg, "Category discovery model error");
            last_err = TagError::Transient(format!("Discovery {} {}: {}", model, status, err_msg));
            continue;
        }

        let Some(raw_text) = payload["candidates"][0]["content"]["parts"][0]["text"].as_str()
        else {
            last_err = TagError::Transient("Missing text in discovery Gemini response".to_string());
            continue;
        };

        return parse_proposed_categories(raw_text, &existing_slugs, cap)
            .map_err(TagError::Transient);
    }

    Err(last_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn qid_for(id_to_slug: &HashMap<String, String>, slug: &str) -> String {
        id_to_slug
            .iter()
            .find_map(|(qid, s)| if s == slug { Some(qid.clone()) } else { None })
            .expect("slug missing from id_to_slug")
    }

    #[test]
    fn build_decision_questions_uses_only_valid_ascii_question_ids() {
        let mut taxonomy = default_taxonomy();
        taxonomy.push(DynamicTagDef {
            slug: "музика".to_string(),
            emoji: "🎵".to_string(),
            instruction: "Does this meme relate to music?".to_string(),
        });

        let (questions, id_to_slug) = build_decision_questions(&taxonomy);
        assert!(questions.contains_key(OTHER_QUESTION_ID));
        assert_eq!(questions.len(), taxonomy.len() + 1);

        for key in questions.keys() {
            assert!(!key.is_empty() && key.len() <= 64);
            assert!(
                key.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
                "invalid question ID: {}",
                key
            );
        }

        let recovered: HashSet<&str> = id_to_slug.values().map(String::as_str).collect();
        for def in &taxonomy {
            assert!(recovered.contains(def.slug.as_str()));
        }
    }

    #[test]
    fn select_tags_picks_top_confident_capped_at_five() {
        let taxonomy = default_taxonomy();
        let (_, id_to_slug) = build_decision_questions(&taxonomy);
        let resp = json!({
            "answers": {
                qid_for(&id_to_slug, "робота"): { "noul": 0.91 },
                qid_for(&id_to_slug, "айті"): { "noul": 0.88 },
                qid_for(&id_to_slug, "коти"): { "noul": 0.95 },
                qid_for(&id_to_slug, "сон"): { "noul": 0.75 },
                qid_for(&id_to_slug, "вигорання"): { "noul": 0.82 },
                qid_for(&id_to_slug, "мавпи"): { "noul": 0.79 },
                qid_for(&id_to_slug, "реакція"): { "noul": 0.60 },
                OTHER_QUESTION_ID: { "noul": 0.15 },
                "unknown_tag": { "noul": 0.99 }
            }
        });

        let selected = select_tags_from_response(&resp, &id_to_slug).unwrap();
        assert!(!selected.is_undetermined);
        let slugs: Vec<&str> = selected.tags.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(slugs, vec!["коти", "робота", "айті", "вигорання", "мавпи"]);
    }

    #[test]
    fn select_tags_falls_back_to_single_best_between_fallback_and_threshold() {
        let taxonomy = default_taxonomy();
        let (_, id_to_slug) = build_decision_questions(&taxonomy);
        let resp = json!({
            "answers": {
                qid_for(&id_to_slug, "реакція"): { "noul": 0.65 },
                qid_for(&id_to_slug, "абсурд"): { "noul": 0.52 },
                qid_for(&id_to_slug, "робота"): { "noul": 0.12 },
                OTHER_QUESTION_ID: { "noul": 0.40 }
            }
        });

        let selected = select_tags_from_response(&resp, &id_to_slug).unwrap();
        assert!(!selected.is_undetermined);
        assert_eq!(selected.tags.len(), 1);
        assert_eq!(selected.tags[0].0, "реакція");
        assert!((selected.tags[0].1 - 0.65).abs() < 1e-6);
    }

    #[test]
    fn select_tags_marks_undetermined_when_other_beats_best_known() {
        let taxonomy = default_taxonomy();
        let (_, id_to_slug) = build_decision_questions(&taxonomy);
        let resp = json!({
            "answers": {
                qid_for(&id_to_slug, "реакція"): { "noul": 0.58 },
                qid_for(&id_to_slug, "побут"): { "noul": 0.40 },
                OTHER_QUESTION_ID: { "noul": 0.76 }
            }
        });

        let selected = select_tags_from_response(&resp, &id_to_slug).unwrap();
        assert!(selected.is_undetermined);
        assert_eq!(selected.tags.len(), 1);
        assert_eq!(selected.tags[0].0, OTHER_TAG_SLUG);
        assert!((selected.tags[0].1 - 0.76).abs() < 1e-6);
    }

    #[test]
    fn select_tags_marks_undetermined_when_all_scores_below_fallback() {
        let taxonomy = default_taxonomy();
        let (_, id_to_slug) = build_decision_questions(&taxonomy);
        let resp = json!({
            "answers": {
                qid_for(&id_to_slug, "реакція"): { "noul": 0.30 },
                qid_for(&id_to_slug, "побут"): { "noul": 0.44 },
                qid_for(&id_to_slug, "робота"): { "noul": 0.05 },
                OTHER_QUESTION_ID: { "noul": 0.48 }
            }
        });

        let selected = select_tags_from_response(&resp, &id_to_slug).unwrap();
        assert!(selected.is_undetermined);
        assert_eq!(selected.tags.len(), 1);
        assert_eq!(selected.tags[0].0, OTHER_TAG_SLUG);
        assert!((selected.tags[0].1 - 0.48).abs() < 1e-6);
    }

    #[test]
    fn normalize_tag_alias_maps_english_and_ukrainian_variants() {
        assert_eq!(normalize_tag_alias("#іт"), "айті");
        assert_eq!(normalize_tag_alias("#мавпа"), "мавпи");
        assert_eq!(normalize_tag_alias("#ai"), "ші");
        assert_eq!(normalize_tag_alias("#burnout"), "вигорання");
        assert_eq!(normalize_tag_alias("#dark"), "чорний");
        assert_eq!(normalize_tag_alias("#Work"), "робота");
        assert_eq!(normalize_tag_alias("#Здоров'я"), "здоровя");
        assert_eq!(normalize_tag_alias("#невизначено"), "інше");
        assert_eq!(normalize_tag_alias("other"), "інше");
    }

    #[test]
    fn parse_proposed_categories_filters_duplicates_and_invalid_entries() {
        let existing = vec!["айті".to_string(), "робота".to_string()];
        let raw = r##"```json
        {
          "categories": [
            {"slug": "#it", "emoji": "💻", "instruction": "Does this relate to IT or code?"},
            {"slug": "інше", "emoji": "📦", "instruction": "Does this not fit other tags?"},
            {"slug": "авто", "emoji": "🚗", "instruction": "Does this meme relate to cars, driving, traffic, or auto repairs?"},
            {"slug": "музика", "emoji": "🎵", "instruction": "Does this meme relate to music, songs, concerts, or musicians?"},
            {"slug": "спорт_екстра", "emoji": "⚽", "instruction": "Does this meme relate to football or sports competitions?"},
            {"slug": "музика", "emoji": "🎶", "instruction": "Duplicate music category that should be skipped."}
          ]
        }
        ```"##;

        let parsed = parse_proposed_categories(raw, &existing, 2).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].slug, "авто");
        assert_eq!(parsed[0].emoji, "🚗");
        assert_eq!(parsed[1].slug, "музика");
        assert_eq!(parsed[1].emoji, "🎵");
    }

    #[test]
    fn is_input_blocked_error_detects_policy_blocks_only_on_400() {
        assert!(is_input_blocked_error(
            StatusCode::BAD_REQUEST,
            "Input blocked: The prompt contains sensitive words that violate Google's Generative AI Prohibited Use policy."
        ));
        assert!(is_input_blocked_error(
            StatusCode::BAD_REQUEST,
            "Request failed due to Prohibited Use Policy violation"
        ));
        assert!(!is_input_blocked_error(
            StatusCode::BAD_REQUEST,
            "question IDs must be 1-64 characters"
        ));
        assert!(!is_input_blocked_error(
            StatusCode::TOO_MANY_REQUESTS,
            "Input blocked"
        ));
    }
}
