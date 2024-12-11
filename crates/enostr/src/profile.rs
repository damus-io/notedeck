use serde_json::Value;

#[derive(Debug, Clone)]
pub struct Profile(Value);

impl Profile {
    pub fn new(value: Value) -> Profile {
        Profile(value)
    }

    pub fn name(&self) -> Option<&str> {
        self.0["name"].as_str()
    }

    pub fn display_name(&self) -> Option<&str> {
        self.0["display_name"].as_str()
    }

    pub fn lud06(&self) -> Option<&str> {
        self.0["lud06"].as_str()
    }

    pub fn lud16(&self) -> Option<&str> {
        self.0["lud16"].as_str()
    }

    pub fn about(&self) -> Option<&str> {
        self.0["about"].as_str()
    }

    pub fn picture(&self) -> Option<&str> {
        self.0["picture"].as_str()
    }

    pub fn website(&self) -> Option<&str> {
        self.0["website"].as_str()
    }
}
