use image_hasher::HasherConfig;
use std::io::{ErrorKind, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempFileGuard(PathBuf);

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

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
    match run_ffmpeg_pipe(video_bytes) {
        Ok(frame) => Ok(frame),
        Err(pipe_err) => run_ffmpeg_tempfile(video_bytes).map_err(|file_err| {
            format!(
                "pipe failed ({}); tempfile fallback failed ({})",
                pipe_err, file_err
            )
        }),
    }
}

pub fn hamming_distance(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

fn run_ffmpeg_pipe(video_bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut child = Command::new("ffmpeg")
        .args([
            "-i",
            "pipe:0",
            "-vframes",
            "1",
            "-f",
            "image2pipe",
            "-vcodec",
            "mjpeg",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to spawn ffmpeg: {}", e))?;

    let output = std::thread::scope(|s| {
        let handle = child.stdin.take().map(|mut stdin| {
            s.spawn(move || match stdin.write_all(video_bytes) {
                Ok(()) => Ok(()),
                // ffmpeg exits and closes stdin as soon as -vframes 1 is decoded
                Err(e) if e.kind() == ErrorKind::BrokenPipe => Ok(()),
                Err(e) => Err(e),
            })
        });

        let output = child
            .wait_with_output()
            .map_err(|e| format!("ffmpeg execution failed: {}", e))?;

        if let Some(h) = handle
            && let Ok(Err(e)) = h.join()
        {
            return Err(format!("Failed to write to ffmpeg stdin: {}", e));
        }

        Ok(output)
    })?;

    if output.status.success() && !output.stdout.is_empty() {
        Ok(output.stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!(
            "ffmpeg failed to extract video frame: {}",
            stderr.trim()
        ))
    }
}

fn run_ffmpeg_tempfile(video_bytes: &[u8]) -> Result<Vec<u8>, String> {
    let id = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp_path = std::env::temp_dir().join(format!(
        "mnemosynem_frame_{}_{}.mp4",
        std::process::id(),
        id
    ));
    let guard = TempFileGuard(tmp_path.clone());

    std::fs::write(&guard.0, video_bytes)
        .map_err(|e| format!("Failed to write temp video file: {}", e))?;

    let output = Command::new("ffmpeg")
        .arg("-y")
        .arg("-i")
        .arg(&guard.0)
        .args([
            "-vframes",
            "1",
            "-f",
            "image2pipe",
            "-vcodec",
            "mjpeg",
            "pipe:1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("ffmpeg execution failed: {}", e))?;

    if output.status.success() && !output.stdout.is_empty() {
        Ok(output.stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!(
            "ffmpeg failed to extract video frame: {}",
            stderr.trim()
        ))
    }
}
