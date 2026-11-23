# 💡 MnemosyneM — Feature Ideas & Backlog

This document captures ideas, product concepts, and future extensions for **MnemosyneM**, organized by priority, implementation complexity, and user engagement impact (Retention & Virality).

---

## 🏆 Top-Priority Features (Near-Term Focus)

### 1. 🎲 Meme Randomizer / "Dopamine Subscription" (Meme Drip Delivery)
* **Concept:** The user configures a subscription to receive random memes over $N$ days at a rate of $K$ memes per day at **unpredictable random times**.
* **Why it works:**
  * Fixed-interval timers (every 30 min) feel like spam. A random meme popping up mid-day feels like a dopamine surprise or an unexpected text from a friend.
* **Key UX details:**
  * **Quiet Hours:** Suppress delivery overnight (default 23:00 – 09:00).
  * **Jitter/Poisson scheduling:** Next delivery timestamp generated as $\text{now} + \text{random}(\text{min}, \text{max})$; if it falls inside quiet hours, shift to morning.
  * **Delivery history tracking:** Persist sent memes in `user_delivered_memes` so the same user never receives duplicates.
* **Rust Architecture:**
  * SQLite schema:
    ```sql
    CREATE TABLE meme_subscriptions (
        user_id BIGINT PRIMARY KEY,
        active_until BIGINT NOT NULL,
        memes_per_day INTEGER NOT NULL DEFAULT 3,
        next_delivery_at BIGINT NOT NULL,
        is_active BOOLEAN NOT NULL DEFAULT 1
    );
    CREATE TABLE user_delivered_memes (
        user_id BIGINT NOT NULL,
        meme_id INTEGER NOT NULL,
        delivered_at BIGINT NOT NULL,
        PRIMARY KEY (user_id, meme_id)
    );
    ```
  * Background `tokio::spawn` loop polling `meme_subscriptions` every 1–2 minutes and calling `bot.send_photo`.

### 2. 🔥 Native Telegram Meme Tinder (`/tinder`) — Inline-Button Swiper
* **Why moved out of Mini App into the native bot:**
  * Building a Telegram Mini App (TMA) brings heavy frontend overhead (web stack, hosting, JS frameworks) that distracts from core Rust backend development.
  * Implementing it directly inside Telegram chat using photo messages and Inline buttons satisfies 100% of the quick-rating workflow with zero extra stack.
* **UI/UX & Button Layout:**
  * Command: `/tinder` (or main menu button).
  * The bot sends a meme photo with three inline buttons arranged in **a single row**:
    * **Wider equal-width primary buttons on the sides:** `[ ❤️ Like ]` (left) and `[ ⏭️ Skip ]` (right).
    * **Compact smaller button in the center:** `[ 👎 ]` (dislike) — icon-only to occupy minimal width between the primary actions:
      ```text
      +-------------------+-------+-------------------+
      |     ❤️ Like       |  👎   |      ⏭️ Skip      |
      +-------------------+-------+-------------------+
      ```
* **Three-Way Reaction System:**
  * 👍 **Like:** Adds the meme to a separate **Likes Feed (`❤️ Liked` / `/liked`)**.
    * *Core UX rule:* Liking a meme **MUST NOT force-add** it into the user's personal working collection (`user_memes`), keeping personal inline search clean instead of polluting it with hundreds of merely liked memes.
  * ⏭️ **Skip:** Neutral skip without rating (advances to next meme).
  * 👎 **Dislike:** Hides the meme permanently from that user and lowers the meme's global ranking.
* **Strict Separation: "My Collection" vs "Liked":**
  * **"My Collection" (`user_memes`):** Memes explicitly saved for fast lookup/search via `@bot`. These can be purely utilitarian/work memes that don't necessarily carry a "like".
  * **"Liked" (`user_reactions WHERE reaction = 'like'`):** A dedicated favorites feed. From there, a user can optionally tap `[ 📥 Save to My Memes ]` if they also want it in personal search.
* **Seamless Updates (Zero Chat Spam):**
  * Tapping any reaction button edits the existing message in place via `edit_message_media`, swapping in the next photo and updated `meme_id` callback data. Zero chat clutter.
* **Database Schema:**
  ```sql
  CREATE TABLE IF NOT EXISTS user_reactions (
      user_id BIGINT NOT NULL,
      meme_id BIGINT NOT NULL,
      reaction TEXT NOT NULL CHECK (reaction IN ('like', 'dislike', 'skip')),
      created_at BIGINT NOT NULL,
      PRIMARY KEY (user_id, meme_id)
  );
  CREATE INDEX IF NOT EXISTS idx_user_reactions_lookup 
      ON user_reactions(user_id, reaction);
  ```

---

## 👥 Social Features & Virality (Virality & Growth)

### 3. 🎭 "Meme for That" / Group Chat Reaction Mode
* **Concept:** The bot is added to a group chat (work or friends). Users reply to any message with `@mnemosyne_bot meme` or a trigger reply.
* **Under the hood:** The bot extracts the quoted message text, runs semantic search for the sharpest / most relevant reaction meme, and replies directly in thread.
* **Impact:** One active user introduces the bot to an entire 20–50 person group chat.

### 4. 📦 Personal Sticker Pack Export (`/make_stickers`)
* **Concept:** On command, the bot builds an official Telegram sticker pack generated from the user's saved collection (`user_memes`).
* **Under the hood:** Uses Telegram Bot API `createNewStickerSet` / `addStickerToSet`.
* **Impact:** Users can send their saved memes in any chat as native stickers.

---

## 🧠 AI Features & Smart Search (Smart Search & VLM)

### 5. 📸 Vibe & Mood Search (Reaction by Selfie / Image)
* **Concept:** User sends a selfie or textless reaction photo to the bot.
* **Under the hood:** VLM describes the emotion and scene mood (e.g., *"exhausted person staring at laptop on Monday morning"*), embeds that text, and retrieves memes with matching vibes.

### 6. 🧩 Meme Template Families & Clustering
* **Concept:** Links variants built on the same meme format (e.g., *"Distracted Boyfriend"*, *"Drake"*, *"Thinking Monkey"*).
* **UX:** Button under a meme: *"🔍 6 more variants of this template"*.
* **Under the hood:** Clusters by a VLM-extracted `template_name` field or visual similarity metrics.

### 7. 🏷️ Auto-Generated Topic Tags & Category Browser (`#work`, `#relationships`, `#it`)
* **Concept:** During VLM indexing, automatically extract 2–4 normalized topical tags/categories per meme.
* **UX:**
  * **`/tags` command (Tag Directory with Counts):** Displays every tag in the catalog alongside meme counts:
    ```text
    🏷️ Tags & Categories Directory:
    • #work (42)
    • #it (38)
    • #relationships (25)
    • #cats (19)
    • #deadline (14)
    • #relatable (12)
    ```
  * **One-Tap Inline Launch:** Inline buttons (`SwitchInlineQueryCurrentChat`) beneath the directory launch instant search pre-filled with the selected tag.
  * **Composable Search Filters:** Support combined inline queries like `@mnemosyne_bot #work deadline` or filtering personal saved memes by category.
* **Under the hood:**
  * Relational tables `tags (id, name UNIQUE)` and `meme_tags (meme_id, tag_id)`.
  * Fast aggregation query:
    ```sql
    SELECT t.name, COUNT(mt.meme_id) AS total_memes
    FROM tags t
    JOIN meme_tags mt ON t.id = mt.tag_id
    GROUP BY t.id
    ORDER BY total_memes DESC;
    ```

### 8. 🎬 Semantic Search & Indexing for GIF / MP4 Animations *(✅ Shipped in v0.2)*
* **Concept:** First-class support for animated memes (GIFs) on par with static image memes.
* **Why it matters:**
  * Standard search engines (Tenor, Giphy) rely on crude manual filename tags.
  * MnemosyneM indexes **scene dynamics**: Gemini VLM natively processes MP4 video frames, recognizing character actions over time, emotional shifts, and subtitles appearing mid-clip.
* **Architectural highlights (Zero-Cost & Pure Rust):**
  * **Telegram Animation Object:** Telegram stores GIFs as silent H.264 MP4s. We download the clip and send it directly to Gemini (`inline_data` with `mime_type: "video/mp4"`).
  * **No heavy FFmpeg dependency:** Zero C/video library bloat on the server—Telegram and Gemini handle decoding.
  * **Instant pHash:** Perceptual hash is computed directly from the static JPEG `animation.thumbnail` Telegram provides for free.
  * **Zero-bandwidth Inline Delivery:** Results return `InlineQueryResultCachedMpeg4Gif` using cached Telegram `file_id`s (0 bytes server egress, 0 local media storage).

---

### 9. 🧹 Perceptual Duplicate Detector (Visual Hashes - pHash) *(✅ Shipped in v0.2)*
* **Concept:** Detects identical memes even across recompression, resolution changes, or minor crops.
* **Under the hood:** Uses `image_hasher` (64-bit gradient hash) in Rust, computing Hamming distance (`POPCNT`) in microseconds and skipping redundant Gemini VLM calls. Backfills legacy rows via autonomous worker [`src/maintenance/maintance_worker.rs`](file:///home/mykhailo/dev/pet/MnemosyneM/m_nemosyne_m/src/maintenance/maintance_worker.rs).

---

## 🎨 Generation & Interactive Features (Meme Studio)

### 10. ✏️ On-the-Fly Meme Generator (Meme Remix / Meme Canvas)
* **Concept:** Create custom captioned memes directly inside bot chat from existing templates.
* **Command:** `/remix template | top text | bottom text`.
* **Under the hood:** Pure Rust rendering (`imageproc` + `ab_glyph`) overlaying classic white Impact font with black outline in milliseconds without paid external APIs.

### 11. 🎯 Daily Meme Quiz / Trivia
* **Concept:** Daily game in private chat or channels: a meme with its punchline blurred out and a 4-option poll to guess the caption.
* **Impact:** High retention and daily habit formation.

---

## 📱 Visual Web Interface (Future Phase)

### 12. 🖼️ Telegram Mini App (TMA): Masonry Grid Pinterest (Future Phase)
* **Concept:** A full web interface launched inside Telegram if rich visual browsing becomes worthwhile later.
  * **Masonry Grid (Pinterest):** Infinite-scroll gallery view across thousands of memes.
  * **Subscription Controls:** Visual sliders to configure "Dopamine Subscription" cadence and quiet hours.
  * *(Note: Meme Tinder swiping was moved to the native bot UI—see item 2—because building a TMA requires maintaining a separate web stack).*

---

## 🧊 Low-Priority / Deferred Ideas

### 13. 🕷️ Autonomous Public Channel Crawler (Channel Crawler) *(Deferred)*
* **Why deferred:** MnemosyneM's core value is a human-curated catalog of funny memes submitted directly by users, rather than bulk-scraping noisy public channels. Revisit only if automated catalog expansion becomes necessary.
* **Mechanism (`t.me/s/` Web Mirror):**
  * Telegram exposes public channel feeds at `https://t.me/s/<channel_username>`.
  * A background Rust worker (`reqwest` + `scraper`) parses recent posts and downloads new images.
  * Fresh media enters the existing `QueueWorker` ingestion pipeline with provenance (`source_title`, `source_url`).
