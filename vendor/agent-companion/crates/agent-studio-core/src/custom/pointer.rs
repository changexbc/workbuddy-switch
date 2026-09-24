//! RFC 6901 JSON Pointer resolution. Mapping values are pointers into the raw
//! hook payload; no expressions, no JSONPath.

use super::limits::POINTER_MAX;
use serde_json::Value;

/// Why a mapping value is not a usable pointer. Kept as plain reasons so a
/// template author gets the offending field path from the caller.
pub fn validate(pointer: &str) -> Result<(), &'static str> {
    // Counted in characters, like every other field limit, so the Node mirror
    // cannot disagree on non-ASCII pointers.
    if pointer.chars().count() > POINTER_MAX {
        return Err("必须为以 / 开头的 JSON Pointer（RFC 6901）");
    }
    if !pointer.starts_with('/') {
        return Err("必须为以 / 开头的 JSON Pointer（RFC 6901）");
    }
    for token in pointer.split('/').skip(1) {
        if token.is_empty() {
            return Err("必须为以 / 开头的 JSON Pointer（RFC 6901）");
        }
    }
    let bytes = pointer.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'~' {
            match bytes.get(index + 1) {
                Some(b'0') | Some(b'1') => {}
                _ => return Err("Pointer 中的 ~ 必须转义为 ~0 或 ~1"),
            }
        }
    }
    Ok(())
}

fn unescape(token: &str) -> String {
    // `~1` must be replaced before `~0`, otherwise `~01` becomes `/`.
    token.replace("~1", "/").replace("~0", "~")
}

/// Resolves a pointer; `None` means "no value" (missing key, out-of-range index,
/// or a non-container on the path). An explicit JSON `null` resolves to `Some(Null)`.
pub fn resolve<'a>(root: &'a Value, pointer: &str) -> Option<&'a Value> {
    if pointer.is_empty() {
        return Some(root);
    }
    if !pointer.starts_with('/') {
        return None;
    }
    let mut current = root;
    for raw in pointer.split('/').skip(1) {
        let token = unescape(raw);
        current = match current {
            Value::Object(map) => map.get(&token)?,
            Value::Array(items) => {
                if token.is_empty() || (token.len() > 1 && token.starts_with('0')) {
                    return None;
                }
                let index: usize = token.parse().ok()?;
                items.get(index)?
            }
            _ => return None,
        };
    }
    Some(current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn resolves_objects_arrays_and_escapes() {
        let root = json!({"a": {"b": [10, {"c/d": 1}, {"e~f": 2}]}, "": 3});
        assert_eq!(resolve(&root, "/a/b/0"), Some(&json!(10)));
        assert_eq!(resolve(&root, "/a/b/1/c~1d"), Some(&json!(1)));
        assert_eq!(resolve(&root, "/a/b/2/e~0f"), Some(&json!(2)));
        assert_eq!(resolve(&root, ""), Some(&root));
        assert_eq!(resolve(&root, "/a/b/9"), None);
        assert_eq!(resolve(&root, "/a/c"), None);
        assert_eq!(resolve(&root, "/a/b/-"), None);
        assert_eq!(resolve(&root, "/a/b/01"), None);
        assert_eq!(resolve(&json!({"a": 1}), "/a/b"), None);
    }

    #[test]
    fn rejects_pointers_that_are_not_useful_or_well_formed() {
        assert!(validate("/session_id").is_ok());
        assert!(validate("/a~0b").is_ok());
        assert!(validate("/a~1b").is_ok());
        assert!(validate("session_id").is_err());
        assert!(validate("").is_err());
        assert!(validate("/").is_err());
        assert!(validate("//a").is_err());
        assert!(validate("/bad~2").is_err());
        assert!(validate("/trailing~").is_err());
        assert!(validate(&format!("/{}", "a".repeat(POINTER_MAX))).is_err());
        // The limit is characters, not bytes: 300 CJK characters are 900 bytes.
        assert!(validate(&format!("/{}", "指".repeat(300))).is_ok());
        assert!(validate(&format!("/{}", "指".repeat(POINTER_MAX))).is_err());
    }
}
