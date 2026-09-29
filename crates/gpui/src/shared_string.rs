use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use std::{
    borrow::{Borrow, Cow},
    iter,
    ops::Deref,
    sync::Arc,
};

/// A shared string is an immutable string that can be cheaply cloned in GPUI
/// tasks. Uses SmolStr for efficient storage with inline optimization for small strings.
#[derive(Eq, PartialEq, PartialOrd, Ord, Hash, Clone, Default)]
pub struct SharedString(SmolStr);

impl SharedString {
    /// Creates a static [`SharedString`] from a `&'static str`.
    pub const fn new_static(str: &'static str) -> Self {
        Self(SmolStr::new_static(str))
    }

    /// Creates a [`SharedString`] from anything that can become a SmolStr
    pub fn new(str: impl Into<SmolStr>) -> Self {
        SharedString(str.into())
    }

    /// Get a &str from the underlying string.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl Deref for SharedString {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.0.as_str()
    }
}

impl JsonSchema for SharedString {
    fn inline_schema() -> bool {
        String::inline_schema()
    }

    fn schema_name() -> Cow<'static, str> {
        String::schema_name()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        String::json_schema(generator)
    }
}

impl AsRef<str> for SharedString {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for SharedString {
    fn borrow(&self) -> &str {
        self.as_ref()
    }
}

impl std::fmt::Debug for SharedString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::fmt::Display for SharedString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.as_str())
    }
}

impl PartialEq<String> for SharedString {
    fn eq(&self, other: &String) -> bool {
        self.as_ref() == other
    }
}

impl PartialEq<SharedString> for String {
    fn eq(&self, other: &SharedString) -> bool {
        self == other.as_ref()
    }
}

impl PartialEq<str> for SharedString {
    fn eq(&self, other: &str) -> bool {
        self.as_ref() == other
    }
}

impl<'a> PartialEq<&'a str> for SharedString {
    fn eq(&self, other: &&'a str) -> bool {
        self.as_ref() == *other
    }
}

impl From<&SharedString> for SharedString {
    fn from(value: &SharedString) -> Self {
        value.clone()
    }
}

impl From<SharedString> for Arc<str> {
    fn from(val: SharedString) -> Self {
        // Reuses the underlying Arc when SmolStr is in its Heap variant (zero-copy).
        Arc::<str>::from(val.0)
    }
}

impl From<&str> for SharedString {
    fn from(value: &str) -> Self {
        Self(SmolStr::from(value))
    }
}

impl From<char> for SharedString {
    #[inline]
    fn from(c: char) -> SharedString {
        SharedString(SmolStr::from_iter(iter::once(c)))
    }
}

impl From<String> for SharedString {
    fn from(value: String) -> Self {
        Self(SmolStr::from(value))
    }
}

impl From<Arc<str>> for SharedString {
    fn from(value: Arc<str>) -> Self {
        // SmolStr's `From<Arc<str>>` keeps the original Arc on the heap path,
        // avoiding an allocation+copy for long strings.
        Self(SmolStr::from(value))
    }
}

impl From<&String> for SharedString {
    fn from(value: &String) -> Self {
        Self(SmolStr::from(value))
    }
}

impl From<SharedString> for String {
    fn from(val: SharedString) -> Self {
        val.0.to_string()
    }
}

impl Serialize for SharedString {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_ref())
    }
}

impl<'de> Deserialize<'de> for SharedString {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(SharedString::from(s))
    }
}

#[cfg(test)]
mod tests {
    use super::SharedString;

    #[test]
    fn from_char_builds_a_one_character_string() {
        assert_eq!(SharedString::from('a').as_str(), "a");
        assert_eq!(SharedString::from('字').as_str(), "字");
        let emoji: SharedString = '😀'.into();
        assert_eq!(emoji.as_str(), "😀");
    }
}
