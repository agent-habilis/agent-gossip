use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

/// A UUID-string id newtype with validating deserialize, so a crafted
/// non-UUID id is rejected at parse. One macro, three ids: the sender-minted
/// message id, the server-minted task id, and the gossip RPC correlation id.
macro_rules! uuid_id {
    ($name:ident, $error:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let raw = String::deserialize(deserializer)?;
                Uuid::parse_str(&raw).map_err(|_| serde::de::Error::custom($error))?;
                Ok(Self(raw))
            }
        }

        impl $name {
            #[must_use]
            pub fn random() -> Self {
                Self(Uuid::new_v4().to_string())
            }

            /// Adopt an existing UUID string (e.g. a frame id, which shares
            /// the UUID form). `None` for a non-UUID.
            #[must_use]
            pub fn from_uuid_str(raw: &str) -> Option<Self> {
                Uuid::parse_str(raw).ok().map(|_| Self(raw.to_string()))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl std::str::FromStr for $name {
            type Err = String;

            fn from_str(raw: &str) -> Result<Self, Self::Err> {
                Self::from_uuid_str(raw).ok_or_else(|| $error.to_string())
            }
        }

        #[cfg(test)]
        impl From<&str> for $name {
            fn from(raw: &str) -> Self {
                Uuid::parse_str(raw).expect($error);
                Self(raw.to_string())
            }
        }
    };
}

uuid_id!(MessageId, "invalid a2a message id");
uuid_id!(TaskId, "invalid a2a task id");
uuid_id!(A2aRpcId, "invalid a2a rpc id");
