//! A password or token held in memory. It has no `Display`, no `Serialize`
//! and a `Debug` that prints `[redacted]`, so logging a struct that holds one
//! cannot leak it.

use std::fmt;

#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Secret(value.into())
    }

    /// The value, for the one place that has to put it on the wire.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_hides_the_value() {
        let s = Secret::new("hunter2");
        assert_eq!(format!("{s:?}"), "[redacted]");
        assert_eq!(format!("{:?}", Some(&s)), "Some([redacted])");
        assert_eq!(s.expose(), "hunter2");
    }
}
