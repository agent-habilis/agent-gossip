use habilis_network::runtime::{SendError, SliceError};
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
            SliceError::TooLarge { .. } => (
                "too_large",
                "delete keys of your own slice with a null merge, or store less",
            ),
            SliceError::RateLimited => (
                "rate_limited",
                "wait a few seconds and write again, and write less often",
            ),
            SliceError::TooManyAuthors { .. } => (
                "too_many_authors",
                "wait until a member that left drops out of the shared state, then write again",
            ),
            SliceError::GoneKey => (
                "gone_key",
                "the key of this run is gone, because this run already left or was replaced: leave the gossip and join again",
            ),
            SliceError::Malformed => (
                "malformed",
                "the engine could not build the slice body: report a bug",
            ),
            SliceError::WrongChannel => (
                "wrong_channel",
                "write to the channel that the command names, `state` or `meta`",
            ),
            SliceError::DraftMismatch => (
                "draft_mismatch",
                "write again; if it repeats, report it as a bug",
            ),
            SliceError::StaleDraft => ("stale_draft", "write again: another write came first"),
            SliceError::Seal => (
                "seal",
                "write again; if it repeats, report a bug",
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
    use habilis_network::runtime::{SendError, SliceError};

    use super::StateRefusalReply;

    fn reply_of(refusal: SliceError) -> StateRefusalReply {
        let error = anyhow::Error::from(SendError::State(refusal));
        StateRefusalReply::of(&error).expect("the error was built from a SendError::State")
    }

    #[test]
    fn each_refusal_has_its_text_a_reason_and_a_way_out() {
        let cases = [
            (
                SliceError::TooLarge {
                    bytes: 3_000,
                    limit: 2_048,
                },
                "too_large",
                "store less",
            ),
            (SliceError::RateLimited, "rate_limited", "write less often"),
            (
                SliceError::TooManyAuthors { limit: 1_024 },
                "too_many_authors",
                "drops out",
            ),
            (SliceError::GoneKey, "gone_key", "join again"),
            (SliceError::Malformed, "malformed", "report a bug"),
            (SliceError::WrongChannel, "wrong_channel", "`state` or `meta`"),
            (SliceError::DraftMismatch, "draft_mismatch", "report it"),
            (SliceError::StaleDraft, "stale_draft", "came first"),
            (SliceError::Seal, "seal", "report a bug"),
        ];
        for (refusal, reason, way_out) in cases {
            let text = refusal.to_string();
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
