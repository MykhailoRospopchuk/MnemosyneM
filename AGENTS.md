# MnemosyneM (`m_nemosyne_m`) — Agent Guide

## Project Overview
Multimodal semantic meme search engine & Telegram bot written in Rust (2024 edition).
- **Stack**: `teloxide` (Telegram bot), `tokio` (async runtime), `fastembed` (`intfloat/multilingual-e5-base`, 768-dim embeddings), `rusqlite` (SQLite), `reqwest` (Gemini VLM API).
- **Architecture docs**: See `docs/ARCHITECTURE.md` and `ROADMAP.md` before large refactors.

## Directory & Module Map
- `src/main.rs` — Binary entry point, bot startup, dispatcher, and worker initialization.
- `src/lib.rs` — Library exports (`database`, `engine`, `helpers`, `meme_store`, `migrations`, `vlm`).
- `src/config.rs` — Environment/configuration loading (`.env`).
- `src/bot/` — Telegram command definitions (`comand.rs`) and message/callback handlers (`handlers.rs`).
- `src/database/` — SQLite access, entities (`entity/`), and enums (`enums/`).
- `src/engine.rs` — Core search/indexing orchestration.
- `src/meme_store.rs` — In-memory / vector store & deduplication.
- `src/migrations.rs` — SQLite schema migrations.
- `src/vlm.rs` — Gemini VLM client for visual description & OCR.
- `src/worker.rs` — Background indexing/rate-limited queue worker.
- `src/maintenance/` — Background maintenance worker (`maintance_worker.rs`).
- `src/helpers/` — Embedding, perceptual hashing, and search query helpers.

## Common Commands (run from `m_nemosyne_m/`)
- `cargo check` — Fast type/borrow check.
- `cargo test` — Run unit & integration tests.
- `cargo clippy` — Lint check.

## Workspace Notes
- SQLite databases (`mnemosynem*.db` and `.bak` files) live one level up in `/home/mykhailo/dev/pet/MnemosyneM/`.
- Do not scan or modify `target/` or `.fastembed_cache/`.
- Keep `AGENTS.md` concise; update the module map if you add or rename top-level modules.
