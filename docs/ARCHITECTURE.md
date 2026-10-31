# MnemosyneM: Architecture & Technical Context

> **Note for Developers & AI Assistants:** This document contains the complete context, architectural decisions, database schemas, and lessons learned for the `MnemosyneM` project. Read this file to immediately understand the system state and design invariants.

---

## 1. High-Level Architecture

MnemosyneM is a multimodal semantic search engine for memes that runs as a Telegram bot.

```
                      ┌───────────────────────────────────────┐
                      │             Telegram User             │
                      └───────┬───────────────────────▲───────┘
          Photo (New Meme)    │                       │  Found Memes (Album)
          or Text Search      ▼                       │
                      ┌───────────────────────────────┴───────┐
                      │        Teloxide Bot (main.rs)         │
                      └───────┬───────────────────────▲───────┘
                              │                       │
           Ingestion Path     │                       │   Search Path
       ┌──────────────────────▼───────┐       ┌───────┴───────────────────────┐
       │     Gemini 3.5 Flash Lite    │       │   FastEmbed (Multilingual E5) │
       │  Visual Language Model (VLM) │       │   768-dim Vector Calculation  │
       │   Scene, OCR & Joke Context  │       │       Sub-30ms CPU Query      │
       └──────────────┬───────────────┘       └───────▲───────────────────────┘
                      │                               │
                      │ Detailed Descriptions         │ Cosine Distance
                      ▼                               ▼
       ┌──────────────────────────────────────────────────────────────┐
       │                    MemeStore (lib.rs)                        │
       │           In-Memory Cache (Vec<Meme>) + SQLite               │
       │         768-dim BLOB Vectors + Telegram File IDs             │
       └──────────────────────────────────────────────────────────────┘
```

---

## 2. Core Architectural Decisions

### 2.1. Hybrid VLM Architecture (Cloud VLM + Local Embeddings)
* **Problem:** Running a local multimodal model (e.g. Qwen2-VL 2B or CLIP Vision) on CPU consumed >8 GB RAM, causing severe swap thrashing and latency (>30 seconds per image).
* **Solution:**
  1. **Image Ingestion (Cloud):** Use **Google Gemini 3.5 Flash Lite** API (`models/gemini-3.5-flash-lite:generateContent`). It extracts verbatim text (OCR), scenes, characters, and joke context in both Ukrainian and English in under 2 seconds. Free tier provides **500 requests per day (RPD)** and **15 requests per minute (RPM)**.
  2. **Semantic Search (Local):** Use **FastEmbed** (`EmbeddingModel::MultilingualE5Base`) running on local ONNX Runtime. Text embedding of search queries takes **~30 milliseconds** and uses <400 MB RAM.
  3. **Zero-API Search:** Search queries never call external APIs. Search is 100% free, unlimited, private, and runs locally.

### 2.2. Zero-Disk Image Storage
* Rather than storing image files on the local filesystem or S3, MnemosyneM stores Telegram's `file_tg_id` and `file_tg_unique_id`.
* Images are served back to users using Telegram's native `InputFile::file_id(...)`.
* Result: Host disk usage is **0 bytes for images**, regardless of whether the database holds 10 or 100,000 memes.

### 2.3. Embedding Format & Model Invariants
* **Model:** `intfloat/multilingual-e5-base` (768 dimensions).
* **Prefix Rule (Critical for E5):**
  * Ingestion passages MUST be prefixed with `"passage: "`:
    ```rust
    let format_passage = format!("passage: {}", descriptions);
    ```
  * Search queries MUST be prefixed with `"query: "`:
    ```rust
    let format_query = format!("query: {}", search_text);
    ```
  * Failing to use these prefixes significantly degrades cosine similarity accuracy.
* **Vector Serialization:** 768 `f32` values are packed as Little-Endian bytes (`768 * 4 = 3072 bytes`) and stored as a SQLite `BLOB`.

---

## 3. Database Schema (`mnemosynem.db`)

SQLite database is located at `../mnemosynem.db` relative to `m_nemosyne_m/`.

```sql
CREATE TABLE IF NOT EXISTS memes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    title TEXT NOT NULL,                  -- Caption or title
    file_tg_id TEXT NOT NULL,             -- Telegram file ID (for sending)
    file_tg_unique_id TEXT NOT NULL UNIQUE, -- Permanent file unique ID (for deduplication)
    embedding BLOB NOT NULL,              -- 3072 bytes (768 * 4 f32)
    user_id INTEGER NOT NULL DEFAULT 0,   -- Telegram User ID of uploader (v0.2)
    is_public BOOLEAN NOT NULL DEFAULT 1, -- Public vs Private visibility (v0.2)
    status TEXT NOT NULL DEFAULT 'approved', -- 'approved', 'quarantined', 'rejected' (v0.3)
    safety_score REAL DEFAULT 1.0         -- Safety score (v0.3)
);

CREATE INDEX IF NOT EXISTS idx_memes_unique_id ON memes(file_tg_unique_id);
```

---

## 4. Key Engineering Lessons & Gotchas

1. **Telegram Caption Limit (1024 chars):**
   * Telegram's `sendPhoto` and `sendMediaGroup` fail with `400 Bad Request: MEDIA_CAPTION_TOO_LONG` if `caption` exceeds 1024 characters.
   * **Rule:** Never put the full Gemini description in the caption. Always truncate captions using Unicode character iterators:
     ```rust
     let short_title: String = meme.title.chars().take(200).collect();
     ```
   * *Caution:* Do not use byte slices `&str[..200]` because multi-byte UTF-8 characters (like Ukrainian letters) will cause a panic if sliced on a non-character boundary.

2. **Async Mutex Boundaries (No Locks Across Network `.await`):**
   * Never hold `state.meme_store.lock().await` or `state.engine.lock().await` across long network calls (such as Telegram downloads or Gemini API requests).
   * Scope locks tightly with `{ ... }` blocks so `MutexGuard` is dropped before calling `.await`.

3. **Gemini API Key Security:**
   * Always pass the Gemini API key via the HTTP header:
     ```rust
     .header("x-goog-api-key", api_key)
     ```
   * Never pass `?key={}` in the query string, as `reqwest::Error` will leak the private API key in logs.

4. **CodeLLDB Zombie Processes in IDEs:**
   * Stepping over `.await` with `F10` in debuggers (like CodeLLDB in Zed/VSCode) can lose the Tokio async task context and leave orphaned debugger processes consuming gigabytes of RAM.
   * Terminate zombie instances with `killall -9 codelldb`.

5. **Score Calibration for E5:**
   * For Multilingual E5, raw cosine similarity for relevant matches ranges from **0.78 to 0.90** (unrelated text is ~0.65 to 0.72).
   * Calibrate percentage accordingly:
     ```rust
     pub fn score_calibration(raw_score: f32) -> f32 {
         ((raw_score - 0.70) / (0.85 - 0.70) * 100.0).clamp(0.0, 100.0)
     }
     ```

---

## 5. Development & Running

### Environment Variables (`.env`)
```bash
TELOXIDE_TOKEN=your_telegram_bot_token
DB_PATH=../mnemosynem.db
GEMINI_API_KEY=your_gemini_api_key
TOP_K=3
ADMIN_TELEGRAM_ID=your_telegram_user_id
```

### Running the Bot
```bash
cd m_nemosyne_m
cargo run
```
