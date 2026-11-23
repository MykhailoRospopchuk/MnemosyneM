use crate::database::entity::media_type::MediaType;
use crate::tagger::normalize_tag_alias;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedSearchQuery {
    pub media_filter: Option<MediaType>,
    pub tag_filters: Vec<String>,
    pub clean_query: String,
}

pub fn parse_search_query(query: &str) -> ParsedSearchQuery {
    let mut media_filter = None;
    let mut tag_filters: Vec<String> = Vec::new();
    let mut raw_words: Vec<&str> = Vec::new();

    for word in query.split_whitespace() {
        let lower = word.to_lowercase();
        match lower.as_str() {
            "!gif" | "!гіф" => media_filter = Some(MediaType::Animation),
            "!img" | "!pic" => media_filter = Some(MediaType::Photo),
            _ if word.starts_with('#') => {
                let raw_tag = word.trim_end_matches([',', '.', ';', ':', '!', '?']);
                let normalized = normalize_tag_alias(raw_tag);
                if !normalized.is_empty() && !tag_filters.contains(&normalized) {
                    tag_filters.push(normalized);
                }
            }
            _ => raw_words.push(word),
        }
    }

    let clean_words: Vec<&str> = if tag_filters.is_empty() {
        raw_words
    } else {
        raw_words.into_iter().filter(|&w| w != "+").collect()
    };

    ParsedSearchQuery {
        media_filter,
        tag_filters,
        clean_query: clean_words.join(" "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_search_query_handles_multi_tags_media_and_plus_separator() {
        assert_eq!(
            parse_search_query("#мавпи + #іт"),
            ParsedSearchQuery {
                media_filter: None,
                tag_filters: vec!["мавпи".to_string(), "айті".to_string()],
                clean_query: String::new(),
            }
        );

        assert_eq!(
            parse_search_query("#work #burnout deadline"),
            ParsedSearchQuery {
                media_filter: None,
                tag_filters: vec!["робота".to_string(), "вигорання".to_string()],
                clean_query: "deadline".to_string(),
            }
        );

        assert_eq!(
            parse_search_query("#іт #айті баг"),
            ParsedSearchQuery {
                media_filter: None,
                tag_filters: vec!["айті".to_string()],
                clean_query: "баг".to_string(),
            }
        );

        assert_eq!(
            parse_search_query("!gif #коти #сон"),
            ParsedSearchQuery {
                media_filter: Some(MediaType::Animation),
                tag_filters: vec!["коти".to_string(), "сон".to_string()],
                clean_query: String::new(),
            }
        );

        assert_eq!(
            parse_search_query("C++ + Rust"),
            ParsedSearchQuery {
                media_filter: None,
                tag_filters: Vec::new(),
                clean_query: "C++ + Rust".to_string(),
            }
        );
    }
}
