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
        You are a content safety filter and meme analysis engine.
        FIRST: Carefully check if the image contains real-life human nudity, exposed genitalia, pornographic acts, or explicit adult content.
        If it contains nudity or explicit adult content, DO NOT describe it. Instead, reply EXACTLY and ONLY with:
        [REJECTED_NSFW]
        OTHERWISE (if the image is a safe meme, cartoon, or clean photo):
        Extract all text from this meme verbatim. Then briefly describe the scene, characters, and the joke context in both Ukrainian and English so it can be easily found by keywords.";

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
            println!("[VLM BLOCKED] Gemini blocked this image by Safety Filter!");
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
        || lower.contains("adult content")
        || lower.contains("еротика")
        || lower.contains("контент для дорослих")
    {
        println!("[VLM BLOCKED] Explicit/NSFW content detected in description!");
        return Err(VlmError::InappropriateContent(
            "Inappropriate content detected: NSFW/Adult content is not allowed!".to_string(),
        ));
    }

    println!("[VLM] Sucessfully processed image in AI!");
    Ok(description)
}
