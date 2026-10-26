use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::json;

pub async fn describe_meme(image_bytes: &[u8], api_key: &str) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(25))
        .build()
        .map_err(|e| e.to_string())?;

    let base64_image = STANDARD.encode(image_bytes);

    let url = "https://generativelanguage.googleapis.com/v1beta/models/gemini-3.5-flash-lite:generateContent".to_string();

    println!(
        "[VLM] Send image from Gemini 3.8 Flash (image: {} KB)...",
        image_bytes.len() / 1024
    );

    let prompt = "Extract all text from this meme verbatim. Then briefly describe the scene, characters, and the joke context in both Ukrainian and English so it can be easily found by keywords.";

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
        }]
    });

    let res = client
        .post(&url)
        .header("x-goog-api-key", api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let status = res.status();
    println!("[VLM] Received HTTP status: {}", status);

    let json_responce: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;

    let description = json_responce["candidates"][0]["content"]["parts"][0]["text"]
        .as_str()
        .ok_or_else(|| format!("Invalid responce from Gemini: {:?}", json_responce))?
        .to_string();

    println!("[VLM] Sucessfully processed image in AI!");
    Ok(description)
}
