# 2. Empirical Benchmarks & Ablation Experiments (`mnemosynem_v2.db`)

All experiments were run locally against the **full `mnemosynem_v2.db` corpus (`N = 1,237` memes)** and a **hard-negative evaluation subset (`N = 157` and `N = 192` — the union of all ground-truth memes plus top-10 hard distractors from vector and BM25 search per query)** using the production encoder `intfloat/multilingual-e5-base` (`fastembed 7.1.0`).

---

## 🧪 Evaluation Query Suite

To avoid circular or overly optimistic benchmarks, queries were split into **3 distinct categories**:

1. **Standard Queries (10 queries)** — realistic Ukrainian search patterns:
   * *Exact / inflected OCR:* `"шоколадки на касі"`, `"фінмоніторинг"`.
   * *Conceptual joke paraphrase:* `"спокуса купити солодощі перед касою"`, `"важко вранці відкрити очі"`, `"ліміти в клоді закінчились код ревю"`.
   * *Visual scene without OCR words:* `"жінка в чорному латексі на мотоциклі озирається"`, `"біле кошеня за ноутбуком дивиться в код"`.
   * *Short topic / slang (2–4 words):* `"тренування в спортзалі"`, `"пішов качатися в зал"`, `"звільнення з роботи"`.
2. **Zero/Minimal Lexical-Overlap Synonyms (`syn_zero`, 4 queries)** — verified in SQLite to avoid matching keywords in target descriptions:
   * `"ласун витрачає заначку біля прилавку"` (*sweet-tooth spending stash at the counter*) ➔ targets Matrix checkout memes (`1793039502`, `1793525020`).
   * `"сонливий підйом доводиться підніматись"` (*sleepy wake-up forced to get up*) ➔ targets morning wake-up memes (`1794574005`, `1794231396`, `1794660377`).
   * `"безробітний після скорочення босом"` (*unemployed after layoff by boss*) ➔ targets layoff memes (`1794298939`, `1794386773`, `1794573822`).
   * `"нейронка заблокувалась схвалювати не дивлячись"` (*neural net blocked, approving blindly*) ➔ targets Claude code-review memes (`1793271784`, `1793359167`).
3. **Negative Controls (`NEGATIVE`, 2 queries)** — topics verified to have 0 matches in the DB (ideal output: **0 results**):
   * `"квантова механіка синхрофазотрону"` (*quantum mechanics of a synchrophasotron*)
   * `"фотосинтез хлоропластів у рослинах"` (*chloroplast photosynthesis in plants*)

---

## 🔬 Experiment 1: Passage Dilution Ablation

Measured raw `MultilingualE5Base` cosine similarity when indexing focused sub-segments instead of full `ocr_text | description_uk`:

| Meme ID | Query | `full` (`ocr \| desc`) | `first_sent` (1st sentence of desc) | Isolated Sentence #3 | Gain vs `full` |
| :--- | :--- | :---: | :---: | :---: | :---: |
| `1793039502` (Matrix) | `"жінка на мотоциклі матриця"` | `0.8260` | **`0.8628`** | — | **`+0.0368`** |
| `1794574005` (Barbell/eyelids) | `"тренування в спортзалі"` | `0.8032` | **`0.8274`** | — | **`+0.0242`** |
| `1794574005` (Barbell/eyelids) | `"важко вранці відкрити очі"` | `0.7817` (missed Top-7) | `0.8007` | **`0.8552`** (via `M3_Sent`) | **`+0.0735`** |

**Takeaway:** Compressing a passage from ~65 words down to the core 10–20 word clause increases E5 cosine similarity by **`+0.024 .. +0.074`**. In a space where the spread between top-1 and random noise is only `~0.04`, that represents dozens of rank positions.

---

## 🔬 Experiment 2: Full-Corpus Retrieval (`N = 1,237`) — Lexical Fixes, BM25, RRF & Z-Score

Strategies compared across **all 1,237 memes** (in-memory BM25 index build time: **`420.8 ms`**):
* **`S0_Baseline`:** production scoring (`max(emb_uk, emb_en)` + byte-length `.contains()` boost, no `phash` dedup).
* **`S1_CleanLex`:** fixed OCR boost (`chars().count() >= 3` + stopwords + tokenized stem match) + `phash` dedup.
* **`S2_BM25_Only`:** field-weighted BM25 (`k1=1.5, b=0.75`; weights `ocr_clean * 2.0`, `desc_body * 1.0`, `keywords * 1.2`, `desc_en * 0.6`) with light suffix stemming + `phash` dedup (`score > 0` only).
* **`S3_HybridRRF`:** Reciprocal Rank Fusion (`1/(60 + rank_vec) + 1/(60 + rank_bm25)` if `bm25 > 0`) + `phash` dedup.
* **`S4_HybridZ`:** calibrated Z-score combination (`0.75 * Z_vec + 0.25 * clamp(Z_bm25, 0..6)`) + `phash` dedup.

### Part 1A: 10 Standard Queries (`task-343` / `task-368`)

| Strategy | Hit@3 (strict target `1794574005`*) | MRR (`task-343`) | Hit@3 (with morning trio) | MRR (`task-368`) |
| :--- | :---: | :---: | :---: | :---: |
| **`S0_Baseline`** | `9/10 (90.0%)` | `0.8342` | `10/10 (100.0%)` | `0.9333` |
| **`S1_CleanLex`** | `9/10 (90.0%)` | `0.8509` | `10/10 (100.0%)` | `0.9500` |
| **`S2_BM25_Only`** | **`10/10 (100.0%)`** | **`1.0000`** | **`10/10 (100.0%)`** | **`1.0000`** |
| **`S3_HybridRRF`** | `9/10 (90.0%)` | `0.9091` | **`10/10 (100.0%)`** | **`1.0000`** |
| **`S4_HybridZ`** | `9/10 (90.0%)` | `0.9100` | **`10/10 (100.0%)`** | **`1.0000`** |

*\*Note: In `task-343`, only the barbell/eyelids meme `1794574005` was marked relevant for `"важко вранці відкрити очі"`—`S0_Baseline` missed it completely in Top-5, whereas `BM25` ranked `1794574005` at **#1 (`21.742`)**. In `task-368`, the literal wake-up memes `1794660377` and `1794231396` were also included as valid targets.*

> [!IMPORTANT]
> **Illustrative visual query (`"жінка в чорному латексі на мотоциклі озирається"`):**
> * `S0_Baseline` ranked a traffic-rules meme of a cyclist woman seen from behind (`1793695556`, `0.849`) at `#1`, a motorcycle dealer meme (`1794384545`, `0.845` boosted by `"на"`) at `#2`, and Matrix (`1793525020`) only at `#3`.
> * `S1_CleanLex` removed the spurious `"на"` boost, moving Matrix above the dealer meme (`#2`).
> * **`S3_HybridRRF` and `S4_HybridZ` promoted Matrix (`1793039502`) straight to `#1`.**

### Part 1B: 4 Zero-Overlap Synonym Queries (`syn_zero`, `N = 1,237`)

| Strategy | Hit@3 (`syn_zero`) | Hit@5 (`syn_zero`) | MRR (`syn_zero`) | Key Observation |
| :--- | :---: | :---: | :---: | :--- |
| **`S0_Baseline`** | **`1/4 (25.0%)`** | **`2/4 (50.0%)`** | **`0.2250`** | Dense vectors still catch weak semantic similarity (~0.82–0.84) |
| **`S2_BM25_Only`** | **`0/4 (0.0%)`** | **`0/4 (0.0%)`** | **`0.0000`** | **Total failure** — blind without shared word stems |
| **`S3_HybridRRF`** | `0/4 (0.0%)` | `1/4 (25.0%)` | `0.1110` | **Worse than pure vector!** Equal-weight RRF (`1/(60+r)`) lets noisy BM25 stem hits outrank #2 vector candidates |
| **`S4_HybridZ`** | `0/4 (0.0%)` | `2/4 (50.0%)` | `0.1236` | Mildly degraded by accidental stem collisions in BM25 |

> [!WARNING]
> **Critical lesson from Experiment 2:** Replacing vectors with BM25 breaks synonym retrieval (`0%`). Worse, **uncalibrated equal-weight RRF is harmful**: when BM25 matches incidental query words (e.g., general verbs/nouns on unrelated memes), `1/(60+rank)` overrides strong dense candidates. BM25 should feed a candidate pool (`Top-20 Vector ∪ Top-20 BM25`) for a reranker or receive IDF/coverage-gated weight rather than dominating dense semantics.

---

## 🔬 Experiment 3: Sentence-Level Multi-Vector vs Passage Cleanup (`Hard Subset N = 192`)

To test multi-vector splitting rigorously without lexical shortcuts, we re-embedded `1,684` vectors (`65.19 s` total = **`38.71 ms/vec`** on CPU) across all **192** hardest memes (positives + Top-10 hard distractors per query) under 4 pure-vector schemes:

* **`M0_CurConcat`:** current format `"{ocr} | {desc_uk}"`.
* **`M1_CleanConcat`:** strips Markdown from `ocr` and drops the trailing `"Ключові слова: ..."` block from `desc_uk`.
* **`M2_Split_2Vec`:** 2 vectors per meme (`ocr_clean`, `desc_body`), score = `max(cos_ocr, cos_desc)`.
* **`M3_Multi_Sent`:** multi-vector (`ocr_clean` + **every individual sentence** of `desc_body` + `keywords`), score = `max_i cos(q, v_i)`.

### Pure-Vector Results on Hard Subset (No Lexical Boost)

| Indexing Scheme | Hit@3 (10 std queries, `N=157`) | MRR (10 std) | Hit@3 (all 14 w/ `syn_zero`, `N=192`) | Hit@5 (all 14) | MRR (all 14) | `NEG` max noise |
| :--- | :---: | :---: | :---: | :---: | :---: | :---: |
| **`M0_CurConcat`** | `9/10 (90.0%)` | `0.8352` | **`12/14 (85.7%)`** | **`14/14 (100.0%)`** | **`0.7583`** | `0.8065 – 0.8145` |
| **`M1_CleanConcat`** | `9/10 (90.0%)` | `0.7860` | `11/14 (78.6%)` | `13/14 (92.9%)` | `0.7054` | — |
| **`M2_Split_2Vec`** | `7/10 (70.0%)` | `0.6771` | `9/14 (64.3%)` | `10/14 (71.4%)` | `0.6553` | — |
| **`M3_Multi_Sent`** | **`10/10 (100.0%)`** | **`0.9000`** | `11/14 (78.6%)` | `11/14 (78.6%)` | `0.6887` | ⬆️ **`0.8160 – 0.8199`** |

### Four Key Insights from Experiment 3

1. **Stripping `"Ключові слова"` (`M1_CleanConcat`) hurts pure-vector MRR (`0.7583 ➔ 0.7054`):**
   Despite prose bloat earlier in `description_uk`, trailing keyword lists act as compact topical anchors for E5. We should strip **framing templates (`"Мем складається з..."`) and Markdown labels**, not the keywords themselves.
2. **Naive 2-way `OCR` vs `Desc` split (`M2_Split_2Vec`) drops Hit@3 to `64.3%`:**
   Meme humor frequently arises from the **interaction between caption and image**. Detaching `ocr` (`"мої повіки Я"`) from the scene description broke context coherence (`"звільнення з роботи"` fell to `#7`; `"пішов качатися в зал"` fell to `#9`).
3. **`M3_Multi_Sent` wins on specific details, but loses on paraphrases ("Lucky Short Sentence Trap"):**
   * On `"важко вранці відкрити очі"`, meme `1794574005` jumped from **rank #53 (`0.7971`, margin `-0.0333`) to rank #1 (`0.8552`, margin `+0.0033`)**.
   * Across all 10 standard queries, `M3_Multi_Sent` hit **`10/10 (100.0%) Hit@3, MRR 0.9000`**.
   * **Yet on `syn_zero`**, `"нейронка заблокувалась схвалювати не дивлячись"` collapsed from **rank #2 (`M0`) / #1 (`M1`) down to #17 (`M3`)**, and `"сонливий підйом"` dropped to **#8**. Splitting 192 memes into ~1,300 short 5–12 word fragments triggers **multiple-testing noise inflation**: unrelated short sentences randomly spike to `0.8538`, raising the negative control ceiling (`0.8065 ➔ 0.8199`).
4. **Architectural takeaway:** Naive `max_i` over sentences must **not replace** the holistic context vector. Combine holistic + structural vectors (`0.5 * holistic + 0.5 * max_field`) or use a **Cross-Encoder Reranker**.

---

## 🔬 Experiment 4: Negative Rejection & Why Static Thresholds Fail

Observed full-corpus (`N = 1,237`) score distributions:

| Query | Type | `max` raw cos | Top-1 `Z-score` | `gap10` (`top1 - top10`) | `bm25_max` |
| :--- | :--- | :---: | :---: | :---: | :---: |
| `"шоколадки на касі"` | Exact OCR | `0.8537` (`0.874` w/ lex) | **`5.82`** | **`0.0427`** | `10.13` |
| `"тренування в спортзалі"` | Short topic | `0.8416` | **`5.33`** | **`0.0359`** | `14.10` |
| `"біле кошеня за ноутбуком..."` | Visual scene | `0.8835` | **`5.27`** | **`0.0457`** | `23.55` |
| `"ліміти в клоді закінчились..."` | Paraphrase+OCR | `0.8581` | **`5.34`** | **`0.0401`** | `30.80` |
| `"важко вранці відкрити очі"` | Deep meaning | `0.8325` | `3.00` | `0.0072` | `21.74` |
| `"пішов качатися в зал"` | Slang short | `0.8215` | `3.44` | `0.0138` | `10.71` |
| `"квантова механіка синхрофазотрону"` | **NEGATIVE** | **`0.8145`** | **`3.46`** | `0.0117` | `4.84` (stem collision) |
| `"фотосинтез хлоропластів у рослинах"` | **NEGATIVE** | **`0.8065`** | **`3.74`** | `0.0159` | **`0.00`** |

**Statistical conclusion:**
For $N = 1,237$ candidates, the extreme-value maximum of Gaussian noise reaches $\approx \sqrt{2 \ln N} \approx 3.77\sigma$. Therefore:
1. Total nonsense (`"фотосинтез..."`) attains `max = 0.8065` and **`z = 3.74`** — higher Z-score than the legitimate slang query `"пішов качатися в зал"` (`z = 3.44`).
2. **Neither a static cosine floor (`> 0.82`) nor bi-encoder Z-score alone reliably separates weak slang matches (`0.8215`) from the noise tail (`0.8145`).**
3. Clean negative rejection requires a **Cross-Encoder Reranker** (whose joint query-passage logit calibration centers cleanly on relevance probabilities instead of collapsing around `0.80`), optionally paired with sharp score-gap & lexical confirmation gates.
