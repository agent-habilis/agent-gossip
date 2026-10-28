use serde::{Deserialize, Serialize};

/// A2A v1.0 `Role`, `ProtoJSON` `SCREAMING_SNAKE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Role {
    #[serde(rename = "ROLE_UNSPECIFIED")]
    Unspecified,
    #[serde(rename = "ROLE_USER")]
    User,
    #[serde(rename = "ROLE_AGENT")]
    Agent,
}
