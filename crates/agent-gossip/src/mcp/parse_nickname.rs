use habilis_network::protocol::Nickname;
use rmcp::model::ErrorData as McpError;

pub(crate) fn parse_nickname(raw: Option<String>) -> Result<Option<Nickname>, McpError> {
    raw.map(|raw| Nickname::try_new(&raw))
        .transpose()
        .map_err(|error| McpError::invalid_params(format!("invalid nickname: {error}"), None))
}
