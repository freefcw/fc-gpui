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
#[derive(Eq, PartialOrd, Ord, Clone, Default)]
pub struct SharedString(SmolStr);

impl PartialEq for SharedString {
    fn eq(&self, other: &Self) -> bool {
        let (this, other_str) = (self.as_str(), other.as_str());
        (this.as_ptr() == other_str.as_ptr() && this.len() == other_str.len()) || self.0 == other.0
    }
}

impl std::hash::Hash for SharedString {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

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
        SmolStr::deserialize(deserializer).map(SharedString)
    }
}

#[cfg(test)]
mod tests {
    use super::SharedString;
    use std::collections::HashSet;
    use std::hash::{Hash, Hasher};

    #[test]
    fn from_char_builds_a_one_character_string() {
        assert_eq!(SharedString::from('a').as_str(), "a");
        assert_eq!(SharedString::from('字').as_str(), "字");
        let emoji: SharedString = '😀'.into();
        assert_eq!(emoji.as_str(), "😀");
    }

    #[test]
    fn pointer_equal_clones_match_and_hash_like_distinct_copies() {
        let long = SharedString::from("a shared string longer than the inline capacity");
        let clone = long.clone();
        assert_eq!(long.as_str().as_ptr(), clone.as_str().as_ptr());
        assert_eq!(long, clone);

        let same_bytes = SharedString::from(long.as_str().to_string());
        assert_ne!(long.as_str().as_ptr(), same_bytes.as_str().as_ptr());
        assert_eq!(long, same_bytes);
        assert_eq!(long, same_bytes.as_str());
        assert_eq!(long, same_bytes.to_string());
        assert_eq!(same_bytes.to_string(), long);

        assert_eq!(hash_of(&long), hash_of(&clone));
        assert_eq!(hash_of(&long), hash_of(&same_bytes));

        let mut set = HashSet::new();
        set.insert(long);
        assert!(set.contains(&clone));
        assert!(set.contains(&same_bytes));

        let static_text = SharedString::new_static("static label longer than the inline capacity");
        let static_clone = static_text.clone();
        assert_eq!(
            static_text.as_str().as_ptr(),
            static_clone.as_str().as_ptr()
        );
        assert_eq!(static_text, static_clone);
        assert_eq!(hash_of(&static_text), hash_of(&static_clone));
        assert_eq!(static_text, "static label longer than the inline capacity");
    }

    #[test]
    fn deserializes_through_smolstr_and_roundtrips() {
        let short: SharedString = serde_json::from_str("\"hi\"").unwrap();
        assert_eq!(short.as_str(), "hi");

        let escaped: SharedString = serde_json::from_str("\"a\\nb\"").unwrap();
        assert_eq!(escaped.as_str(), "a\nb");

        let long_text = "this string is longer than smolstr inline capacity";
        let encoded = serde_json::to_string(&SharedString::from(long_text)).unwrap();
        let long: SharedString = serde_json::from_str(&encoded).unwrap();
        assert_eq!(long.as_str(), long_text);
        assert_eq!(serde_json::to_string(&long).unwrap(), encoded);
    }

    fn hash_of(value: &impl Hash) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        value.hash(&mut hasher);
        hasher.finish()
    }
}
