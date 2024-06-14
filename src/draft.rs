#[derive(Default)]
pub struct Draft {
    pub buffer: String,
}

impl Draft {
    pub fn new() -> Self {
        Draft::default()
    }
}
