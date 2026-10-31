use serde::{Deserialize, Serialize};

/// A2A v1.0 `Role`, `ProtoJSON` `SCREAMING_SNAKE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Role {
    /// Wire value `ROLE_UNSPECIFIED`; the proto default, meaning none was set.
    #[serde(rename = "ROLE_UNSPECIFIED")]
    Unspecified,
    /// Wire value `ROLE_USER`; the requester, the side that delegates work.
    #[serde(rename = "ROLE_USER")]
    User,
    /// Wire value `ROLE_AGENT`; the worker, the side that does the work.
    #[serde(rename = "ROLE_AGENT")]
    Agent,
}
