# 🔬 MnemosyneM Semantic Meme Search Research

This directory contains an engineering investigation—codebase audit, empirical database analysis, ablation benchmarks, and architectural solution evaluation—addressing retrieval quality issues on short queries (1–4 words), visual scene descriptions, and conceptual paraphrases.

---

## 🎯 Problem Statement

The current retrieval pipeline captions each meme via a VLM (Gemini), gets text fields (`ocr_text`, `description_uk`, `description_en`), concatenates `ocr_text | description_*`, and computes **a single dense embedding** via local `intfloat/multilingual-e5-base`.

In practice, this causes two visible failure modes:
1. **Garbage in Top-K results:** Querying `"шоколадки на касі"` (*"chocolate bars at the checkout"*) returns two duplicate clones of the Matrix meme, immediately followed by completely unrelated memes (`"Смажу курку..."` / *frying chicken*, `"Твої 12 карієсів..."` / *12 cavities*, `"Свінабони"` / *pork rolls*).
2. **Degradation on abstract & visual queries:** Short 2–4 word queries (`"важко вранці відкрити очі"` / *hard to open eyes in the morning*, `"пішов качатися в зал"` / *went to lift at the gym*, or pure synonyms without literal text overlap) get drowned out in 50–100+ word descriptions and lose to random memes.

---

## 📋 TL;DR: Key Findings from Data & Experiments

Auditing `mnemosynem_v2.db` (**1,237 memes**) and running experiments on `MultilingualE5Base` exposed **5 concrete root causes**:

1. **Critical `w.len() >= 3` + `.contains()` bug in OCR lexical boost ([`meme_store.rs`](file:///home/mykhailo/dev/pet/MnemosyneM/m_nemosyne_m/src/meme_store.rs#L694-L724)):**
   In Rust, `str::len()` counts **UTF-8 bytes**, not Unicode characters. A 2-letter Cyrillic preposition like `"на"` (*"on/at"*) is **4 bytes** (`>= 3`) and is matched as an arbitrary substring in `ocr_text` with no word boundaries. On `"шоколадки на касі"`, `"на"` granted a spurious `+0.0133` boost to `"Піти на безкоштовні курси..."` (`Смажу курку`), `"...в оплату частиНАми"` (`12 карієсів`), and `"СвіНАбони"`. Similarly, `"очі"` (*eyes*) substring-matched inside `"ВНОЧІ"` (*at night*).
2. **Dead `best_score > 0.05` threshold + E5 cosine anisotropy:**
   In `multilingual-e5-base`, random meme pairs have **`mean = 0.8175`, `median = 0.8186`**, and complete nonsense like `"квантова механіка синхрофазотрону"` (*quantum mechanics of a synchrophasotron*) scores **`max = 0.8145`**. Because `> 0.05` passes **100% of the database**, whenever only 1–2 relevant memes exist, unrelated noise is forced into the remaining `TOP_K` slots.
3. **Mean Pooling dilution in long descriptions:**
   Average `ocr_text | description_uk` length is **66.7 words** (median **62**, max **231**); **54.1%** end with a `"Ключові слова: ..."` (*Keywords*) block, **33.9%** start with framing boilerplate (`"Мем складається з..."` / *The meme consists of...*), and **14.4%** of `ocr_text` contains Gemini Markdown labels (`*Top text:*`, `* Left side:`). When the punchline sits in a single sentence near the end (e.g., the `"мої повіки"` barbell meme for `"важко вранці відкрити очі"`), the full passage collapses to **rank #53** (`0.7971`), whereas embedding that individual sentence boosts it to **rank #1 (`0.8552`)**.
4. **Unmerged `phash` duplicates:**
   `mnemosynem_v2.db` contains **26 exact duplicate `phash` entries** (including two clones of the Matrix chocolate meme sharing `phash = -3403679589597394704`), eating up multiple slots in a small `TOP_K`.
5. **Limits of pure bi-encoders and naive sentence splitting:**
   * Pure **BM25** hits **100% (`10/10`)** when lexical/morphological overlap exists, but drops to **0% (`0/4`)** on pure synonyms with zero overlapping words.
   * Naive multi-vector `max(sent_i)` solves single-sentence dilution (`10/10 Hit@3` on standard queries), yet without global context or reranking it suffers from **multiple-testing noise inflation** (dropping pure-synonym rank down to `#17` and raising negative-control noise from `0.8065` to `0.8199`).
   * **Optimal architecture for our Rust stack:** **Hybrid retrieval (Vector + SQLite FTS5/BM25)** ➔ **`phash` deduplication** ➔ **Cross-Encoder Reranking (`fastembed::TextRerank`, e.g., `JINARerankerV2BaseMultiligual` or `BGERerankerV2M3`)** over Top-15–20 candidates with calibrated score thresholding.

---

## 🗂️ Contents

| File | Description |
| :--- | :--- |
| [01-current-pipeline-audit.md](file:///home/mykhailo/dev/pet/MnemosyneM/m_nemosyne_m/docs/search-research/01-current-pipeline-audit.md) | Detailed code audit (`engine.rs`, `meme_store.rs`, `vlm.rs`), corpus statistics on `mnemosynem_v2.db`, and forensic breakdown of the screenshot failure. |
| [02-empirical-experiments.md](file:///home/mykhailo/dev/pet/MnemosyneM/m_nemosyne_m/docs/search-research/02-empirical-experiments.md) | Methodology and measurements across 4 experiments on 1,237 memes: passage dilution, BM25 vs RRF vs Z-score, sentence-level multi-vectors, zero-overlap synonyms, and negative controls. |
| [03-solution-architecture.md](file:///home/mykhailo/dev/pet/MnemosyneM/m_nemosyne_m/docs/search-research/03-solution-architecture.md) | Engineering solution space (`fastembed 7.1.0` capabilities, FTS5, Cross-Encoder reranking, BGE-M3, visual embeddings, LLM query rewriting) and phased rollout plan. |

---

## ⚖️ Solution Comparison Matrix

| Approach | Exact / Scene Queries | Pure Synonyms (Zero Word Overlap) | Noise / Negative Rejection | Implementation Cost | DB Re-indexing |
| :--- | :---: | :---: | :---: | :--- | :--- |
| **Current baseline** | Medium (harmed by OCR bug) | Low (`25% Hit@3`) | ❌ None (`> 0.05`) | — | — |
| **Phase 0: Fix OCR + Stopwords + pHash dedup + Gap Cutoff + VLM Prompt** | High | Medium | Basic (gap + floor) | Very low (hours) | New memes only (optional backfill) |
| **Phase 1A: Hybrid (E5 Vector + SQLite FTS5/BM25)** | High (`~100%`) | Needs calibration (avoid equal RRF!) | Medium | Low–Medium | No (FTS5 built from existing columns) |
| **Phase 1B: Hybrid Top-20 + Cross-Encoder Rerank (`TextRerank`)** | Very High | High (token-to-token attention) | ✅ High (calibrated logits) | Medium (1.1–2.2 GB RAM + CPU latency) | Optional (clean VLM prompt helps) |
| **Phase 1B (Alt): Hybrid Top-15 + `gemini-decision-1` Cloud Judge** | Very High | High | ✅ High (`noul`/`score` 0.01–0.99) | Low (**0 GB extra RAM**, ~150–250 ms RTT, `10K RPD / 200 RPM`) | No (uses existing descriptions) |
| **Phase 2: Multi-Vector (`holistic + scene + meaning`) + Rerank / `BGEM3Q`** | Maximum | High | ✅ High | High (schema migration) | Yes (VLM + embeddings) |
| **Visual Embeddings (`CLIP` / `SigLIP`)** | Medium (misses meme captions) | Low for text-heavy memes | Low | High (`fastembed` CLIP is EN-only) | Yes |
| **LLM Query Rewriting (Gemini per search)** | High | High | Medium | High (latency + RPM quota in inline) | No |

---

## ✅ Implementation Progress (Audited Nov 18, 2026)

* **Phase 0 is implemented and covered by `cargo test` (13 passing tests):**
  * Fixed lexical boost in [`lexical_helper.rs`](file:///home/mykhailo/dev/pet/MnemosyneM/m_nemosyne_m/src/helpers/lexical_helper.rs) + [`find_top_k`](file:///home/mykhailo/dev/pet/MnemosyneM/m_nemosyne_m/src/meme_store.rs#L795-L853) (`chars().count() >= 3`, stopwords, light Ukrainian stemming, non-alphanumeric tokenization).
  * Implemented search-time `phash` deduplication ([`deduplicate_by_phash`](file:///home/mykhailo/dev/pet/MnemosyneM/m_nemosyne_m/src/meme_store.rs#L855-L884)), [`select_relative`](file:///home/mykhailo/dev/pet/MnemosyneM/m_nemosyne_m/src/meme_store.rs#L886-L902), and [`weak_results`](file:///home/mykhailo/dev/pet/MnemosyneM/m_nemosyne_m/src/meme_store.rs#L904-L924).
  * Shipped tightened `VLM_PROMPT`, `sanitize_analysis`, `vlm_version = 2` migration, and background `reprocess_vlm` worker ([`maintance_worker.rs`](file:///home/mykhailo/dev/pet/MnemosyneM/m_nemosyne_m/src/maintenance/maintance_worker.rs)): on `mnemosynem_v2.db` (`N = 1,390`), **621 rows** already migrated to `v2` (dropping avg length **`64.4 -> 44.2` words** and fixing **72.0%** legacy `v1` rows whose `description_uk` had been generated in English).
* **Remaining gaps:** lexical matching still scans only `ocr_text` (not `description_uk`, `description_en`, or `title` — requires **Phase 1A FTS5**), and Stage-2 Reranking (**Phase 1B**, either local `TextRerank` or `gemini-decision-1`) is not implemented yet.
