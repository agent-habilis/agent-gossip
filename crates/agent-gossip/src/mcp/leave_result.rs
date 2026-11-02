use serde::Serialize;

#[derive(Debug, Serialize)]
pub(crate) struct LeaveResult {
    pub(crate) ok: bool,
}
