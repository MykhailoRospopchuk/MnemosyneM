use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::json;

pub async fn dewscribe_meme(image_bytes: &[u8], api_key: &str) -> Result<String, String> {
    let client = reqwest::Client::new();
    let base64_image = STANDARD.encode(image_bytes);

    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent?key={}",
        api_key
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
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let json_responce: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;

    let description = json_responce["candidates"][0]["content"]["parts"][0]["text"]
        .as_str()
        .ok_or_else(|| format!("Invalid responce from Gemini: {:?}", json_responce))?
        .to_string();

    Ok(description)
}
