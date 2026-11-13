#[derive(Debug)]
pub enum VlmError {
    InappropriateContent(String),
    Transient(String),
}
