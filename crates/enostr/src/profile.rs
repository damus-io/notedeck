use serde_json::{Map, Value};

#[derive(Debug, Clone, Default)]
pub struct ProfileState(Value);

impl ProfileState {
    pub fn new(value: Map<String, Value>) -> Self {
        Self(Value::Object(value))
    }

    pub fn get_str(&self, name: &str) -> Option<&str> {
        self.0.get(name).and_then(|v| v.as_str())
    }

    pub fn values_mut(&mut self) -> &mut Map<String, Value> {
        self.0.as_object_mut().unwrap()
    }

    /// Insert or overwrite an existing value with a string
    pub fn str_mut(&mut self, name: &str) -> &mut String {
        let val = self
            .values_mut()
            .entry(name)
            .or_insert(Value::String("".to_string()));

        // if its not a string, make it one. this will overrwrite
        // the old value, so be careful
        if !val.is_string() {
            *val = Value::String("".to_string());
        }

        match val {
            Value::String(s) => s,
            // SAFETY: we replace it above, so its impossible to be something
            // other than a string
            _ => panic!("impossible"),
        }
    }

    pub fn value(&self) -> &Value {
        &self.0
    }

    pub fn to_json(&self) -> String {
        // SAFETY: serializing a value should be irrefutable
        serde_json::to_string(self.value()).unwrap()
    }

    #[inline]
    pub fn name(&self) -> Option<&str> {
        self.get_str("name")
    }

    #[inline]
    pub fn banner(&self) -> Option<&str> {
        self.get_str("name")
    }

    #[inline]
    pub fn display_name(&self) -> Option<&str> {
        self.get_str("display_name")
    }

    #[inline]
    pub fn lud06(&self) -> Option<&str> {
        self.get_str("lud06")
    }

    #[inline]
    pub fn nip05(&self) -> Option<&str> {
        self.get_str("nip05")
    }

    #[inline]
    pub fn lud16(&self) -> Option<&str> {
        self.get_str("lud16")
    }

    #[inline]
    pub fn about(&self) -> Option<&str> {
        self.get_str("about")
    }

    #[inline]
    pub fn picture(&self) -> Option<&str> {
        self.get_str("picture")
    }

    #[inline]
    pub fn website(&self) -> Option<&str> {
        self.get_str("website")
    }

    pub fn from_note_contents(contents: &str) -> Self {
        let json = serde_json::from_str(contents);
        let data = if let Ok(Value::Object(data)) = json {
            data
        } else {
            Map::new()
        };

        Self::new(data)
    }
}
