use std::collections::BTreeSet;

use super::{AgentGossipServer, MCP_INSTRUCTIONS};

fn declared_tools() -> BTreeSet<String> {
    AgentGossipServer::tool_router()
        .list_all()
        .into_iter()
        .map(|tool| tool.name.to_string())
        .collect()
}

/// Identifiers in `MCP_INSTRUCTIONS` that look like a tool but name a JSON
/// field, an event kind, or a CLI flag instead. Anything tool-shaped that is
/// not here has to be a real tool.
const NOT_TOOLS: &[&str] = &[
    // event kinds
    "msg_posted",
    "peer_return",
    "peer_timeout",
    "ping_report",
    "task_progress",
    // JSON fields
    "task_id",
    "peer_count",
    "state_file",
    "last_seen",
    // task states
    "input_required",
    "auth_required",
];

/// The instructions are the agent's map of this server. A tool named there
/// that does not exist sends every MCP agent down a dead end, and the agent
/// has no way to tell — the call just fails.
///
/// This is how `send_task` survived: it was never a tool, the tool
/// descriptions in this same file documented the real flow, and nothing
/// compared the two.
#[test]
fn the_instructions_name_only_tools_that_exist() {
    let tools = declared_tools();
    let mut unknown: Vec<&str> = Vec::new();
    let code_spans = MCP_INSTRUCTIONS.split('`').skip(1).step_by(2);
    for chunk in code_spans {
        let tool_shaped = chunk.contains('_')
            && chunk
                .chars()
                .all(|character| character.is_ascii_lowercase() || character == '_');
        if tool_shaped && !NOT_TOOLS.contains(&chunk) && !tools.contains(chunk) {
            unknown.push(chunk);
        }
    }
    unknown.sort_unstable();
    unknown.dedup();
    assert!(
        unknown.is_empty(),
        "the instructions name tools that do not exist: {unknown:?}\ndeclared: {tools:?}"
    );
}

/// The module doc is the list a reader trusts before running anything, and
/// it drifted twice over: a stale count and a tool it never gained an entry
/// for.
#[test]
fn the_module_doc_lists_every_tool() {
    let source = include_str!("mod.rs");
    let header: Vec<&str> = source
        .lines()
        .take_while(|line| line.starts_with("//!"))
        .collect();
    let doc = header.join("\n");
    let tools = declared_tools();
    for tool in &tools {
        assert!(
            doc.contains(&format!("`{tool}`")),
            "the module doc never lists the `{tool}` tool"
        );
    }
    let spelled = ["eighteen", "nineteen", "twenty"]
        .into_iter()
        .find(|word| doc.contains(*word))
        .expect("the module doc spells out how many tools there are");
    let expected = match tools.len() {
        18 => "eighteen",
        19 => "nineteen",
        20 => "twenty",
        other => panic!("no spelled-out numeral for {other} tools — update this test"),
    };
    assert_eq!(
        spelled,
        expected,
        "the module doc says {spelled} tools; there are {}",
        tools.len()
    );
}
