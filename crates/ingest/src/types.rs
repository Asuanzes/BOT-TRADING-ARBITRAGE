use serde_json::Value;

#[derive(Clone)]
pub struct RawEvent {
    pub feed: &'static str,
    pub stream: String,
    pub ts_recv_ns: i64,
    pub payload: Value,
}
