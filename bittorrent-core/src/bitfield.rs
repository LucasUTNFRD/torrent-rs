use thiserror::Error;

#[derive(Debug, Error)]
pub enum BitfieldError {}

struct Bifield {
    inner: Vec<u8>,
}

impl Bifield {
    pub fn empty() {}

    pub fn with_size() {}

    pub fn from_bytes() {}

    pub fn is_empty() {}

    pub fn resize() {}
}

