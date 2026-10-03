/// A message between features. Delivered at the end of the frame it was published in.
#[derive(Clone, Debug)]
pub struct Event {
    pub topic: String,
    /// Id of the publishing feature, or `kernel`.
    pub source: String,
    pub payload: serde_json::Value,
}
