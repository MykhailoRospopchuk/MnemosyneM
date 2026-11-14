# 1. Current Search Pipeline Audit & Database Diagnostics

This document captures the actual technical state of MnemosyneM's search pipeline as of **November 2026**, grounded in source code inspection (`src/`) and read-only measurements on the live SQLite database `../mnemosynem_v2.db` (**1,237 memes**).

---

## ⚙️ Current Indexing & Retrieval Flow

### 1.1. VLM Caption Generation
In [`describe_meme`](file:///home/mykhailo/dev/pet/MnemosyneM/m_nemosyne_m/src/vlm.rs#L35-L197), images are sent to the Gemini API (`GEMINNI_MODEL_POOL`: `gemini-3.5-flash-lite`, `gemini-3.1-flash-lite`, `gemini-3.5-flash`, etc.) with the following prompt ([`vlm.rs#L65-L70`](file:///home/mykhailo/dev/pet/MnemosyneM/m_nemosyne_m/src/vlm.rs#L65-L70)):

```text
Extract all text from this meme verbatim. Then briefly describe the scene, characters,
and the joke context in both Ukrainian and English so it can be easily found by keywords.
JSON fields to return:
- 'is_nsfw': ...
- 'ocr_text': exact verbatim text written on the meme (empty string if none)
- 'uk_description': describe the scene, characters, joke context, and key search keywords in Ukrainian.
- 'en_description': meme template name, scene description, and key search keywords in English.
```

> [!WARNING]
> The prompt enforces **no length limit**, **no ban on Markdown layout annotations in `ocr_text`**, and **no structural separation** between visual scene, joke meaning, and tags. As a result, Gemini produces long multi-sentence prose mixed with formatting prefixes and trailing keyword lists.

### 1.2. Embedding Construction
In [`MemeStore::add_from_analysis`](file:///home/mykhailo/dev/pet/MnemosyneM/m_nemosyne_m/src/meme_store.rs#L66-L86), fields are concatenated verbatim with `" | "`:

```rust
let passage_uk = format!(
    "{} | {}",
    new_meme.analysis.ocr_text, new_meme.analysis.uk_description
);
let passage_en = format!(
    "{} | {}",
    new_meme.analysis.ocr_text, new_meme.analysis.en_description
);
```

[`ClipEngine`](file:///home/mykhailo/dev/pet/MnemosyneM/m_nemosyne_m/src/engine.rs#L16-L28) (despite its legacy name, it wraps `EmbeddingModel::MultilingualE5Base`—see earlier CLIP/Nomic artifacts still present in `.fastembed_cache`) prepends `"passage: {} "` / `"query: {} "` and produces **one 768-dim vector** for UK and **one** for EN (`model_max_length: 512`, `XLMRobertaModel`, `vocab_size: 250002`, Mean Pooling).

### 1.3. Search & Ranking
Queries from [`handle_search`](file:///home/mykhailo/dev/pet/MnemosyneM/m_nemosyne_m/src/bot/handlers.rs#L155) and [`handle_inline_query`](file:///home/mykhailo/dev/pet/MnemosyneM/m_nemosyne_m/src/bot/handlers.rs#L274) (500 ms debounce, minimum 4 chars/bytes check) route to [`MemeStore::search_top_k`](file:///home/mykhailo/dev/pet/MnemosyneM/m_nemosyne_m/src/meme_store.rs#L531) ➔ [`find_top_k`](file:///home/mykhailo/dev/pet/MnemosyneM/m_nemosyne_m/src/meme_store.rs#L684-L732) with default `top_k = 3` in [`config.rs`](file:///home/mykhailo/dev/pet/MnemosyneM/m_nemosyne_m/src/config.rs#L4-L37) (overridable via `TOP_K` env var):

1. Computes `best_score = f32::max(sim_uk, sim_en)` (cosine similarity).
2. Applies an OCR lexical boost:
   ```rust
   let query_words: Vec<&str> = query_lower
       .split_whitespace()
       .filter(|w| w.len() >= 3)
       .collect();
   // ...
   let matches = query_words.iter().filter(|&&w| ocr_lower.contains(w)).count();
   if matches > 0 {
       let boost = 0.04 * (matches as f32 / query_words.len() as f32);
       best_score = (best_score + boost).min(1.0);
   }
   ```
3. Drops only items with `best_score <= 0.05`, sorts descending, and truncates to `k`.

---

## 📊 Real Corpus Anatomy (`mnemosynem_v2.db`, `N = 1,237`)

Read-only inspection of `mnemosynem_v2.db` yields the following actual distributions:

| Metric | Observed Value |
| :--- | :--- |
| Total memes in `mnemosynem_v2.db` | **1,237** (`mnemosynem_v3.db` has **432**) |
| `ocr_text` length (words) | `min = 0`, `median = 10`, `avg = 12.7`, `max = 137` (`53` empty) |
| `description_uk` length (words) | `min = 1`, `median = 50`, `avg = 53.0`, `max = 137` |
| `description_uk` length (chars) | `min = 2`, `median = 347`, `avg = 373.3`, `max = 916` |
| **Combined `passage_uk` (`ocr \| desc_uk`) in words** | `min = 10`, **`median = 62`**, **`avg = 66.7`**, `max = 231` |
| Combined passages longer than 100 / 150 / 200 words | **108** (>100), **9** (>150), **1** (>200) |
| `description_en` length (words) | `min = 0`, `median = 57`, `avg = 62.1`, `max = 159` |
| **`ocr_text` polluted with Gemini Markdown/layout tags** (`*Top text:*`, `*`, `Left/Right`) | **178 (14.4%)** |
| **`description_uk` ending with `"Ключові слова: ..."` (*Keywords*)** | **669 (54.1%)** |
| **`description_uk` with framing boilerplate** (`"Мем складається..."`, `"На зображенні..."`) | **419 (33.9%)** |
| Exact duplicate `phash` entries | **26** rows sharing a duplicate `phash` |
| Non-empty `title` rows | **600**, frequently Telegram channel spam (e.g., `'💀TG \| X \| FB \| Insta'`), and `title` is **completely unindexed** during search |

---

## 🔍 Five Empirically Proven Root Causes

### Root Cause 1: Critical Bug in `ocr_text` Boost (`w.len() >= 3` Bytes + `.contains()` Substring Match)

Inside [`find_top_k`](file:///home/mykhailo/dev/pet/MnemosyneM/m_nemosyne_m/src/meme_store.rs#L694-L724):
* Cyrillic characters take **2 bytes each in UTF-8**. Thus `w.len() >= 3` lets **every 2-letter Ukrainian preposition pass** (2 Cyrillic letters = 4 UTF-8 bytes `>= 3`): `"на"`, `"до"`, `"за"`, `"по"`, `"від"`, etc.!
* `.contains(w)` performs an **unbounded substring check inside arbitrary words** rather than whole-token matching.

#### 🔎 Forensics of the `"шоколадки на касі"` Screenshot
The query splits into `["шоколадки", "на", "касі"]` (3 words kept because `"на"` is 4 bytes!). Since `"касі"` (*checkout, locative sg.*) doesn't match `"касах"` (*checkouts, locative pl.*), `"на"` triggers false boosts everywhere:

| Rank / Meme ID | Meme Content | Actual `ocr_text` Substring | Spurious Boost & Score |
| :--- | :--- | :--- | :--- |
| `#1` (`1793039502`) | Matrix meme (`шоколадки на касах`) | `* шоколадки на касах...` | `+0.0267` (`шоколадки`, `на`) ➔ `0.8804` |
| `#2` (`1793525020`) | **Exact pHash duplicate** of Matrix meme | `* Left side...: "шоколадки на касах"...` | `+0.0267` ➔ `0.8590` |
| `#3` (`1793447081`) | **"Смажу курку..."** (trolleybus depot job pitch) | `> Піти на безкоштовні курси...` | **`+0.0133`** from `"на"` ➔ **`0.8294`** |
| `#4` (`1794384121`) | **"Твої 12 карієсів..."** (iPhone on installments) | `...в оплату частиНАми` | **`+0.0133`** from `"НА"` inside `"частинами"` ➔ **`0.8267`** |
| `#5` (`1793044252`) | **"Свінабони"** (pork meat rolls pun) | `helena_uxui 9 год + СвіНАбони` | **`+0.0133`** from `"НА"` inside `"Свінабони"` ➔ **`0.8243`** |

> [!CAUTION]
> **The exact same bug hurts `"важко вранці відкрити очі"` (*hard to open eyes in the morning*):** `"очі"` (*eyes*, 6 UTF-8 bytes) matched inside `"ВНОЧІ"` / `"о третій ночі"` (*at night*), granting `+0.0133` to meme `1794519254` (`"Я ВНОЧІ: 02:00"`) and pushing down relevant meme `1794231396` in `S0_Baseline`.

---

### Root Cause 2: Narrow Dynamic Range (Anisotropy) in E5 + Dead `> 0.05` Threshold

`MultilingualE5Base` maps nearly all passages into a tight cone in vector space. Measuring 100 random pairs across `mnemosynem_v2.db`:
* **Random meme-to-meme `embedding_uk` cosine:** `min = 0.7202`, `p10 = 0.7904`, **`median = 0.8186`**, `p90 = 0.8430`, `max = 0.9483`, **`mean = 0.8175`**.
* **Query-to-corpus (`N = 1,237`) median:** hovers between **`0.7720 – 0.7932`**, while pure out-of-domain nonsense `"квантова механіка синхрофазотрону"` still scores **`max = 0.8145`** (`p95 = 0.7941`).

Consequently, [`if best_score > 0.05`](file:///home/mykhailo/dev/pet/MnemosyneM/m_nemosyne_m/src/meme_store.rs#L725) **never rejects anything** (100% of visible memes pass). When the database only contains 1 unique relevant meme, `find_top_k` pads the rest of `TOP_K` with random noise in the `0.815–0.829` band.

---

### Root Cause 3: Short-Query vs Long-Passage Mean Pooling Dilution

A user query typically has **2–4 words**, whereas the indexed `passage_uk` averages **66.7 words** (up to **231 words**).

Under Mean Pooling, a passage embedding is the L2-normalized average of hidden states across **every token** (`ocr_text + intro boilerplate + visual background + character descriptions + joke explanation + keywords`).
* If a user searches for a **specific detail or punchline** confined to 1 sentence out of 4–5, the remaining ~50 words pull the averaged vector away from that niche concept.
* Example — meme `1794574005`:
  * `ocr_text`: `"мої повіки\nЯ"` (*my eyelids / Me*)
  * `uk_description`: *"Мем із маленьким хлопчиком, який у спортзалі намагається присісти зі штангою і кричить від зусилля. На штанзі напис «мої повіки», а на хлопчику — «я». Гумор полягає в тому, наскільки важко вранці відкрити очі, коли повіки здаються такими ж важкими, як важка штанга. Ключові слова: тренування, штанга, важко, повіки, ранок, спортзал."*
  * For `"важко вранці відкрити очі"`, the full-passage cosine collapses to **`0.7817` / `0.7971` (rank #53 on the hard subset)**, whereas isolating sentence #3 jumps similarity to **`0.8552` (rank #1)**.

---

### Root Cause 4: Gemini Text Clutter in `ocr_text` and `description_uk`

1. In **14.4%** of rows, `ocr_text` embeds English spatial Markdown annotations inside Ukrainian text:
   `"* Left side (on the person bent over the motorcycle): \"шоколадки на касах\" * Right side (on the man standing): ..."` (`1793525020`).
   This injects irrelevant English scaffolding into `embedding_uk` and breaks exact phrase matching.
2. In **33.9%** of rows, descriptions begin with template prose like *"Мем складається з двох кадрів..."*, *"На зображенні показано..."*, *"Скріншот публікації користувача..."*. Shared boilerplate pulls unrelated Threads/social screenshots artificially closer together in cosine space.

---

### Root Cause 5: Unfiltered Duplicates & Missing Morphological Normalization

* **Duplicate `phash` collisions:** Memes `1793039502` and `1793525020` share identical `phash = -3403679589597394704` (same for `1793271784` & `1793359167` sharing `phash = -3107889894206827898`). Without search-time `phash` deduplication, clones occupy multiple `TOP_K` slots.
* **Zero inflection handling:** Ukrainian is heavily inflected (`"касі"` vs `"касах"`, `"клоді"` vs `"клод"`, `"звільнення"` vs `"звільнили"`). Plain `.contains()` fails inflections while producing false substring hits, and domain nouns like `"фінмоніторинг"` benefit heavily from IDF-weighted lexical matching (BM25 / FTS5).
