# MnemosyneM (Мнемозіна) 🧠✨

> **Fast, Multimodal Semantic Meme Search Engine & Telegram Bot in Rust**

MnemosyneM indexes your meme collection and lets you search for memes by meaning, joke punchlines, visual scene descriptions, or extracted text — in both **Ukrainian and English**.

---

## 🚀 Key Features

* **Multimodal Semantic Understanding:** Combines **Gemini 3.5 Flash Lite** (for detailed visual recognition, verbatim OCR, and cultural joke context) with **FastEmbed Multilingual E5 Base** (for 768-dimensional local vector representations).
* **Cross-Lingual Search:** Search in Ukrainian for English memes, or in English for Ukrainian memes (e.g. searching `"dog with flower"` finds `"собака з квіткою"`).
* **Zero-Disk Image Footprint:** Images are stored via Telegram's cloud storage (`file_id`); local storage only holds vector embeddings and metadata in SQLite.
* **Blazing Fast Local Search:** Text query embeddings are computed in <30ms locally on CPU via ONNX Runtime without touching any external APIs during search.
* **Instant Deduplication:** Prevents duplicate AI processing by verifying Telegram `file_unique_id` in memory before calling Gemini.

---

## 🛠️ Architecture & Tech Stack

* **Language:** Rust (2024 edition)
* **Telegram Framework:** [`teloxide`](https://github.com/teloxide/teloxide) (v0.17)
* **Async Runtime:** [`tokio`](https://tokio.rs/)
* **VLM (Visual Language Model):** Google Gemini 3.5 Flash Lite
* **Embedding Model:** `intfloat/multilingual-e5-base` via [`fastembed-rs`](https://github.com/Anush008/fastembed-rs)
* **Storage:** SQLite via [`rusqlite`](https://github.com/rusqlite/rusqlite)

For in-depth architectural details, gotchas, and concurrency patterns, see:
👉 **[Architecture & Context Documentation](docs/ARCHITECTURE.md)**

---

## 🗺️ Product Roadmap

We are actively evolving MnemosyneM into a privacy-first, community-driven meme repository with automated moderation and a custom vector search engine from scratch:
👉 **[Product Roadmap & Technical Backlog (ROADMAP.md)](ROADMAP.md)**

* **v0.1:** Core PoC (Completed) ✅
* **v0.2:** Clean UX, Rate-Limiting Queue (15 RPM), Public vs Private Memes 🚧
* **v0.3:** Automated NSFW / Safety Ratings & Telegram Admin Moderation Queue 📋
* **v0.4:** Custom High-Performance Vector Engine (AVX2 SIMD + In-Memory HNSW) 🚀

---

## 🏁 Quick Start

### 1. Requirements
* Rust 1.85+ / Cargo
* Telegram Bot Token (from [@BotFather](https://t.me/BotFather))
* Google Gemini API Key (from [Google AI Studio](https://aistudio.google.com/))

### 2. Configuration
Create a `.env` file in the project root:
```bash
cp .env.example .env
```
Fill in your credentials:
```env
TELOXIDE_TOKEN=your_telegram_bot_token
DB_PATH=../mnemosynem.db
GEMINI_API_KEY=your_gemini_api_key
TOP_K=3
```

### 3. Run
```bash
cargo run
```
Send a photo to your bot in Telegram to index your first meme, then send any search phrase to find it!