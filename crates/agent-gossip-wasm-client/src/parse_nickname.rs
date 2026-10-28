use habilis_network::protocol::Nickname;
use wasm_bindgen::JsValue;

use crate::err::err;

pub(crate) fn parse_nickname(nickname: Option<String>) -> Result<Option<Nickname>, JsValue> {
    match nickname
        .as_deref()
        .map(str::trim)
        .filter(|nick| !nick.is_empty())
    {
        None => Ok(None),
        Some(nick) => nick
            .parse::<Nickname>()
            .map(Some)
            .map_err(|error| err("parse nickname", &error)),
    }
}
