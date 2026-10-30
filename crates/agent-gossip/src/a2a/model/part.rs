use serde::{Deserialize, Deserializer, Serialize};

/// A2A v1.0 `Part`: a proto `oneof content` with exactly one of `text`, `raw`, `url` or `data`.
///
/// `raw` holds base64 bytes. The `filename`, `mediaType` and `metadata` fields are siblings.
/// `ProtoJSON` puts the set oneof member at top level (no
/// `kind` discriminator), so the deserialize step enforces exactly-one-of at parse.
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Part {
    /// Text content. Exactly one of `text`, `raw`, `url`, `data` is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Base64-encoded bytes of a file. Exactly one of `text`, `raw`, `url`, `data` is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
    /// Where to fetch a file. Exactly one of `text`, `raw`, `url`, `data` is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Structured JSON content. Exactly one of `text`, `raw`, `url`, `data` is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    /// Name of the file that `raw` or `url` carries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    /// MIME type of the content; wire name `mediaType`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    /// Free-form JSON that rides along with the part.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

impl Part {
    /// A plain text part.
    #[must_use]
    pub fn text(text: &str) -> Self {
        Part {
            text: Some(text.to_owned()),
            ..Part::default()
        }
    }

    /// The display projection: the text content, or a bracketed placeholder for
    /// a file (`url`/`raw`) or structured (`data`) part. One projection, reused
    /// wherever parts are rendered to a line.
    #[must_use]
    pub fn display(&self) -> String {
        if let Some(text) = &self.text {
            text.clone()
        } else if self.url.is_some() || self.raw.is_some() {
            format!("[file {}]", self.filename.as_deref().unwrap_or("unnamed"))
        } else {
            "[data]".to_string()
        }
    }
}

impl<'de> Deserialize<'de> for Part {
    fn deserialize<Input>(deserializer: Input) -> Result<Self, Input::Error>
    where
        Input: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Wire {
            text: Option<String>,
            raw: Option<String>,
            url: Option<String>,
            data: Option<serde_json::Value>,
            filename: Option<String>,
            media_type: Option<String>,
            metadata: Option<serde_json::Value>,
        }
        let raw = Wire::deserialize(deserializer)?;
        let present: [bool; 4] = [
            raw.text.is_some(),
            raw.raw.is_some(),
            raw.url.is_some(),
            raw.data.is_some(),
        ];
        let content: usize = present.into_iter().filter(|set| *set).count();
        if content != 1 {
            return Err(serde::de::Error::custom(
                "a part must carry exactly one of text, raw, url, or data",
            ));
        }
        Ok(Part {
            text: raw.text,
            raw: raw.raw,
            url: raw.url,
            data: raw.data,
            filename: raw.filename,
            media_type: raw.media_type,
            metadata: raw.metadata,
        })
    }
}
