/// One chat message parsed from a daemon's JSON `message` event.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Msg {
    /// The nickname of the sender.
    pub author: String,
    /// The message text. Never empty: the parser skips events with an empty body.
    pub body: String,
    /// The recipient on a `type:"msg"`; `None` on a `type:"broadcast"`.
    pub to: Option<String>,
}
