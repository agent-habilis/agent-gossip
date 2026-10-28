#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Msg {
    pub author: String,
    pub body: String,
    /// The recipient on a `type:"msg"`; `None` on a `type:"broadcast"`.
    pub to: Option<String>,
}
