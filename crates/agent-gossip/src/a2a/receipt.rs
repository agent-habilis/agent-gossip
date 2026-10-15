use habilis_network::embed::{EventLoopState, HandlerCtx};
use habilis_network::protocol::{AppTag, MessageBody, MessageKind, Nickname};
use serde::{Deserialize, Serialize};

use crate::a2a::send::{DirectedRpcParams, seal_directed, send_directed_rpc};
use crate::a2a::wire;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReceiptState {
    Delivered,
    Read,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Receipt {
    #[serde(rename = "ref")]
    pub(crate) ref_id: String,
    pub(crate) state: ReceiptState,
}

/// Takes the handler context rather than a bare sender: the engine state does
/// not expose the mesh id or our nickname, and the frame needs both.
///
/// Fire-and-forget: a receipt never echoes, never retains, and never answers a
/// receipt, so a failure only costs the sender a proof.
pub(crate) async fn send_receipt(
    state: &mut EventLoopState,
    ctx: &HandlerCtx<'_>,
    to: &Nickname,
    receipt: &Receipt,
) -> anyhow::Result<()> {
    let outcome = send_sealed(state, ctx, to, receipt).await;
    if let Err(error) = &outcome {
        tracing::debug!(target: "agent_gossip::a2a", %error, "receipt not sent");
    }
    outcome
}

async fn send_sealed(
    state: &mut EventLoopState,
    ctx: &HandlerCtx<'_>,
    to: &Nickname,
    receipt: &Receipt,
) -> anyhow::Result<()> {
    let plain = MessageBody::new(serde_json::to_string(receipt)?)?;
    let body = seal_directed(state, to, &plain)?;
    let kind = MessageKind::App {
        tag: AppTag::from(wire::RECEIPT),
        to: Some(to.clone()),
        corr: None,
    };
    send_directed_rpc(
        DirectedRpcParams {
            mesh: ctx.mesh,
            author: ctx.author,
            kind,
            body,
        },
        state,
        ctx.sender,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::{Receipt, ReceiptState};

    #[test]
    fn receipt_wire_shape_is_ref_and_snake_case_state() {
        let receipt = Receipt {
            ref_id: "abc".to_owned(),
            state: ReceiptState::Delivered,
        };
        assert_eq!(
            serde_json::to_string(&receipt).unwrap(),
            r#"{"ref":"abc","state":"delivered"}"#
        );
        let read: Receipt = serde_json::from_str(r#"{"ref":"abc","state":"read"}"#).unwrap();
        assert_eq!(read.state, ReceiptState::Read);
    }
}
