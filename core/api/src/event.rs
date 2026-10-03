/// A message between modules. Delivered at the end of the frame it was published in.
#[derive(Clone, Debug)]
pub struct Event {
    pub topic: String,
    /// The publisher: a module id, `kernel`, or `<module>#<name>` for a script run of that module
    /// or a module it hosts.
    pub source: String,
    pub payload: serde_json::Value,
}

impl Event {
    /// Reads the payload into a type declared by the receiving module.
    pub fn decode<T: serde::de::DeserializeOwned>(&self) -> Result<T, String> {
        T::deserialize(&self.payload).map_err(|e| format!("event '{}' from '{}': {e}", self.topic, self.source))
    }
}
