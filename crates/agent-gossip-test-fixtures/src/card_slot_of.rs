/// The slot of the member called `nickname` in a meta document, or `None` while its entry is absent.
///
/// The slot is the hex of the author key, because a nickname is a label that two members can
/// share. The entry carries the nickname in its `nickname` field, so a test that knows only a name
/// finds the slot by it. If two members share the name, this gives the first slot in key order.
#[must_use]
pub fn card_slot_of(meta_doc: &serde_json::Value, nickname: &str) -> Option<String> {
    let slots = meta_doc.as_object()?;
    let mut named: Vec<&String> = slots
        .iter()
        .filter(|(_, entry)| {
            entry.get("nickname").and_then(serde_json::Value::as_str) == Some(nickname)
        })
        .map(|(slot, _)| slot)
        .collect();
    named.sort();
    named.first().map(|slot| (*slot).clone())
}
