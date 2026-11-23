#[derive(Debug)]
pub enum VlmError {
    InappropriateContent(String),
    DailyQuotaExhausted(String),
    Transient(String),
}
