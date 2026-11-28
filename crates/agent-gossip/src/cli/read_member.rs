use std::path::Path;

/// The `member` field of a state file: the stable member key in hex, or `null` where the file has
/// none or cannot be read. A file from an older daemon has no field, and it proves nothing.
pub(super) fn read_member(state_file: &Path) -> serde_json::Value {
    let parsed: Option<serde_json::Value> = std::fs::read_to_string(state_file)
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok());
    let member = parsed
        .as_ref()
        .and_then(|document| document.get("member"))
        .and_then(serde_json::Value::as_str);
    member.map_or(serde_json::Value::Null, Into::into)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::read_member;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("read-member-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the temp dir is writable");
        dir
    }

    #[test]
    fn a_state_file_with_a_member_key_gives_it_as_text() {
        let dir = scratch("has");
        let path = dir.join("node.json");
        std::fs::write(&path, r#"{"member":"ab12","pubkey":"cd34"}"#).expect("the dir is writable");

        assert_eq!(
            read_member(&path),
            serde_json::json!("ab12"),
            "the file names the member, so the reader gives it as text"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_state_file_with_no_member_key_a_null_member_or_no_file_gives_null() {
        let dir = scratch("none");
        let without = dir.join("without.json");
        let null = dir.join("null.json");
        std::fs::write(&without, r#"{"pubkey":"cd34"}"#).expect("the dir is writable");
        std::fs::write(&null, r#"{"member":null}"#).expect("the dir is writable");

        let values = [
            read_member(&without),
            read_member(&null),
            read_member(&dir.join("none.json")),
        ];
        assert!(
            values.iter().all(serde_json::Value::is_null),
            "a file with no proof of a member shows null: {values:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
