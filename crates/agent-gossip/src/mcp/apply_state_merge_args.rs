use rmcp::schemars;
use serde::Deserialize;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct ApplyStateMergeArgs {
    /// The RFC 7386 JSON Merge Patch — a JSON value merged into the document,
    /// e.g. `{"turn":"b"}`. An object deep-merges (each key is set; a `null`
    /// value deletes that key; nested objects merge recursively; arrays are
    /// replaced wholesale — model a mutable collection as an object keyed by
    /// index), and a non-object value (scalar/array/null) replaces the target.
    pub(crate) merge: serde_json::Value,
}
