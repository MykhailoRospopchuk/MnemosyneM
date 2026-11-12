use std::io::Write;
use std::process::{Command, Stdio};
use image_hasher::HasherConfig;

pub fn compute_hash(image_bytes: &[u8]) -> Result<u64, String> {
    let img = image::load_from_memory(image_bytes).map_err(|e| e.to_string())?;

    let hasher = HasherConfig::new().to_hasher();
    let hash = hasher.hash_image(&img);

    let butes = hash.as_bytes();
    if butes.len() == 8 {
        Ok(u64::from_be_bytes(butes.try_into().unwrap()))
    } else {
        Err("Unexpacted hash lenght".to_string())
    }
}

pub fn extract_frame_from_video(video_bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut child = Command::new("ffmpeg")
        .args([
            "-i", "pipe:0",
            "-vframes", "1",
            "-f", "image2pipe",
            "-vcodec", "mjpeg",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Failed to spawn ffmpeg: {}", e))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(video_bytes)
            .map_err(|e| format!("Failed to write to ffmpeg stdin: {}", e))?;
    }

    let output = child
        .wait_with_output()
        .map_err(|e| format!("ffmpeg execution failed: {}", e))?;

    if output.status.success() && !output.stdout.is_empty() {
        Ok(output.stdout)
    } else {
        Err("ffmpeg failed to extract video frame".to_string())
    }
}
