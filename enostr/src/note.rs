#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct NoteId([u8; 32]);

impl NoteId {
    pub fn new(bytes: [u8; 32]) -> Self {
        NoteId(bytes)
    }

    pub fn bytes(&self) -> &[u8; 32] {
        &self.0
    }
}
