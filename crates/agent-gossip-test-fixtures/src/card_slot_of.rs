/// The slot of the member called `nickname` in a meta document, or `None` while its card is absent.
///
/// The slot is the hex of the author key, because a nickname is a label that two members can
/// share. The card carries the nickname in its `name`, so a test that knows only a name finds the
/// slot by it. If two members share the name, this gives the first slot in key order.
#[must_use]
pub fn card_slot_of(meta_doc: &serde_json::Value, nickname: &str) -> Option<String> {
    let peers = meta_doc.get("peers")?.as_object()?;
    let mut slots: Vec<&String> = peers
        .iter()
        .filter(|(_, entry)| {
            entry
                .pointer("/card/name")
                .and_then(serde_json::Value::as_str)
                == Some(nickname)
        })
        .map(|(slot, _)| slot)
        .collect();
    slots.sort();
    slots.first().map(|slot| (*slot).clone())
}
