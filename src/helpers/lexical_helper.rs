pub fn extract_stemmed_terms(text: &str) -> Vec<String> {
    let mut tokens: Vec<String> = text
        .to_lowercase()
        .replace(['\'', '’', 'ʼ', '`'], "")
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= 3 && !STOPWORDS.contains(t))
        .map(light_stem)
        .collect();

    tokens.sort();
    tokens.dedup();
    tokens
}

pub fn normalize_for_fts(text: &str) -> String {
    let tokens: Vec<String> = text
        .to_lowercase()
        .replace(['\'', '’', 'ʼ', '`'], "")
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= 3 && !STOPWORDS.contains(t))
        .map(light_stem)
        .collect();

    tokens.join(" ")
}

pub fn clean_title_for_fts(raw_title: &str) -> String {
    let target = ["@", "http:", "https:", "t.me/"];

    let tokens: String = raw_title
        .to_lowercase()
        .split_whitespace()
        .filter(|t| !target.iter().any(|prefix| t.starts_with(prefix)))
        .map(|f| f.to_string())
        .collect::<Vec<_>>()
        .join(" ");

    normalize_for_fts(&tokens)
}

pub fn build_fts_match_query(query: &str) -> Option<String> {
    let terms = extract_stemmed_terms(query);

    if terms.is_empty() {
        return None;
    }

    let query = terms
        .iter()
        .map(|t| format!("\"{}\"", t))
        .collect::<Vec<_>>()
        .join(" OR ");

    Some(query)
}

fn light_stem(token: &str) -> String {
    let chars: Vec<char> = token.chars().collect();
    if chars.len() <= 3 {
        return token.to_string();
    }

    const LONG_SUFFIXES: &[&str] = &[
        "ами", "ями", "ові", "еві", "ого", "ому", "ими", "іми", "ах", "ях", "ів", "їв", "ої", "ою",
        "ею", "єю", "ння", "ти", "ся", "ть", "ing", "ed", "es",
    ];
    for suffix in LONG_SUFFIXES {
        if let Some(stem) = token.strip_suffix(suffix)
            && stem.chars().count() >= 3
        {
            return stem.to_string();
        }
    }

    const SHORT_ENDINGS: &[char] = &['а', 'я', 'у', 'ю', 'і', 'ї', 'е', 'о', 'и', 'й', 's'];
    if chars.len() >= 4 && SHORT_ENDINGS.contains(chars.last().unwrap()) {
        return chars[..chars.len() - 1].iter().collect();
    }
    token.to_string()
}

const STOPWORDS: &[&str] = &[
    // UA
    "для",
    "про",
    "при",
    "під",
    "над",
    "без",
    "між",
    "через",
    "після",
    "перед",
    "коли",
    "хто",
    "кого",
    "кому",
    "ким",
    "що",
    "чого",
    "чому",
    "чим",
    "який",
    "яка",
    "яке",
    "які",
    "але",
    "або",
    "так",
    "вже",
    "ще",
    "тут",
    "там",
    "він",
    "вона",
    "воно",
    "вони",
    "його",
    "її",
    "їх",
    "мені",
    "тебе",
    "себе",
    "мій",
    "твій",
    "свій",
    "цей",
    "того",
    "той",
    "всі",
    "все",
    "був",
    "була",
    "було",
    "були",
    "буде",
    "нема",
    "ніж",
    "щоб",
    "аби",
    "ось",
    "коли",
    "тільки",
    "навіть",
    "просто",
    "дуже",
    // EN
    "the",
    "and",
    "for",
    "with",
    "that",
    "this",
    "from",
    "you",
    "your",
    "are",
    "was",
    "were",
    "not",
    "but",
    "can",
    "have",
    "has",
    "had",
    "just",
    "like",
    "what",
    "when",
    "how",
    "who",
    "all",
    "out",
];

#[cfg(test)]
mod lexical_helper_tests {
    use super::*;

    #[test]
    fn extract_stemmed_terms_test() {
        let tokens = extract_stemmed_terms("*п’ятниця* «клоді»");

        assert_eq!(extract_stemmed_terms("шоколадки на касі").len(), 2);
        assert!(tokens.contains(&"клод".to_string()));
        assert!(tokens.contains(&"пятниц".to_string()));
    }

    #[test]
    fn normalize_for_fts_preserves_term_frequency_and_stems() {
        let normalized = normalize_for_fts("шоколадки на касі, шоколадки!");
        assert_eq!(normalized, "шоколадк кас шоколадк");

        assert_eq!(normalize_for_fts("Я ВНОЧІ: 02:00"), "вноч");
    }

    #[test]
    fn clean_title_for_fts_strips_mentions_and_urls() {
        assert_eq!(
            clean_title_for_fts("Раночку @nepokazuy_mami https://t.me/test"),
            "раночк"
        );
        assert_eq!(clean_title_for_fts("@yesterdays_deadline t.me/abc"), "");
    }

    #[test]
    fn build_fts_match_query_quotes_terms_and_handles_empty() {
        assert_eq!(build_fts_match_query("на і до ???"), None);

        let q = build_fts_match_query("що це? C++ (NOT) *шоколадки* на касі").unwrap();
        assert_eq!(q, "\"кас\" OR \"шоколадк\"");
    }
}
