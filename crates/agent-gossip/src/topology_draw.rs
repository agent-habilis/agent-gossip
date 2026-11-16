use std::collections::BTreeSet;
use std::fmt::Write as _;

use serde_json::Value;

/// A cap on direct connections: `used` of `max`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Cap {
    pub used: u64,
    pub max: u64,
}

/// One peer of the roster.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PeerView {
    pub nickname: String,
    /// Endpoint id in hex; the key of the link-state edges.
    pub endpoint: Option<String>,
    /// The path label of `peers`: `ip`, `webrtc`, `relay`, `ip+relay`, `unknown`.
    pub path: String,
    /// The lane of a directed send: `unicast`, `multihop`, `relay-only`, ...
    pub lane: Option<String>,
    pub quiet: bool,
    pub neighbor: Option<bool>,
    pub pinned: Option<bool>,
    pub via: Option<String>,
    /// Reached through other members because the cap was full.
    pub capped: Option<bool>,
}

/// One directed link-state edge, by endpoint id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EdgeView {
    pub from: String,
    pub to: String,
    pub metric: u64,
}

// `neighbor`, `pinned`, `via`, `capped` and `cap` are `Option`: the engine
// does not report them yet, and the drawing omits a field it does not have.
/// Everything the drawing needs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TopologyDoc {
    pub me: String,
    /// Endpoint id of this node; empty when the multihop transport is off.
    pub self_id: String,
    pub cap: Option<Cap>,
    pub peers: Vec<PeerView>,
    pub edges: Vec<EdgeView>,
}

impl TopologyDoc {
    /// Build the document from the `peers` and `topology` IPC responses.
    pub(crate) fn from_responses(me: &str, peers: &Value, topology: &Value) -> Self {
        let endpoint_of = |nickname: &str| {
            topology["endpoints"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|entry| entry["nickname"] == nickname)
                .and_then(|entry| entry["endpoint"].as_str())
                .map(str::to_owned)
        };
        let text = |row: &Value, key: &str| row[key].as_str().map(str::to_owned);
        let peers = peers["peers"].as_array().into_iter().flatten().map(|row| {
            let nickname = text(row, "nickname").unwrap_or_default();
            PeerView {
                endpoint: endpoint_of(&nickname),
                path: text(row, "path").unwrap_or_else(|| "unknown".to_owned()),
                lane: text(row, "transport"),
                quiet: row["quiet"].as_bool().unwrap_or(false),
                neighbor: row["neighbor"].as_bool(),
                pinned: row["pinned"].as_bool(),
                via: text(row, "via"),
                capped: row["capped"].as_bool(),
                nickname,
            }
        });
        let edges = topology["topology"]["edges"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|edge| {
                Some(EdgeView {
                    from: edge["from"].as_str()?.to_owned(),
                    to: edge["to"].as_str()?.to_owned(),
                    metric: edge["metric"].as_u64()?,
                })
            });
        let cap = &topology["topology"]["cap"];
        Self {
            me: me.to_owned(),
            self_id: topology["topology"]["self_id"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            cap: cap["used"]
                .as_u64()
                .zip(cap["max_connections"].as_u64())
                .map(|(used, max)| Cap { used, max }),
            peers: peers.collect(),
            edges: edges.collect(),
        }
    }

    /// The name of an endpoint id: this node, a peer, or the id shortened.
    fn name_of(&self, endpoint: &str) -> String {
        if endpoint == self.self_id {
            return self.me.clone();
        }
        self.peers
            .iter()
            .find(|peer| peer.endpoint.as_deref() == Some(endpoint))
            .map_or_else(
                || endpoint.chars().take(8).collect(),
                |peer| peer.nickname.clone(),
            )
    }

    fn knows(&self, endpoint: &str) -> bool {
        endpoint == self.self_id
            || self
                .peers
                .iter()
                .any(|peer| peer.endpoint.as_deref() == Some(endpoint))
    }
}

/// The problems of the document, one line each, `warning: ...`.
pub(crate) fn warnings(doc: &TopologyDoc) -> Vec<String> {
    let mut lines = Vec::new();
    if doc.self_id.is_empty() {
        lines.push("warning: multihop is off, the mesh view is empty".to_owned());
    }
    if let Some(cap) = &doc.cap
        && cap.used >= cap.max
    {
        lines.push(format!(
            "warning: connection cap is full ({}/{})",
            cap.used, cap.max
        ));
    }
    for peer in doc
        .peers
        .iter()
        .filter(|peer| !peer.quiet && peer.path == "unknown")
    {
        lines.push(format!("warning: {} has no known path", peer.nickname));
    }
    let unknown: BTreeSet<&str> = doc
        .edges
        .iter()
        .flat_map(|edge| [edge.from.as_str(), edge.to.as_str()])
        .filter(|id| !doc.knows(id))
        .collect();
    for id in unknown {
        let short: String = id.chars().take(8).collect();
        lines.push(format!(
            "warning: link endpoint {short} is not in the roster"
        ));
    }
    lines
}

/// Draw the document. The text ends with a newline.
pub(crate) fn render(doc: &TopologyDoc) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut me_lines = vec![format!("{} (you)", doc.me)];
    if let Some(cap) = &doc.cap {
        me_lines.push(format!("cap {}/{}", cap.used, cap.max));
    }
    out.extend(boxed(&me_lines));

    let mut peers: Vec<&PeerView> = doc.peers.iter().collect();
    peers.sort_by(|left, right| (left.quiet, &left.nickname).cmp(&(right.quiet, &right.nickname)));
    let labels: Vec<String> = peers.iter().map(|peer| edge_label(peer)).collect();
    let label_width = labels
        .iter()
        .map(|label| label.chars().count())
        .max()
        .unwrap_or(0);
    for (index, (peer, label)) in peers.iter().zip(&labels).enumerate() {
        let last = index + 1 == peers.len();
        let (branch, trunk) = if last {
            ("└─", " ")
        } else {
            ("├─", "│")
        };
        let fill = "─".repeat(label_width - label.chars().count());
        for (row, line) in boxed(&peer_lines(peer)).into_iter().enumerate() {
            if row == 1 {
                out.push(format!("  {branch} {label} {fill}─▶ {line}"));
            } else {
                out.push(format!("  {trunk}  {} {line}", " ".repeat(label_width + 3)));
            }
        }
    }

    let capped: Vec<&str> = peers
        .iter()
        .filter(|peer| peer.capped == Some(true))
        .map(|peer| peer.nickname.as_str())
        .collect();
    if !capped.is_empty() {
        out.push(String::new());
        out.push(format!(
            "reached through other peers because of the cap: {}",
            capped.join(", ")
        ));
    }

    out.push(String::new());
    out.push("links between other peers".to_owned());
    let links = mesh_links(doc);
    if links.is_empty() {
        out.push("  none".to_owned());
    }
    out.extend(links);
    out.push(String::new());
    out.join("\n")
}

fn edge_label(peer: &PeerView) -> String {
    let mut label = peer.path.clone();
    if let Some(via) = &peer.via {
        let _ = write!(label, " via {via}");
    }
    if let Some(lane) = peer.lane.as_deref().filter(|lane| *lane != "unicast") {
        let _ = write!(label, " · {lane}");
    }
    label
}

fn peer_lines(peer: &PeerView) -> Vec<String> {
    let mut flags = Vec::new();
    match peer.neighbor {
        Some(true) => flags.push("neighbor"),
        Some(false) => flags.push("unicast"),
        None => {}
    }
    if peer.pinned == Some(true) {
        flags.push("pinned");
    }
    if peer.quiet {
        flags.push("quiet");
    }
    let mut lines = vec![peer.nickname.clone()];
    if !flags.is_empty() {
        lines.push(flags.join(" · "));
    }
    lines
}

/// The edges that do not touch this node, by name, without repeats.
fn mesh_links(doc: &TopologyDoc) -> Vec<String> {
    let links: BTreeSet<(String, String, u64)> = doc
        .edges
        .iter()
        .filter(|edge| edge.from != doc.self_id && edge.to != doc.self_id)
        .map(|edge| (doc.name_of(&edge.from), doc.name_of(&edge.to), edge.metric))
        .collect();
    links
        .into_iter()
        .map(|(from, to, metric)| format!("  {from} ──{metric}─▶ {to}"))
        .collect()
}

/// Put the lines in a box. The first line is the title row; a box with one
/// line still has the three rows top, content, bottom.
fn boxed(lines: &[String]) -> Vec<String> {
    let width = lines
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0);
    let bar = "─".repeat(width + 2);
    let mut rows = vec![format!("┌{bar}┐")];
    rows.extend(lines.iter().map(|line| {
        let pad = " ".repeat(width - line.chars().count());
        format!("│ {line}{pad} │")
    }));
    rows.push(format!("└{bar}┘"));
    rows
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{TopologyDoc, render, warnings};

    const ME: &str = "aa";
    const ONE: &str = "bb";
    const TWO: &str = "cc";

    fn responses(with_edges: bool) -> (serde_json::Value, serde_json::Value) {
        let peers = json!({"ok": true, "peers": [
            {"nickname": "zed", "path": "webrtc", "quiet": false, "transport": "multihop"},
            {"nickname": "amy", "path": "ip", "quiet": false, "transport": "unicast"},
            {"nickname": "old", "path": "unknown", "quiet": true, "transport": "unreachable"},
        ]});
        let edges = if with_edges {
            json!([
                {"from": ME, "to": ONE, "metric": 1},
                {"from": ONE, "to": TWO, "metric": 3},
                {"from": ONE, "to": TWO, "metric": 3},
                {"from": TWO, "to": "ffffffffffff", "metric": 2},
            ])
        } else {
            json!([])
        };
        let topology = json!({"ok": true,
        "topology": {"self_id": ME, "edges": edges},
        "endpoints": [
            {"nickname": "amy", "endpoint": ONE},
            {"nickname": "zed", "endpoint": TWO},
        ]});
        (peers, topology)
    }

    /// Paths and lanes label the edges; a multihop lane shows after the path;
    /// quiet peers sort last; the links between other peers are named and
    /// listed once.
    #[test]
    fn draws_peers_edges_and_mesh_links() {
        let (peers, topology) = responses(true);
        let doc = TopologyDoc::from_responses("me", &peers, &topology);

        let drawing = render(&doc);

        let expected = "\
┌──────────┐
│ me (you) │
└──────────┘
  │                           ┌─────┐
  ├─ ip ────────────────────▶ │ amy │
  │                           └─────┘
  │                           ┌─────┐
  ├─ webrtc · multihop ─────▶ │ zed │
  │                           └─────┘
                              ┌───────┐
  └─ unknown · unreachable ─▶ │ old   │
                              │ quiet │
                              └───────┘

links between other peers
  amy ──3─▶ zed
  zed ──2─▶ ffffffff
";
        assert_eq!(drawing, expected, "the drawing of the fixed document");
    }

    /// With the multihop transport off, the mesh view is empty and the
    /// warning says why.
    #[test]
    fn warns_when_multihop_is_off() {
        let peers = json!({"peers": []});
        let topology =
            json!({"ok": true, "topology": {"self_id": "", "edges": []}, "endpoints": []});
        let doc = TopologyDoc::from_responses("me", &peers, &topology);

        assert_eq!(
            warnings(&doc),
            ["warning: multihop is off, the mesh view is empty"],
            "one warning for the off transport"
        );
    }

    /// An edge to an endpoint that no roster row owns is a warning, and a
    /// peer with no path is a warning; a quiet peer with no path is not.
    #[test]
    fn warns_about_unknown_endpoints_and_missing_paths() {
        let (mut peers, topology) = responses(true);
        peers["peers"][0]["path"] = json!("unknown");
        let doc = TopologyDoc::from_responses("me", &peers, &topology);

        assert_eq!(
            warnings(&doc),
            [
                "warning: zed has no known path",
                "warning: link endpoint ffffffff is not in the roster",
            ],
            "the warnings, in order"
        );
    }

    /// The engine fields that are not reported yet stay out of the drawing;
    /// when they are present they show: the cap, the neighbor flag, and the
    /// peers reached through others because of the cap.
    #[test]
    fn shows_the_optional_engine_fields_when_present() {
        let (mut peers, mut topology) = responses(false);
        peers["peers"][1]["neighbor"] = json!(true);
        peers["peers"][1]["pinned"] = json!(true);
        peers["peers"][0]["neighbor"] = json!(false);
        peers["peers"][0]["capped"] = json!(true);
        peers["peers"][0]["via"] = json!("amy");
        topology["topology"]["cap"] = json!({"used": 8, "max_connections": 8});
        let doc = TopologyDoc::from_responses("me", &peers, &topology);

        let drawing = render(&doc);

        assert!(
            drawing.contains("cap 8/8"),
            "the cap shows in the box of this node:\n{drawing}"
        );
        assert!(
            drawing.contains("neighbor · pinned"),
            "flags of amy:\n{drawing}"
        );
        assert!(
            drawing.contains("webrtc via amy · multihop"),
            "the via shows:\n{drawing}"
        );
        assert!(
            drawing.contains("reached through other peers because of the cap: zed"),
            "the capped footer:\n{drawing}"
        );
        assert_eq!(
            warnings(&doc),
            ["warning: connection cap is full (8/8)"],
            "a full cap warns"
        );
    }
}
