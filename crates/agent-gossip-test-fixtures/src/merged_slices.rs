use serde_json::{Map, Value};

/// The slices of a channel view folded into one document.
///
/// A view holds one slice for each author. A test that writes disjoint keys can compare the union
/// of the keys of all slices, which is the document that the members agree on.
#[must_use]
pub fn merged_slices(view: &Value) -> Value {
    let mut document = Map::new();
    for slice in view.as_object().into_iter().flat_map(Map::values) {
        for (key, value) in slice.as_object().into_iter().flatten() {
            document.insert(key.clone(), value.clone());
        }
    }
    Value::Object(document)
}
