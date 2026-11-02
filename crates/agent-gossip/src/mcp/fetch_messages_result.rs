use serde::Serialize;

#[derive(Debug, Serialize)]
pub(crate) struct FetchMessagesResult {
    /// Surfaced events since the cursor, each the *same* JSON object the live
    /// `--output json` stream emits (carrying `seq`, `event`/`type`,
    /// `display`, `self`, …) — chat, presence, task legs, and the
    /// transient `ping_report` / `peer_timeout` / … events alike. Held as
    /// [`RawValue`](serde_json::value::RawValue) so each event's rendered,
    /// field-order-pinned line is embedded verbatim (re-parsing to a `Value`
    /// would key-sort it and break parity with `--output json`).
    pub(crate) messages: Vec<Box<serde_json::value::RawValue>>,
    /// The newest `seq` returned (the next `after`), or `None` when nothing
    /// new arrived.
    pub(crate) current_seq: Option<u64>,
    /// Set when the `after` cursor aged out of the surfaced-events ring: every
    /// event below this `seq` was evicted before this call and is gone. The
    /// window below is a fresh baseline, not a continuation. `None` on a normal
    /// read — which is the only case in which `messages` is complete.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) missed_before: Option<u64>,
}
