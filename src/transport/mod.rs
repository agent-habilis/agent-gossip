//! `ipc`: the unix-socket / named-pipe listener used by the CLI's
//! `msg` and `poll` subcommands to talk to a running `create` or
//! `join` daemon. (The MCP stdio server lives in `crate::mcp`.)

pub(crate) mod ipc;
