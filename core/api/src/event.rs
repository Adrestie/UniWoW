/// A message between features. Delivered at the end of the frame it was published in.
#[derive(Clone, Debug)]
pub struct Event {
    pub topic: String,
    /// Id of the publishing feature, or `kernel`.
    pub source: String,
    pub payload: serde_json::Value,
}

impl Event {
    /// Reads the payload into a type declared by the receiving feature.
    pub fn decode<T: serde::de::DeserializeOwned>(&self) -> Result<T, String> {
        T::deserialize(&self.payload).map_err(|e| format!("event '{}' from '{}': {e}", self.topic, self.source))
    }
}
