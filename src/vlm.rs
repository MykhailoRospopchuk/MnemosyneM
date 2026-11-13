use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::database::enums::vlm_error_enum::VlmError;

const GEMINNI_MODEL_POOL: &[&str] = &[
    "gemini-3.5-flash-lite",
    "gemini-3.1-flash-lite",
    "gemini-3.5-flash",
    "gemini-3.6-flash",
    "gemini-3.8-flash",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemeAnalysis {
    #[serde(default)]
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

    let prompt = "
        You are a meme indexing engine.

        SAFETY RULES:
        1. ART, DRAWINGS & SATIRE ARE ALWAYS SAFE:
            Medieval illuminations, historical paintings, frescoes, statues, cartoons, comics, sketches, caricatures, and drawn art are CULTURAL HERITAGE, ART & HUMOR.
            Set 'is_nsfw': false for ALL drawings, medieval art, paintings, and caricatures regardless of any anatomical humor.
        2. REAL-LIFE PHOTOGRAPHIC NUDITY ONLY:
            Set 'is_nsfw': true ONLY for REAL-LIFE PHOTOGRAPHS depicting explicit human genitalia or direct real-life sexual acts.
        3. Erotic jokes, wordplay, double entendres, underwear, and suggestive memes are SAFE ('is_nsfw': false).
        TASK:
        Extract all text from this meme verbatim. Then briefly describe the scene, characters, and the joke context in both Ukrainian and English so it can be easily found by keywords.
        JSON fields to return:
        - 'is_nsfw': boolean (false for all drawings, art, memes, and caricatures; true ONLY for real-life photographic pornography)
        - 'ocr_text': exact verbatim text written on the meme (empty string if none)
        - 'uk_description': describe the scene, characters, joke context, and key search keywords in Ukrainian.
        - 'en_description': meme template name, scene description, and key search keywords in English.
        ";

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

        let trimmed = raw_text.trim();
        let without_prefix = trimmed
            .strip_prefix("```json")
            .or_else(|| trimmed.strip_prefix("```"))
            .unwrap_or(trimmed);
        let clean_json = without_prefix
            .strip_suffix("```")
            .unwrap_or(without_prefix)
            .trim();
        let analysis: MemeAnalysis = serde_json::from_str(clean_json).map_err(|e| {
            VlmError::Transient(format!("JSON parse error: {} | Raw: {}", e, raw_text))
        })?;
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
