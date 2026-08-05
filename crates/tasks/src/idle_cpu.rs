use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

use xshell::{Shell, cmd};

use crate::TaskOutcome;

/// The log configuration the subjects run under.
///
/// `RUST_LOG` *replaces* the daemon's default `EnvFilter` rather than adding
/// to it (`logging::log_filter` takes `try_from_default_env` first), so naming
/// `netwatch=debug` alone would silence every one of our own pinned subsystems
/// and measure a daemon that logs less than a shipped one does. This restates
/// the release-build default and appends to it: `netwatch=debug` for the
/// enumeration counter, `lookup=info` for the endpoint counter.
const RUST_LOG: &str = "error,noq_proto::connection=off,mainline::rpc=off,\
    fofoca::gossip=info,\
    fofoca::lookup=info,\
    fofoca::beacon=info,\
    fofoca::lifecycle=info,\
    fofoca::directory=info,\
    fofoca::messages=info,\
    netwatch=debug";

/// CPU charged to one process, split by where it went. The split is the
/// cheapest attribution there is: a daemon whose cost is `system` is paying
/// syscalls and sockets, one whose cost is `user` is paying serialization,
/// crypto and allocation. Ranking the two before profiling narrows the search
/// by half.
#[derive(Clone, Copy, Default, Debug)]
struct Cpu {
    user: f64,
    system: f64,
}

impl Cpu {
    fn total(self) -> f64 {
        self.user + self.system
    }

    fn since(self, start: Self) -> Self {
        Self {
            user: self.user - start.user,
            system: self.system - start.system,
        }
    }
}

/// Where the daemon's event loop spent its wakeups, summed from the
/// `mesh census` lines it wrote.
///
/// This is the whole point of the exercise. CPU percentage says a daemon costs
/// something; it never says *which* of the eight maintenance tickers bought it.
/// The daemon emits these as per-interval deltas on a line it already wrote at
/// `info` on a pinned target, so they cost nothing to collect and survive a
/// release build.
///
/// `lines` is load-bearing beyond curiosity: the census fires on a fixed
/// cadence, so its count is an independent witness that the daemon was alive
/// and ticking for the whole window — the one thing a CPU delta cannot tell you
/// apart from a daemon that died early and stopped spending any.
#[derive(Clone, Copy, Default, Debug)]
struct Census {
    lines: u64,
    wakeups: u64,
    prune: u64,
    alive: u64,
    sweep: u64,
    heal: u64,
    reclaim: u64,
    antientropy: u64,
    state_refresh: u64,
    linkstate: u64,
    external: u64,
    broadcasts: u64,
}

impl Census {
    fn since(self, start: Self) -> Self {
        Self {
            lines: self.lines - start.lines,
            wakeups: self.wakeups - start.wakeups,
            prune: self.prune - start.prune,
            alive: self.alive - start.alive,
            sweep: self.sweep - start.sweep,
            heal: self.heal - start.heal,
            reclaim: self.reclaim - start.reclaim,
            antientropy: self.antientropy - start.antientropy,
            state_refresh: self.state_refresh - start.state_refresh,
            linkstate: self.linkstate - start.linkstate,
            external: self.external - start.external,
            broadcasts: self.broadcasts - start.broadcasts,
        }
    }
}

/// One measurement subject: a whole mesh sharing a lookup/tuning
/// configuration. The unit of comparison is the variant, not the node —
/// per-node figures exist to show the spread within one.
#[derive(Debug)]
struct Variant {
    label: String,
    args: Vec<String>,
    /// Overrides the run-wide `--nodes`. Mesh size is the one dimension worth
    /// varying *within* a run rather than across runs: the absolute level
    /// drifts with ambient churn between runs, so a slope built from three
    /// separate invocations measures the weather as much as the mesh.
    nodes: Option<usize>,
    /// A prebuilt binary to run this variant from, instead of the one just
    /// built. This is how a fix gets validated: patched and stock side by
    /// side, under one ambient stream, exactly as the netwatch `RTM_MISS` fix
    /// was measured. It is also the only way to ablate a knob that is a
    /// `const` with no CLI override.
    binary: Option<PathBuf>,
}

/// How long to run for and at what mesh size — the three knobs that decide
/// what the run can resolve, kept together because no caller sets one without
/// the others.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Plan {
    pub settle: u64,
    pub window: u64,
    pub nodes: usize,
}

/// Where one daemon belongs: which binary, which scratch root, and its
/// (variant, index) address within the run.
#[derive(Clone, Copy, Debug)]
struct Site<'a> {
    binary: &'a Path,
    base: &'a Path,
    variant: usize,
    index: usize,
}

/// A daemon under measurement, plus where to read its cost from.
#[derive(Debug)]
struct Node {
    variant: usize,
    index: usize,
    child: Child,
    log_dir: PathBuf,
    state_file: PathBuf,
    cpu_start: Cpu,
    enums_start: u64,
    census_start: Census,
}

/// Measure what an idle daemon costs, and record the conditions that set that
/// cost.
///
/// The headline number is not reproducible on its own. Idle CPU here is driven
/// substantially by route/netlink traffic the host generates for its own
/// reasons: iroh runs a `netwatch` netmon per endpoint, and every message it
/// accepts costs a full `netdev` interface enumeration. Measured twice twenty
/// minutes apart on one unchanged machine, the same daemon read 1.78% and
/// 2.78% purely because ambient churn had doubled. So the event rate and the
/// interface count are reported *beside* the CPU figure — a before/after that
/// does not hold both constant is comparing two different experiments.
///
/// Every variant runs **concurrently**, under one ambient event stream, for
/// exactly that reason: a delta between two variants measured at the same
/// moment is a real delta, one measured an hour apart is not. Pass the same
/// variant twice to get a control pair, and treat its spread as the noise
/// floor any claimed effect has to clear.
pub(crate) fn run(sh: &Shell, plan: Plan, variants: &[String], binaries: &[String]) -> TaskOutcome {
    // `ci`, not `release`: same optimization level, but it keeps symbols, so a
    // `sample`/`perf` taken against this build attributes frames instead of `???`.
    cmd!(sh, "cargo build --profile ci -p agent-gossip").run()?;
    let binary = sh.current_dir().join("target/ci/agent-gossip");

    let mut variants = parse_variants(variants)?;
    reject_duplicate_labels(&variants)?;
    attach_binaries(&mut variants, binaries)?;
    let Plan {
        settle,
        window,
        nodes,
    } = plan;
    let nodes = nodes.max(1);

    let base = std::env::temp_dir().join(format!("agent-gossip-idle-cpu-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base)?;

    let mut running = spawn_meshes(&binary, &base, &variants, nodes)?;

    println!("settling {settle}s");
    sleep(Duration::from_secs(settle));
    check_alive(&variants, &mut running, "settle")?;
    // Sampled in one `ps` for every pid at once: a fork per node would stagger
    // the nodes' windows by the fork cost, which is exactly the kind of
    // per-node skew the spread column is supposed to be reporting.
    let starts = cpu_of_all(&running);
    for (node, cpu) in running.iter_mut().zip(starts) {
        node.cpu_start = cpu;
        node.enums_start = enumerations(&node.log_dir);
        node.census_start = census(&node.log_dir);
    }

    let ambient_log = base.join("ambient.txt");
    let mut monitor = ambient_monitor(&ambient_log);

    println!("measuring {window}s");
    sleep(Duration::from_secs(window));
    if let Some(monitor) = monitor.as_mut() {
        let _ = monitor.kill();
        let _ = monitor.wait();
    }
    // Before reading any cost. A daemon that died mid-window stops spending
    // CPU, so its delta collapses and the table reports the crash as the
    // cheapest variant in the run — the one failure mode a benchmark must
    // never render as a win.
    check_alive(&variants, &mut running, "the measurement window")?;

    let elapsed = f64::from(u32::try_from(window).unwrap_or(u32::MAX));
    println!("\n=== idle cost over {window}s, {nodes} node(s) per variant ===");
    report_host(&ambient_log, monitor.is_some(), elapsed);
    report(&variants, &mut running, elapsed);

    for node in &mut running {
        let _ = node.child.kill();
        let _ = node.child.wait();
    }
    let _ = fs::remove_dir_all(&base);
    Ok(())
}

/// `label=arg arg …`, repeatable. Bare `label` means a variant with no extra
/// flags, which is how a control is written. Defaults to the pair the netwatch
/// investigation used: loopback against public isolates whether the public
/// lookup preset (portmapper, mDNS, DHT) is what costs.
fn parse_variants(specs: &[String]) -> Result<Vec<Variant>, Box<dyn std::error::Error>> {
    if specs.is_empty() {
        return Ok(vec![
            Variant {
                label: "loopback".to_owned(),
                args: Vec::new(),
                nodes: None,
                binary: None,
            },
            Variant {
                label: "public".to_owned(),
                args: vec!["--public".to_owned()],
                nodes: None,
                binary: None,
            },
        ]);
    }
    specs
        .iter()
        .map(|spec| {
            let (head, args) = spec.split_once('=').unwrap_or((spec.as_str(), ""));
            let (label, nodes) = match head.split_once('@') {
                Some((label, count)) => {
                    let parsed = count
                        .parse::<usize>()
                        .map_err(|_| format!("variant {spec:?}: {count:?} is not a node count"))?;
                    (label, Some(parsed.max(1)))
                }
                None => (head, None),
            };
            if label.is_empty() {
                return Err(format!("variant {spec:?} has an empty label").into());
            }
            Ok(Variant {
                label: label.to_owned(),
                args: args.split_whitespace().map(str::to_owned).collect(),
                nodes,
                binary: None,
            })
        })
        .collect()
}

/// Labels are the key for `--binary` and the only identifier in the report
/// table, so two variants may not share one. That collides with the natural way
/// to write a control pair — naming the same variant twice — hence the hint:
/// `loopback-a` / `loopback-b` are still a control pair (identical flags), and
/// stay individually addressable.
fn reject_duplicate_labels(variants: &[Variant]) -> Result<(), Box<dyn std::error::Error>> {
    for (index, variant) in variants.iter().enumerate() {
        if variants[..index]
            .iter()
            .any(|prior| prior.label == variant.label)
        {
            return Err(format!(
                "variant label {:?} is used twice; labels key `--binary` and \
                 name the report rows, so a control pair needs distinct labels \
                 (e.g. {0}-a and {0}-b with the same flags)",
                variant.label
            )
            .into());
        }
    }
    Ok(())
}

/// Resolve `--binary label=path` against the parsed variants. Keyed by label
/// rather than by position so the pairing survives reordering the variants,
/// and unknown labels are an error rather than a silently ignored flag — a
/// typo here would otherwise measure stock against stock and report it as a
/// fix that changed nothing.
fn attach_binaries(
    variants: &mut [Variant],
    binaries: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    for spec in binaries {
        let (label, path) = spec
            .split_once('=')
            .ok_or_else(|| format!("--binary {spec:?} is not `label=path`"))?;
        let path = PathBuf::from(path);
        if !path.is_file() {
            return Err(format!("--binary {spec:?}: {} is not a file", path.display()).into());
        }
        let variant = variants
            .iter_mut()
            .find(|variant| variant.label == label)
            .ok_or_else(|| format!("--binary {spec:?}: no variant labelled {label:?}"))?;
        variant.binary = Some(path);
    }
    Ok(())
}

/// Stand up every variant's mesh, creators first and all at once, so the
/// variants stay in lockstep and share one settle window. The gossip id comes
/// back from `agent-gossip ready`, which doubles as the convergence gate — the
/// formation transient must never land inside the measurement window.
fn spawn_meshes(
    binary: &Path,
    base: &Path,
    variants: &[Variant],
    nodes: usize,
) -> Result<Vec<Node>, Box<dyn std::error::Error>> {
    let mut running = Vec::new();
    let mut meshes = Vec::new();

    for (variant, spec) in variants.iter().enumerate() {
        let extra: Vec<&str> = spec.args.iter().map(String::as_str).collect();
        let site = Site {
            binary: spec.binary.as_deref().unwrap_or(binary),
            base,
            variant,
            index: 0,
        };
        running.push(spawn(&site, "create", &extra)?);
    }
    for node in &running {
        // The `ready` gate only reads a state file, so the freshly built
        // binary can gate a variant running from a different one.
        meshes.push(ready(binary, &node.state_file)?);
    }

    for (variant, spec) in variants.iter().enumerate() {
        // The lookup set is baked into the gossip id, so a joiner inherits the
        // variant's discovery config; only the tuning flags need repeating.
        let extra: Vec<&str> = spec
            .args
            .iter()
            .map(String::as_str)
            .filter(|arg| !matches!(*arg, "--public" | "--mdns" | "--dht" | "--relay"))
            .collect();
        for index in 1..spec.nodes.unwrap_or(nodes) {
            let mut args = vec![meshes[variant].as_str()];
            args.extend_from_slice(&extra);
            let site = Site {
                binary: spec.binary.as_deref().unwrap_or(binary),
                base,
                variant,
                index,
            };
            running.push(spawn(&site, "join", &args)?);
        }
    }
    for node in &running {
        ready(binary, &node.state_file)?;
    }
    Ok(running)
}

fn spawn(
    site: &Site<'_>,
    command: &str,
    extra: &[&str],
) -> Result<Node, Box<dyn std::error::Error>> {
    let Site {
        binary,
        base,
        variant,
        index,
    } = *site;
    let log_dir = base.join(format!("logs-{variant}-{index}"));
    let state_file = base.join(format!("{variant}-{index}.state.json"));
    let child = Command::new(binary)
        .arg(command)
        .args(extra)
        .args(["--nickname", &format!("idle-{variant}-{index}")])
        .arg("--log-dir")
        .arg(&log_dir)
        .arg("--state-file")
        .arg(&state_file)
        // Every counter here is read by grepping the log, and the sink rotates
        // to `<file>.1` at `LOG_FILE_MAX_BYTES` (10 MiB) — which `find_log`
        // does not follow. A rotation mid-run would silently drop the rotated
        // half of the enumerations and censuses, i.e. read as an improvement.
        // `0` disables rotation for the run.
        .args(["--log-max-bytes", "0"])
        .env("RUST_LOG", RUST_LOG)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(Node {
        variant,
        index,
        child,
        log_dir,
        state_file,
        cpu_start: Cpu::default(),
        enums_start: 0,
        census_start: Census::default(),
    })
}

/// Block until the daemon serves, and return the mesh id it is serving. Both
/// come from the same `ready` call because the gate already prints the
/// identity — parsing the state file separately would be a second source of
/// truth for the same fact.
fn ready(binary: &Path, state_file: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let out = Command::new(binary)
        .arg("ready")
        .arg("--state-file")
        .arg(state_file)
        .args(["--output", "json", "--timeout-secs", "120"])
        .output()?;
    if !out.status.success() {
        return Err(format!("daemon never became ready: {}", state_file.display()).into());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.split_once("\"gossip\":\"")
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(id, _)| id.to_owned())
        .ok_or_else(|| format!("no gossip id in ready output: {text}").into())
}

fn node_label(variants: &[Variant], node: &Node) -> String {
    format!("{}/{}", variants[node.variant].label, node.index)
}

/// Fail the run if any daemon has exited. Called on both edges of the window:
/// a corpse reports near-zero CPU, which is indistinguishable from a fix.
fn check_alive(
    variants: &[Variant],
    running: &mut [Node],
    phase: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    for node in running.iter_mut() {
        if let Some(status) = node.child.try_wait()? {
            let label = node_label(variants, node);
            return Err(format!("{label} exited during {phase}: {status}").into());
        }
    }
    Ok(())
}

/// The daemon's census cadence (`STATE_REFRESH_SECS`) — the divisor for the
/// expected census count. A `const` here rather than a read of the engine's,
/// because `tasks` deliberately does not depend on `fofoca`; if the
/// engine's cadence changes, the census-coverage check goes loud, which is the
/// intended way to find out.
const CENSUS_INTERVAL_SECS: f64 = 10.0;

fn report(variants: &[Variant], running: &mut [Node], elapsed: f64) {
    let cpus = cpu_of_all(running);
    println!(
        "\n{:<14} {:>5} {:>8} {:>8} {:>8} {:>8} {:>7} {:>5}",
        "variant", "node", "cpu(s)", "user", "sys", "%core", "enums", "endp"
    );
    for (variant, spec) in variants.iter().enumerate() {
        let mut totals = Vec::new();
        for (node, cpu) in running.iter().zip(&cpus) {
            if node.variant != variant {
                continue;
            }
            let cpu = cpu.since(node.cpu_start);
            let enums = enumerations(&node.log_dir).saturating_sub(node.enums_start);
            totals.push(cpu.total());
            println!(
                "{:<14} {:>5} {:>8.2} {:>8.2} {:>8.2} {:>7.2}% {:>7} {:>5}",
                spec.label,
                node.index,
                cpu.total(),
                cpu.user,
                cpu.system,
                100.0 * cpu.total() / elapsed,
                enums,
                endpoints(&node.log_dir),
            );
        }
        if totals.len() > 1 {
            let count = f64::from(u32::try_from(totals.len()).unwrap_or(u32::MAX));
            let mean = totals.iter().sum::<f64>() / count;
            let low = totals.iter().copied().fold(f64::MAX, f64::min);
            let high = totals.iter().copied().fold(f64::MIN, f64::max);
            println!(
                "{:<14} {:>5} {:>8.2} {:>8} {:>8} {:>7.2}%  spread {:.2}s",
                spec.label,
                "mean",
                mean,
                "",
                "",
                100.0 * mean / elapsed,
                high - low,
            );
        }
    }

    report_wakeups(variants, running, elapsed);

    println!(
        "\nA claimed effect must clear the spread of a control pair run in the\n\
         same batch; ambient churn moves these numbers more than most fixes do.\n\
         Variants are concurrent, which holds ambient churn constant but does\n\
         NOT make them independent: `--public` variants share the host's mDNS\n\
         multicast group and gateway portmapper, so each public variant added\n\
         inflates every other one. Compare like against like within a batch."
    );
}

/// Where the wakeups went, per minute so the numbers are comparable across
/// window lengths. This is the table the CPU one cannot replace: a `%core`
/// figure is the *sum* of everything below, and nothing in it says which row
/// to attack first.
fn report_wakeups(variants: &[Variant], running: &[Node], elapsed: f64) {
    let per_min = |count: u64| 60.0 * f64::from(u32::try_from(count).unwrap_or(u32::MAX)) / elapsed;
    println!(
        "\n{:<14} {:>5} {:>7} {:>8} {:>6} {:>6} {:>8} {:>6} {:>6} {:>7} {:>5} {:>6}",
        "variant",
        "node",
        "wake",
        "reclaim",
        "sweep",
        "antien",
        "refresh",
        "heal",
        "alive",
        "lnkstat",
        "ext",
        "bcast",
    );
    println!("{:<14} {:>5} {:>7}", "", "", "per minute");
    for (variant, spec) in variants.iter().enumerate() {
        for node in running.iter().filter(|node| node.variant == variant) {
            let seen = census(&node.log_dir).since(node.census_start);
            println!(
                "{:<14} {:>5} {:>7.0} {:>8.0} {:>6.0} {:>6.0} {:>8.0} {:>6.0} {:>6.0} {:>7.0} {:>5.0} {:>6.0}",
                spec.label,
                node.index,
                per_min(seen.wakeups),
                per_min(seen.reclaim),
                per_min(seen.sweep),
                per_min(seen.antientropy),
                per_min(seen.state_refresh),
                per_min(seen.heal),
                per_min(seen.alive),
                per_min(seen.linkstate),
                per_min(seen.external),
                per_min(seen.broadcasts),
            );
            warn_census_coverage(spec, node, seen, elapsed);
        }
    }
}

/// The census fires on a fixed cadence, so its count over the window is a
/// second, independent witness that the daemon ticked throughout — one that
/// does not go quiet just because a process stopped consuming CPU. A short
/// count means the loop stalled (or the window straddled a restart), and every
/// per-minute figure above it is then an average over a shorter live period
/// than the window claims.
fn warn_census_coverage(spec: &Variant, node: &Node, seen: Census, elapsed: f64) {
    let expected = elapsed / CENSUS_INTERVAL_SECS;
    let actual = f64::from(u32::try_from(seen.lines).unwrap_or(u32::MAX));
    if expected >= 2.0 && actual < 0.8 * expected {
        println!(
            "  !! {}/{}: {actual:.0} census lines, expected ~{expected:.0} — the \
             event loop stalled during the window; treat the rates above as \
             unattributed.",
            spec.label, node.index,
        );
    }
}

/// The host-wide routing-event stream, observed without billing anything to
/// the daemons. `route -n monitor` on BSD/macOS, `ip monitor` on Linux — the
/// netmon subscribes to the same feed, and an accepted message is what buys an
/// interface enumeration.
fn ambient_monitor(log: &Path) -> Option<Child> {
    let file = fs::File::create(log).ok()?;
    let mut command = if cfg!(target_os = "linux") {
        let mut command = Command::new("ip");
        command.args(["monitor", "all"]);
        command
    } else {
        let mut command = Command::new("route");
        command.args(["-n", "monitor"]);
        command
    };
    command
        .stdout(Stdio::from(file))
        .stderr(Stdio::null())
        .spawn()
        .ok()
}

fn report_host(ambient_log: &Path, monitored: bool, elapsed: f64) {
    println!("host: {} interfaces", interface_count());

    if !monitored {
        println!("ambient: unavailable (no route/netlink monitor on this platform)");
        return;
    }
    let text = fs::read_to_string(ambient_log).unwrap_or_default();
    if cfg!(target_os = "linux") {
        let total = text.lines().filter(|line| !line.trim().is_empty()).count();
        let rate = f64::from(u32::try_from(total).unwrap_or(u32::MAX)) / elapsed;
        println!("netlink: {total} msgs ({rate:.2}/s)");
        return;
    }
    let total = text.lines().filter(|line| line.starts_with("RTM_")).count();
    let misses = text
        .lines()
        .filter(|line| line.starts_with("RTM_MISS"))
        .count();
    // netwatch drops link-local destinations, and since the pinned fork it
    // drops `RTM_MISS` outright — that message reports the fate of a packet,
    // not a change to the routing table. What survives both filters is what
    // buys an interface enumeration 250ms later, so `accepted` counts only
    // that. On a host whose churn is all `RTM_MISS` it should now read 0, and
    // the `enums` column should agree.
    // Single pass, no lookahead that consumes: the address line is examined by
    // peeking at the *next* iteration's line instead of pulling it out of the
    // iterator. Pulling it desynchronized the `RTM_` state — whenever the line
    // after a `sockaddrs: <DST>` was itself a message header, it was swallowed
    // without updating `in_miss`, and the following message was classified
    // under its predecessor's verdict.
    let mut accepted = 0;
    let mut in_miss = false;
    let mut pending_dst = false;
    for line in text.lines() {
        if pending_dst {
            pending_dst = false;
            if !line.trim_start().starts_with("fe80:") {
                accepted += 1;
            }
        }
        if line.starts_with("RTM_") {
            in_miss = line.starts_with("RTM_MISS");
        }
        if !in_miss && line.contains("sockaddrs: <DST>") {
            pending_dst = true;
        }
    }
    let rate = f64::from(u32::try_from(total).unwrap_or(u32::MAX)) / elapsed;
    println!(
        "route: {total} msgs ({rate:.2}/s) | RTM_MISS {misses}, other {} | accepted {accepted}",
        total - misses,
    );
}

fn interface_count() -> usize {
    if cfg!(target_os = "linux") {
        return fs::read_dir("/sys/class/net")
            .map(|entries| entries.flatten().count())
            .unwrap_or_default();
    }
    Command::new("ifconfig")
        .arg("-l")
        .output()
        .map_or(0, |out| {
            String::from_utf8_lossy(&out.stdout)
                .split_whitespace()
                .count()
        })
}

/// Cumulative CPU for every node, user and system apart, in `running` order.
///
/// One `ps` for all pids rather than one per node: the sampling loop runs at
/// both window edges, and a fork per node puts each node's window a few
/// milliseconds after the last one's — skew that lands in the very
/// within-variant spread this harness asks you to read as the noise floor.
///
/// Linux reads `/proc`, not `ps`, and so has nothing to batch: `ps -o time=`
/// there is whole seconds, which against a ~1%-of-a-core signal over a
/// two-minute window is a quantum the same size as the measurement.
/// `/proc/<pid>/stat` counts in `USER_HZ` ticks — 10ms, a hundred times finer.
/// macOS `ps` reports centiseconds and splits user from system directly.
fn cpu_of_all(running: &[Node]) -> Vec<Cpu> {
    if cfg!(target_os = "linux") {
        return running
            .iter()
            .map(|node| proc_stat_cpu(node.child.id()).unwrap_or_default())
            .collect();
    }
    let pids: Vec<String> = running
        .iter()
        .map(|node| node.child.id().to_string())
        .collect();
    let Ok(out) = Command::new("ps")
        .args(["-o", "pid=,utime=,stime=", "-p", &pids.join(",")])
        .output()
    else {
        return vec![Cpu::default(); running.len()];
    };
    // `ps` orders by pid, not by the order asked for, so map back explicitly.
    let text = String::from_utf8_lossy(&out.stdout);
    let by_pid: Vec<(u32, Cpu)> = text
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            Some((
                pid,
                Cpu {
                    user: fields.next().map(parse_ps_time).unwrap_or_default(),
                    system: fields.next().map(parse_ps_time).unwrap_or_default(),
                },
            ))
        })
        .collect();
    running
        .iter()
        .map(|node| {
            by_pid
                .iter()
                .find(|(pid, _)| *pid == node.child.id())
                .map(|(_, cpu)| *cpu)
                .unwrap_or_default()
        })
        .collect()
}

/// `[[HH:]MM:]SS.ss` — `ps` grows a leading field as the value passes a minute
/// and again past an hour, so scale from the right.
fn parse_ps_time(field: &str) -> f64 {
    field
        .trim()
        .rsplit(':')
        .zip([1.0, 60.0, 3600.0])
        .map(|(part, scale)| part.parse::<f64>().unwrap_or(0.0) * scale)
        .sum()
}

fn proc_stat_cpu(pid: u32) -> Option<Cpu> {
    let text = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // Field 2 is the executable name in parentheses and may itself contain
    // spaces or parens, so the only safe split point is the *last* `)`. What
    // follows starts at field 3, putting utime at index 11 and stime at 12.
    let rest = text.rsplit_once(')')?.1;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let ticks = clock_ticks();
    Some(Cpu {
        user: fields.get(11)?.parse::<f64>().ok()? / ticks,
        system: fields.get(12)?.parse::<f64>().ok()? / ticks,
    })
}

/// `USER_HZ`, the ABI constant `/proc` counts in. Fixed at 100 on every Linux
/// port we run on regardless of the kernel's own tick rate, but asked rather
/// than assumed.
fn clock_ticks() -> f64 {
    Command::new("getconf")
        .arg("CLK_TCK")
        .output()
        .ok()
        .and_then(|out| String::from_utf8_lossy(&out.stdout).trim().parse().ok())
        .unwrap_or(100.0)
}

/// netmon logs `no changes detected` once per completed enumeration, so
/// counting that line prices the work without patching netwatch. The line
/// costs a write per enumeration, which slightly inflates CPU — acceptable,
/// since it is the only way to attribute the cost at all.
fn enumerations(log_dir: &Path) -> u64 {
    count_lines(log_dir, "no changes detected")
}

/// Live iroh endpoints this daemon stands up. Not one per node: a member that
/// co-hosts the rendezvous binds a *second* full endpoint, and every
/// per-endpoint background cost — mDNS discoverer, netmon, relay connection,
/// portmapper — is paid again by that node. Reported because it is the
/// multiplier that explains why two nodes in one mesh can differ by half.
fn endpoints(log_dir: &Path) -> u64 {
    count_lines(log_dir, "endpoint bound")
}

/// Sum the `idle_*` deltas across every `mesh census` line in the daemon's log.
///
/// Fields are matched by `key=` anywhere in the line rather than by position:
/// the subscriber's field order is not a contract, and no key here is a prefix
/// of another once the `=` is included.
fn census(log_dir: &Path) -> Census {
    let Some(log) = find_log(log_dir) else {
        return Census::default();
    };
    let Ok(text) = fs::read_to_string(log) else {
        return Census::default();
    };
    let mut total = Census::default();
    for line in text.lines().filter(|line| line.contains("mesh census")) {
        total.lines += 1;
        total.wakeups += field(line, "idle_wakeups");
        total.prune += field(line, "idle_prune");
        total.alive += field(line, "idle_alive");
        total.sweep += field(line, "idle_sweep");
        total.heal += field(line, "idle_heal");
        total.reclaim += field(line, "idle_reclaim");
        total.antientropy += field(line, "idle_antientropy");
        total.state_refresh += field(line, "idle_state_refresh");
        total.linkstate += field(line, "idle_linkstate");
        total.external += field(line, "idle_external");
        total.broadcasts += field(line, "idle_broadcasts");
    }
    total
}

fn field(line: &str, key: &str) -> u64 {
    line.split_once(&format!("{key}="))
        .map(|(_, rest)| {
            rest.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
        })
        .and_then(|digits| digits.parse().ok())
        .unwrap_or(0)
}

fn count_lines(log_dir: &Path, needle: &str) -> u64 {
    let Some(log) = find_log(log_dir) else {
        return 0;
    };
    fs::read_to_string(log).map_or(0, |text| {
        text.lines().filter(|line| line.contains(needle)).count() as u64
    })
}

fn find_log(dir: &Path) -> Option<PathBuf> {
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_log(&path) {
                return Some(found);
            }
        } else if path.to_string_lossy().ends_with(".tracing.log") {
            return Some(path);
        }
    }
    None
}
