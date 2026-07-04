use std::time::Duration;

use anstyle::{AnsiColor, Style};
use anyhow::Result;
use serde::Serialize;

use crate::lookup::{self, NetworkCapability};
use crate::protocol::SwarmId;
use crate::protocol::swarm::{RelayChoice, Swarm};
use crate::transport::ipc::{self, IpcCommand};
use crate::util::output;

use super::agent::{self, AgentState};
use super::args::{DoctorOpts, OutputFormat};

/// Budget for the machine net-report (build endpoint + first completed report).
const CAPABILITY_TIMEOUT: Duration = Duration::from_secs(6);
/// Per-rung relay reachability budget for `--swarm` ladder probing.
const RUNG_TIMEOUT: Duration = Duration::from_secs(2);
/// Budget for the `--swarm` rendezvous connect-probe.
const RENDEZVOUS_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
enum Verdict {
    Ok,
    Warn,
    Fail,
}

impl Verdict {
    fn symbol(self) -> (&'static str, AnsiColor) {
        match self {
            Verdict::Ok => ("✓", AnsiColor::Green),
            Verdict::Warn => ("!", AnsiColor::Yellow),
            Verdict::Fail => ("✗", AnsiColor::Red),
        }
    }
}

#[derive(Debug, Serialize)]
struct Check {
    name: String,
    verdict: Verdict,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

impl Check {
    fn new(name: impl Into<String>, verdict: Verdict, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            verdict,
            detail: Some(detail.into()),
        }
    }

    fn bare(name: impl Into<String>, verdict: Verdict) -> Self {
        Self {
            name: name.into(),
            verdict,
            detail: None,
        }
    }
}

#[derive(Debug, Serialize)]
struct Section {
    title: String,
    checks: Vec<Check>,
}

#[derive(Debug, Serialize)]
struct Summary {
    ok: usize,
    warn: usize,
    fail: usize,
}

#[derive(Debug, Serialize)]
struct Report {
    mode: &'static str,
    sections: Vec<Section>,
    summary: Summary,
}

impl Report {
    fn new(mode: &'static str, sections: Vec<Section>) -> Self {
        let mut summary = Summary {
            ok: 0,
            warn: 0,
            fail: 0,
        };
        for check in sections.iter().flat_map(|section| &section.checks) {
            match check.verdict {
                Verdict::Ok => summary.ok += 1,
                Verdict::Warn => summary.warn += 1,
                Verdict::Fail => summary.fail += 1,
            }
        }
        Self {
            mode,
            sections,
            summary,
        }
    }
}

pub(super) async fn run(opts: DoctorOpts) -> Result<()> {
    let report = match opts.swarm {
        Some(swarm) => swarm_report(&swarm, opts.no_probe).await,
        None => machine_report(opts.no_probe).await,
    };

    match opts.output {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        OutputFormat::Human => render_human(&report),
    }

    // Diagnostic-tool convention (flutter/brew): a hard failure is a non-zero
    // exit. `process::exit` rather than returning `Err` so the rendered report
    // is the whole output — no anyhow `Error:` banner tacked on after it.
    if report.summary.fail > 0 {
        std::process::exit(1);
    }
    Ok(())
}

// ---- Mode 1: machine health ------------------------------------------------

async fn machine_report(no_probe: bool) -> Report {
    let mut sections = vec![environment_section(), integrations_section()];
    sections.push(network_section(no_probe).await);
    sections.push(active_swarms_section().await);
    Report::new("machine", sections)
}

fn environment_section() -> Section {
    let checks = vec![
        Check::new("agent-gossip", Verdict::Ok, crate::util::version::VERSION),
        Check::new(
            "platform",
            Verdict::Ok,
            format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH),
        ),
        Check::new(
            "log dir",
            Verdict::Ok,
            output::home_path(&crate::util::logs::log_dir()),
        ),
        Check::new("runtime dir", Verdict::Ok, crate::util::consts::RUNTIME_DIR),
    ];
    Section {
        title: "Environment".to_owned(),
        checks,
    }
}

fn integrations_section() -> Section {
    let checks = match agent::home_dir() {
        Ok(home) => agent::states(&home)
            .into_iter()
            .map(|(agent, path, state)| {
                let verdict = match state {
                    AgentState::UpToDate => Verdict::Ok,
                    AgentState::OutOfDate | AgentState::NotSetUp | AgentState::Absent => {
                        Verdict::Warn
                    }
                };
                let base = format!("{} ({})", state.label(), output::home_path(&path));
                let detail = if state == AgentState::OutOfDate {
                    format!("{base} — run `agent-gossip plug --agent {}`", agent.label())
                } else {
                    base
                };
                Check::new(agent.label(), verdict, detail)
            })
            .collect(),
        Err(error) => vec![Check::new("home", Verdict::Fail, error.to_string())],
    };
    Section {
        title: "Integrations".to_owned(),
        checks,
    }
}

async fn network_section(no_probe: bool) -> Section {
    let checks = if no_probe {
        vec![Check::bare(
            "network probe skipped (--no-probe)",
            Verdict::Warn,
        )]
    } else {
        match lookup::capability_probe(CAPABILITY_TIMEOUT).await {
            Ok(capability) => capability_checks(&capability),
            Err(error) => vec![Check::new(
                "local endpoint",
                Verdict::Fail,
                format!("failed to bind: {error}"),
            )],
        }
    };
    Section {
        title: "Network".to_owned(),
        checks,
    }
}

fn capability_checks(capability: &NetworkCapability) -> Vec<Check> {
    let mut checks = Vec::new();

    let local = if capability.local_addrs.is_empty() {
        capability.node_id.clone()
    } else {
        let addrs = capability
            .local_addrs
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        format!("{} ({addrs})", capability.node_id)
    };
    checks.push(Check::new("local endpoint", Verdict::Ok, local));

    if !capability.report_obtained {
        checks.push(Check::new(
            "net-report",
            Verdict::Warn,
            "no report completed (offline?) — relying on relay/mDNS/DHT",
        ));
        return checks;
    }

    let udp_v4 = capability.udp_v4.unwrap_or(false);
    let udp_v6 = capability.udp_v6.unwrap_or(false);
    let udp_verdict = if udp_v4 || udp_v6 {
        Verdict::Ok
    } else {
        Verdict::Warn
    };
    checks.push(Check::new(
        "UDP",
        udp_verdict,
        format!("IPv4 {}, IPv6 {}", yes_no(udp_v4), yes_no(udp_v6)),
    ));

    let (nat_verdict, nat_detail) = match capability.mapping_varies_by_dest {
        Some(false) => (
            Verdict::Ok,
            "endpoint-independent NAT — direct hole punching supported",
        ),
        Some(true) => (
            Verdict::Warn,
            "address-dependent (symmetric) NAT — direct punch unlikely, traffic relays",
        ),
        None => (Verdict::Warn, "unknown (insufficient probes)"),
    };
    checks.push(Check::new("hole punching", nat_verdict, nat_detail));

    match (&capability.public_v4, &capability.public_v6) {
        (None, None) => checks.push(Check::new(
            "public address",
            Verdict::Warn,
            "none discovered",
        )),
        (v4, v6) => {
            let mut parts = Vec::new();
            if let Some(addr) = v4 {
                parts.push(addr.clone());
            }
            if let Some(addr) = v6 {
                parts.push(addr.clone());
            }
            checks.push(Check::new("public address", Verdict::Ok, parts.join(", ")));
        }
    }

    match &capability.preferred_relay {
        Some(relay) => {
            let detail = match capability.preferred_relay_latency_ms {
                Some(latency) => format!("{relay} ({latency}ms)"),
                None => relay.clone(),
            };
            checks.push(Check::new("relay", Verdict::Ok, detail));
        }
        None => checks.push(Check::new("relay", Verdict::Warn, "no relay reachable")),
    }

    if capability.captive_portal == Some(true) {
        checks.push(Check::new(
            "captive portal",
            Verdict::Warn,
            "detected — HTTP traffic may be intercepted",
        ));
    }

    // Per-protocol port-mapping (UPnP / NAT-PMP / PCP) isn't exposed at this
    // iroh version; the public-address + NAT checks above are the observable
    // signal. Surface that so the absence of a row isn't read as "no mapping".
    checks.push(Check::new(
        "port mapping",
        Verdict::Ok,
        "UPnP/NAT-PMP/PCP not individually probed (see public address)",
    ));

    checks
}

async fn active_swarms_section() -> Section {
    let mut checks = Vec::new();
    for path in ipc::active_socket_paths() {
        // `Info` carries no swarm — the daemon answers with its own identity.
        // A dead/stale socket errors and is skipped.
        let Ok(response) = ipc::send_to_path(&path, &IpcCommand::Info).await else {
            continue;
        };
        let Ok(info) = serde_json::from_str::<InfoResponse>(&response) else {
            continue;
        };
        let detail = format!(
            "{} · <{}> · {} {}",
            info.swarm,
            info.nickname,
            info.participant_count,
            plural(info.participant_count, "member", "members"),
        );
        checks.push(Check::new(format!("#{}", info.name), Verdict::Ok, detail));
    }
    if checks.is_empty() {
        checks.push(Check::bare("no active swarms on this machine", Verdict::Ok));
    }
    Section {
        title: "Active swarms".to_owned(),
        checks,
    }
}

#[derive(serde::Deserialize)]
struct InfoResponse {
    swarm: String,
    name: String,
    nickname: String,
    participant_count: usize,
}

// ---- Mode 2: per-swarm connection analysis ---------------------------------

async fn swarm_report(swarm_id: &SwarmId, no_probe: bool) -> Report {
    let swarm = match swarm_id.as_str().parse::<Swarm>() {
        Ok(swarm) => swarm,
        Err(error) => {
            let section = Section {
                title: "Swarm".to_owned(),
                checks: vec![Check::new(
                    "decode",
                    Verdict::Fail,
                    format!("could not decode swarm id: {error}"),
                )],
            };
            return Report::new("swarm", vec![section]);
        }
    };

    let mut sections = vec![
        swarm_identity_section(&swarm),
        declared_methods_section(&swarm),
    ];
    // The rendezvous identity and port ladder of a passworded swarm derive
    // from the stretched key, which doctor doesn't hold — probing the
    // password-less derivations would analyze a swarm that doesn't exist.
    if !no_probe && !swarm.requires_password() {
        sections.push(live_reachability_section(&swarm).await);
    }
    Report::new("swarm", sections)
}

fn swarm_identity_section(swarm: &Swarm) -> Section {
    let mut checks = vec![
        Check::new("name", Verdict::Ok, format!("#{}", swarm.name.as_str())),
        Check::new("network", Verdict::Ok, swarm.network_label()),
    ];
    if swarm.requires_password() {
        checks.push(Check::new(
            "password",
            Verdict::Ok,
            "protected — joining and the rendezvous derivations need --password",
        ));
    }
    Section {
        title: "Swarm".to_owned(),
        checks,
    }
}

fn declared_methods_section(swarm: &Swarm) -> Section {
    let lookups = swarm.lookups();
    let mut checks = Vec::new();

    match &lookups.relay {
        RelayChoice::Disabled => {
            checks.push(Check::new("relay", Verdict::Ok, "disabled"));
        }
        RelayChoice::Pinned => {
            let rungs = lookup::relay_ladder(&lookups.relay)
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            checks.push(Check::new(
                "relay",
                Verdict::Ok,
                format!("pinned ladder: {rungs}"),
            ));
        }
        RelayChoice::Custom(ladder) => {
            let rungs = ladder
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            checks.push(Check::new(
                "relay",
                Verdict::Ok,
                format!("custom ladder: {rungs}"),
            ));
        }
    }

    checks.push(Check::new(
        "mDNS (LAN)",
        Verdict::Ok,
        if lookups.mdns { "enabled" } else { "disabled" },
    ));
    checks.push(Check::new(
        "mainline DHT",
        Verdict::Ok,
        if lookups.dht { "enabled" } else { "disabled" },
    ));

    if swarm.is_loopback() {
        // The ladder of a passworded swarm derives from the stretched key,
        // which doctor doesn't hold.
        if swarm.requires_password() {
            checks.push(Check::new(
                "loopback ports",
                Verdict::Ok,
                "password-derived — need --password to compute",
            ));
        } else {
            let ports = swarm
                .rendezvous_ports()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            checks.push(Check::new(
                "loopback ports",
                Verdict::Ok,
                format!("127.0.0.1 ladder: {ports}"),
            ));
        }
    }

    Section {
        title: "Declared methods".to_owned(),
        checks,
    }
}

async fn live_reachability_section(swarm: &Swarm) -> Section {
    let lookups = swarm.lookups();
    let mut checks = Vec::new();

    // Relay rungs — show the whole ladder's health, not just the first pick.
    let ladder = lookup::relay_ladder(&lookups.relay);
    let mut first_reachable = None;
    if !ladder.is_empty() {
        for (rung, reachable) in lookup::probe_ladder(&ladder, RUNG_TIMEOUT).await {
            if reachable && first_reachable.is_none() {
                first_reachable = Some(rung.clone());
            }
            let verdict = if reachable {
                Verdict::Ok
            } else {
                Verdict::Warn
            };
            checks.push(Check::new(
                "relay rung",
                verdict,
                format!("{rung} {}", if reachable { "online" } else { "offline" }),
            ));
        }
    }

    // Rendezvous reachability + the resulting path type (direct vs relay).
    match lookup::build_participant_endpoint(lookups).await {
        Ok(endpoint) => {
            let rendezvous_id = swarm.rendezvous_id();
            let mut addr = iroh::EndpointAddr::new(rendezvous_id);
            if swarm.is_loopback() {
                for port in swarm.rendezvous_ports() {
                    addr = addr.with_ip_addr(std::net::SocketAddr::from((
                        std::net::Ipv4Addr::LOCALHOST,
                        port,
                    )));
                }
            } else if let Some(rung) = first_reachable {
                addr = addr.with_relay_url(rung);
            }

            let reached = lookup::probe_connect(&endpoint, addr, RENDEZVOUS_TIMEOUT).await;
            if reached {
                let (path, relay) = crate::gossip::conn_path(&endpoint, rendezvous_id).await;
                let detail = match relay {
                    Some(url) => format!("reachable — {path} path (relay {url})"),
                    None => format!("reachable — {path} path"),
                };
                checks.push(Check::new("rendezvous", Verdict::Ok, detail));
            } else {
                checks.push(Check::new(
                    "rendezvous",
                    Verdict::Warn,
                    "not reachable — peers must resolve via mDNS/DHT",
                ));
            }
            endpoint.close().await;
        }
        Err(error) => {
            checks.push(Check::new(
                "endpoint",
                Verdict::Fail,
                format!("failed to bind: {error}"),
            ));
        }
    }

    Section {
        title: "Live reachability".to_owned(),
        checks,
    }
}

// ---- rendering -------------------------------------------------------------

fn render_human(report: &Report) {
    for section in &report.sections {
        let header = Style::new().bold();
        anstream::println!("\n{header}{}{header:#}", section.title);
        for check in &section.checks {
            let (symbol, color) = check.verdict.symbol();
            let marked = Style::new().fg_color(Some(color.into())).bold();
            match &check.detail {
                Some(detail) => {
                    anstream::println!("  [{marked}{symbol}{marked:#}] {} — {detail}", check.name);
                }
                None => {
                    anstream::println!("  [{marked}{symbol}{marked:#}] {}", check.name);
                }
            }
        }
    }
    let summary = &report.summary;
    anstream::println!(
        "\n{} ok, {} {}, {} {}",
        summary.ok,
        summary.warn,
        plural(summary.warn, "warning", "warnings"),
        summary.fail,
        plural(summary.fail, "failure", "failures"),
    );
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn plural(count: usize, singular: &'static str, plural: &'static str) -> &'static str {
    if count == 1 { singular } else { plural }
}
