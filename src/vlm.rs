use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::database::enums::vlm_error_enum::VlmError;

const GEMINNI_MODEL_POOL: &[&str] = &[
    "gemini-3.1-flash-lite",
    "gemini-3.5-flash-lite",
    "gemini-3.6-flash",
    "gemini-3.8-flash",
];

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
    match serde_json::Value::deserialize(deserializer)? {
        serde_json::Value::Bool(b) => Ok(b),
        serde_json::Value::Number(n) => Ok(n.as_i64().unwrap_or(0) != 0),
        serde_json::Value::String(s) => match s.trim().to_lowercase().as_str() {
            "true" | "1" | "yes" => Ok(true),
            "false" | "0" | "no" | "" => Ok(false),
            other => Err(serde::de::Error::custom(format!(
                "Invalid boolean string: {}",
                other
            ))),
        },
        serde_json::Value::Null => Ok(false),
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

pub async fn describe_meme(
    image_bytes: &[u8],
    api_key: &str,
    mime_type: &str,
) -> Result<MemeAnalysis, VlmError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| VlmError::Transient(e.to_string()))?;

    let base64_image = STANDARD.encode(image_bytes);

    println!(
        "[VLM] Send {} to Gemini {} (size: {} KB)...",
        mime_type,
        GEMINNI_MODEL_POOL[0],
        image_bytes.len() / 1024
    );

    let prompt = VLM_PROMPT;

    let body = json!({
        "contents": [{
            "parts": [
                { "text" : prompt },
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

    let mut last_error = String::new();

    for model in GEMINNI_MODEL_POOL {
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent",
            model
        );

        let res = match client
            .post(&url)
            .header("x-goog-api-key", api_key)
            .json(&body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                last_error = e.to_string();
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                continue;
            }
        };

        let status = res.status();
        println!("[VLM] Received HTTP status: {} | model {}", status, model);

        if status == StatusCode::SERVICE_UNAVAILABLE || status == StatusCode::TOO_MANY_REQUESTS {
            eprintln!(
                "[VLM] {} is busy (status {}). Switch to next model...",
                model, status
            );
            last_error = format!("Google API {}: Model busy/overloaded", status);
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            continue;
        }

        let json_responce: serde_json::Value = res
            .json()
            .await
            .map_err(|e| VlmError::Transient(e.to_string()))?;

        if !status.is_success() {
            let err_msg = json_responce["error"]["message"]
                .as_str()
                .unwrap_or("Unknown Gemini API error");
            eprintln!("[VLM ERROR] Google API returned {}: {}", status, err_msg);
            return Err(VlmError::Transient(format!(
                "Google API {}: {}",
                status, err_msg
            )));
        }

        if let Some(candidate) = json_responce["candidates"].get(0)
            && candidate["finishReason"].as_str() == Some("SAFETY")
        {
            println!(
                "[VLM BLOCKED] Gemini blocked this image by Safety Filter! Gemini response: {}",
                json_responce
            );
            return Err(VlmError::InappropriateContent(
                "Inappropriate content detected (Gemini Safety Filter Explicit/NSFW)".to_string(),
            ));
        }

        let raw_text = json_responce["candidates"][0]["content"]["parts"][0]["text"]
            .as_str()
            .ok_or_else(|| VlmError::Transient("Missing text in Gemini response".to_string()))?;

        let analysis = parse_meme_analysis(raw_text)?;
        if analysis.is_nsfw {
            println!(
                "[VLM BLOCKED] Explicit/NSFW content detected in description! Gemini response: {}",
                json_responce
            );
            return Err(VlmError::InappropriateContent(
            "NSFW explicit content detected. Inappropriate content detected: NSFW/Adult content is not allowed!".to_string(),
        ));
        }
        println!("[VLM] Sucessfully processed image in AI!");
        return Ok(analysis);
    }

    Err(VlmError::Transient(format!(
        "All VLM models failed. Last error: {}",
        last_error
    )))
}

fn unwrap_vlm_value(value: serde_json::Value) -> Result<serde_json::Value, String> {
    match value {
        serde_json::Value::Array(mut arr) => {
            if arr.is_empty() {
                return Err("Empty JSON array from VLM".to_string());
            }
            unwrap_vlm_value(arr.remove(0))
        }
        serde_json::Value::Object(map) => {
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
                return Ok(serde_json::Value::Object(map));
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

            Ok(serde_json::Value::Object(map))
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

    let value: serde_json::Value = serde_json::from_str(json_slice)
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
}
