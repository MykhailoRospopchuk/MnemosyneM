use crate::database::entity::media_type::MediaType;

pub fn parse_media_filter(query: &str) -> (Option<MediaType>, String) {
    let mut media_filter = None;
    let mut clean_query: Vec<&str> = Vec::new();

    for word in query.split_whitespace() {
        match word.to_lowercase().as_str() {
            "!gif" | "!гіф" => media_filter = Some(MediaType::Animation),
            "!img" | "!pic" => media_filter = Some(MediaType::Photo),
            _ => clean_query.push(word),
        }
    }

    (media_filter, clean_query.join(" "))
}
