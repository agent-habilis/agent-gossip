use habilis_network::runtime::{SendError, StateRefusal};
use serde_json::{Value, json};

/// What the app tells a caller whose write the shared state refused: the text of the engine, a
/// stable `reason` for a script, and the way out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StateRefusalReply {
    pub(crate) text: String,
    pub(crate) reason: &'static str,
    pub(crate) hint: &'static str,
}

impl StateRefusalReply {
    /// The reply for `error`, when `error` is a refusal of the shared state.
    pub(crate) fn of(error: &anyhow::Error) -> Option<Self> {
        let Some(SendError::State(refusal)) = error.downcast_ref::<SendError>() else {
            return None;
        };
        let (reason, hint) = match refusal {
            StateRefusal::NotSynced => (
                "not_synced",
                "wait for the first sync, then write again: it ends when a peer answers, and after 30 seconds at the latest",
            ),
            StateRefusal::Full { .. } => (
                "state_full",
                "delete keys with a null merge, or wait for the next cut of the state; at the memory limit the state refuses deletes too, so wait for the cut; then write again",
            ),
            StateRefusal::TooDeep { .. } => (
                "too_deep",
                "use fewer levels: key the entries by an id in one object",
            ),
            StateRefusal::TooLarge { .. } => (
                "too_large",
                "split the value into smaller values under their own keys",
            ),
            _ => ("refused", "write again later"),
        };
        Some(Self {
            text: refusal.to_string(),
            reason,
            hint,
        })
    }

    /// The response of the CLI: `ok` is false, and the process exits with a non-zero code.
    pub(crate) fn to_response(&self) -> String {
        json!({
            "ok": false,
            "error": self.text,
            "reason": self.reason,
            "hint": self.hint,
        })
        .to_string()
    }

    /// The message of an A2A error, which has no data field: the text, the reason and the hint.
    pub(crate) fn to_rpc_message(&self) -> String {
        format!("{} (reason: {}); {}", self.text, self.reason, self.hint)
    }

    /// The `data` of an MCP error: the stable reason and the way out.
    pub(crate) fn to_data(&self) -> Value {
        json!({ "reason": self.reason, "hint": self.hint })
    }
}

#[cfg(test)]
mod tests {
    use habilis_network::runtime::{SendError, StateRefusal};

    use super::StateRefusalReply;

    fn reply_of(refusal: StateRefusal) -> StateRefusalReply {
        let error = anyhow::Error::from(SendError::State(refusal));
        StateRefusalReply::of(&error).expect("the error was built from a SendError::State")
    }

    #[test]
    fn each_refusal_has_its_text_a_reason_and_a_way_out() {
        let cases = [
            (
                StateRefusal::NotSynced,
                "the shared state has not synced yet; retry in a few seconds",
                "not_synced",
                "30 seconds",
            ),
            (
                StateRefusal::Full {
                    used: 23_000,
                    limit: 22_666,
                    what: "operations",
                },
                "the shared state is full: 23000 of 22666 operations; a write that adds a value is refused, also with the same value; delete keys with a null merge to free space, or wait for the next cut",
                "state_full",
                "null merge",
            ),
            (
                StateRefusal::TooDeep {
                    depth: 13,
                    limit: 12,
                },
                "the value is nested 13 levels deep and the shared state allows 12",
                "too_deep",
                "fewer levels",
            ),
            (
                StateRefusal::TooLarge { bytes: 3_000 },
                "a value of the write is too large for the shared state: 3000 bytes with its keys; a value and its keys must fit one frame",
                "too_large",
                "smaller values",
            ),
        ];
        for (refusal, text, reason, way_out) in cases {
            let reply = reply_of(refusal);
            assert_eq!(reply.text, text, "the text is the one of the engine");
            assert_eq!(reply.reason, reason, "the reason is stable");
            assert!(
                reply.hint.contains(way_out),
                "the hint names the way out `{way_out}`: {}",
                reply.hint
            );
            let response: serde_json::Value = serde_json::from_str(&reply.to_response())
                .expect("the response is built with json!, so it parses");
            assert_eq!(response["ok"], false, "the CLI reports ok:false");
            assert_eq!(response["error"], text, "and the exact text");
        }
    }

    #[test]
    fn an_error_that_is_not_a_refusal_has_no_reply() {
        let stopped = anyhow::Error::from(SendError::Stopped);
        assert_eq!(
            StateRefusalReply::of(&stopped),
            None,
            "a stopped loop is not a refusal"
        );
        let other = anyhow::anyhow!("a transport failure");
        assert_eq!(StateRefusalReply::of(&other), None, "nor is a plain error");
    }
}
