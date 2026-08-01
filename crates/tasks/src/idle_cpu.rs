use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

use xshell::{Shell, cmd};

use crate::TaskOutcome;

/// A daemon under measurement, plus where to read its cost from.
struct Subject {
    label: &'static str,
    child: Child,
    log_dir: PathBuf,
    cpu_start: f64,
    enums_start: u64,
}

/// Measure what an idle daemon costs, and record the conditions that set that
/// cost.
///
/// The headline number is not reproducible on its own. Idle CPU here is driven
/// almost entirely by `AF_ROUTE` traffic the host generates for its own
/// reasons: iroh runs a `netwatch` netmon per endpoint, and every route message
/// it accepts costs a full `netdev` interface enumeration. Measured twice
/// twenty minutes apart on one unchanged machine, the same daemon read 1.78%
/// and 2.78% purely because ambient route churn had doubled. So the route rate
/// and the interface count are reported *beside* the CPU figure — a before/after
/// that does not hold both constant is comparing two different experiments.
///
/// Runs a loopback-only and a `--public` daemon side by side, because the
/// difference between them is the only way to see whether the public lookup
/// preset (portmapper, mDNS, DHT) is contributing.
pub(crate) fn run(sh: &Shell, settle: u64, window: u64) -> TaskOutcome {
    // `ci`, not `release`: same optimization level, but it keeps symbols, so a
    // `sample` taken against this build attributes frames instead of `???`.
    cmd!(sh, "cargo build --profile ci -p agent-gossip").run()?;
    let binary = sh.current_dir().join("target/ci/agent-gossip");

    let base = std::env::temp_dir().join(format!("agent-gossip-idle-cpu-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base)?;

    let mut subjects = vec![
        spawn(&binary, &base, "loopback", &[])?,
        spawn(&binary, &base, "public", &["--public"])?,
    ];

    println!("settling {settle}s");
    sleep(Duration::from_secs(settle));
    for subject in &mut subjects {
        if let Some(status) = subject.child.try_wait()? {
            return Err(format!("{} daemon exited during settle: {status}", subject.label).into());
        }
        subject.cpu_start = cpu_seconds(subject.child.id());
        subject.enums_start = enumerations(&subject.log_dir);
    }

    let route_log = base.join("route.txt");
    let mut route_monitor = route_monitor(&route_log);

    println!("measuring {window}s");
    sleep(Duration::from_secs(window));
    if let Some(monitor) = route_monitor.as_mut() {
        let _ = monitor.kill();
        let _ = monitor.wait();
    }

    let elapsed = f64::from(u32::try_from(window).unwrap_or(u32::MAX));
    println!("\n=== idle cost over {window}s ===");
    report_host(&route_log, route_monitor.is_some(), elapsed);
    for subject in &mut subjects {
        let cpu = cpu_seconds(subject.child.id()) - subject.cpu_start;
        let enums = enumerations(&subject.log_dir).saturating_sub(subject.enums_start);
        println!(
            "{:9}: {cpu:.2}s CPU = {:.2}% of one core | {enums} enumerations",
            subject.label,
            100.0 * cpu / elapsed,
        );
        let _ = subject.child.kill();
        let _ = subject.child.wait();
    }
    let _ = fs::remove_dir_all(&base);
    Ok(())
}

fn spawn(
    binary: &Path,
    base: &Path,
    label: &'static str,
    extra: &[&str],
) -> Result<Subject, Box<dyn std::error::Error>> {
    let log_dir = base.join(format!("logs-{label}"));
    let child = Command::new(binary)
        .arg("create")
        .args(extra)
        .args(["--nickname", &format!("idle-{label}")])
        .arg("--log-dir")
        .arg(&log_dir)
        .arg("--state-file")
        .arg(base.join(format!("{label}.state.json")))
        // netmon logs `no changes detected` once per completed enumeration, so
        // counting that line prices the work without patching netwatch. The
        // line costs a write per enumeration, which slightly inflates CPU —
        // acceptable, since it is the only way to attribute the cost at all.
        .env("RUST_LOG", "netwatch=debug")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(Subject {
        label,
        child,
        log_dir,
        cpu_start: 0.0,
        enums_start: 0,
    })
}

/// `route -n monitor` observes the host-wide message stream; it bills nothing
/// to the daemons. Absent off BSD, where the netmon uses netlink instead and
/// none of this applies.
fn route_monitor(log: &Path) -> Option<Child> {
    let file = fs::File::create(log).ok()?;
    Command::new("route")
        .args(["-n", "monitor"])
        .stdout(Stdio::from(file))
        .stderr(Stdio::null())
        .spawn()
        .ok()
}

fn report_host(route_log: &Path, monitored: bool, elapsed: f64) {
    let interfaces = Command::new("ifconfig").arg("-l").output().ok();
    let count = interfaces.as_ref().map_or(0, |out| {
        String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .count()
    });
    println!("host: {count} interfaces");

    if !monitored {
        println!("route: unavailable (no AF_ROUTE monitor on this platform)");
        return;
    }
    let text = fs::read_to_string(route_log).unwrap_or_default();
    let total = text.lines().filter(|line| line.starts_with("RTM_")).count();
    let misses = text
        .lines()
        .filter(|line| line.starts_with("RTM_MISS"))
        .count();
    // netwatch drops link-local destinations; everything else it accepts, and
    // an accepted message is what buys an enumeration 250ms later.
    let mut accepted = 0;
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        if line.contains("sockaddrs: <DST>")
            && !lines
                .next()
                .unwrap_or_default()
                .trim_start()
                .starts_with("fe80:")
        {
            accepted += 1;
        }
    }
    let rate = f64::from(u32::try_from(total).unwrap_or(u32::MAX)) / elapsed;
    println!(
        "route: {total} msgs ({rate:.2}/s) | RTM_MISS {misses}, other {} | accepted {accepted}",
        total - misses,
    );
}

fn cpu_seconds(pid: u32) -> f64 {
    let Ok(out) = Command::new("ps")
        .args(["-o", "time=", "-p", &pid.to_string()])
        .output()
    else {
        return 0.0;
    };
    // macOS prints `MM:SS.ss`, growing a leading `HH:` once it passes an hour.
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .rsplit(':')
        .zip([1.0, 60.0, 3600.0])
        .map(|(field, scale)| field.parse::<f64>().unwrap_or(0.0) * scale)
        .sum()
}

fn enumerations(log_dir: &Path) -> u64 {
    let Some(log) = find_log(log_dir) else {
        return 0;
    };
    fs::read_to_string(log).map_or(0, |text| {
        text.lines()
            .filter(|line| line.contains("no changes detected"))
            .count() as u64
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
