use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::json;

#[derive(Debug)]
pub enum VlmError {
    InappropriateContent(String),
    Transient(String),
}

pub async fn describe_meme(image_bytes: &[u8], api_key: &str) -> Result<String, VlmError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(25))
        .build()
        .map_err(|e| VlmError::Transient(e.to_string()))?;

    let base64_image = STANDARD.encode(image_bytes);

    let url = "https://generativelanguage.googleapis.com/v1beta/models/gemini-3.5-flash-lite:generateContent".to_string();

    println!(
        "[VLM] Send image to Gemini 3.5 Flash (image: {} KB)...",
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
        "safetySettings": [
            {
                "category": "HARM_CATEGORY_SEXUALLY_EXPLICIT",
                "threshold": "BLOCK_LOW_AND_ABOVE"
            },
            {
                "category": "HARM_CATEGORY_DANGEROUS_CONTENT",
                "threshold": "BLOCK_MEDIUM_AND_ABOVE"
            }
        ]
    });

    let res = client
        .post(&url)
        .header("x-goog-api-key", api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| VlmError::Transient(e.to_string()))?;

    let status = res.status();
    println!("[VLM] Received HTTP status: {}", status);

    let json_responce: serde_json::Value = res
        .json()
        .await
        .map_err(|e| VlmError::Transient(e.to_string()))?;

    if let Some(candidate) = json_responce["candidates"].get(0) {
        if candidate["finishReason"].as_str() == Some("SAFETY") {
            println!(
                "[VLM BLOCKED] Gemini blocked this image by Safety Filter! Gemini response: {}",
                json_responce
            );
            return Err(VlmError::InappropriateContent(
                "Inappropriate content detected (Gemini Safety Filter Explicit/NSFW)".to_string(),
            ));
        }
    }

    let description = json_responce["candidates"][0]["content"]["parts"][0]["text"]
        .as_str()
        .ok_or_else(|| {
            VlmError::Transient(format!("Invalid responce from Gemini: {:?}", json_responce))
        })?
        .to_string();

    let lower = description.to_lowercase();
    if description.contains("[REJECTED_NSFW]")
        || lower.contains("nsfw")
        || lower.contains("pornography")
        || lower.contains("порнографія")
        || lower.contains("exposed genitals")
        || lower.contains("голі геніталії")
    {
        println!(
            "[VLM BLOCKED] Explicit/NSFW content detected in description! Gemini response: {}",
            json_responce
        );
        return Err(VlmError::InappropriateContent(
            "Inappropriate content detected: NSFW/Adult content is not allowed!".to_string(),
        ));
    }

    println!("[VLM] Sucessfully processed image in AI!");
    Ok(description)
}
