use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::json;

const GEMINNI_MODEL_POOL: &[&str] = &[
    "gemini-3.5-flash-lite",
    "gemini-3.5-flash",
    "gemini-3.6-flash",
    "gemini-3.1-flash-lite",
];

#[derive(Debug)]
pub enum VlmError {
    InappropriateContent(String),
    Transient(String),
}

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

pub async fn describe_meme(image_bytes: &[u8], api_key: &str) -> Result<MemeAnalysis, VlmError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| VlmError::Transient(e.to_string()))?;

    let base64_image = STANDARD.encode(image_bytes);

    println!(
        "[VLM] Send image to Gemini {} (image: {} KB)...",
        GEMINNI_MODEL_POOL[0],
        image_bytes.len() / 1024
    );

    let prompt = "
        You are a meme indexing engine with strict pornography protection.
        SAFETY RULE:
        Only reject the image if it contains EXPLICIT, UNCLOTHED human nudity:
        - Exposed genitalia (naked penis, vulva/vagina, anus)
        - Bare female breasts/nipples
        - Direct sexual intercourse
        CRITICAL INSTRUCTIONS:
        - Fully clothed or partially clothed people, underwear, bikinis, suggestive poses, romantic hugs, and adult/erotic jokes or innuendo in memes are SAFE.
        - If the image contains text, humor, and people in clothes — IT IS A VALID MEME, DO NOT REJECT IT!
        If and ONLY if the image depicts explicitly exposed genitalia, bare breasts, or sexual intercourse, reply EXACTLY with:
        [REJECTED_NSFW]
        OTHERWISE:
        Extract all text from this meme verbatim. Then briefly describe the scene, characters, and the joke context in both Ukrainian and English so it can be easily found by keywords.

        SAFETY RULE:
        Set 'is_nsfw': true ONLY if the image depicts explicitly exposed genitalia, bare breasts, or sexual intercourse.
        Clothed people, adult/erotic jokes, innuendo, and underwear are SAFE ('is_nsfw': false).
        JSON fields to return:
        - 'is_nsfw': boolean
        - 'ocr_text': exact verbatim text written on the meme (empty string if none)
        - 'uk_description': describe the scene, characters, joke context, and key search keywords in Ukrainian.
        - 'en_description': meme template name (e.g. 'thinking monkey', 'distracted boyfriend'), scene description, and key search keywords in English.
        ";

    let body = json!({
        "contents": [{
            "parts": [
                { "text" : prompt },
                {
                    "inline_data": {
                        "mime_type": "image/jpeg",
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

        if let Some(candidate) = json_responce["candidates"].get(0) {
            if candidate["finishReason"].as_str() == Some("SAFETY") {
                println!(
                    "[VLM BLOCKED] Gemini blocked this image by Safety Filter! Gemini response: {}",
                    json_responce
                );
                return Err(VlmError::InappropriateContent(
                    "Inappropriate content detected (Gemini Safety Filter Explicit/NSFW)"
                        .to_string(),
                ));
            }
        }

        let raw_text = json_responce["candidates"][0]["content"]["parts"][0]["text"]
            .as_str()
            .ok_or_else(|| VlmError::Transient("Missing text in Gemini response".to_string()))?;

        let clean_json = raw_text
            .trim()
            .strip_prefix("```json")
            .unwrap_or(raw_text)
            .strip_prefix("```")
            .unwrap_or(raw_text)
            .strip_suffix("```")
            .unwrap_or(raw_text)
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
