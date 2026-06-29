//! `cargo task build [--target TRIPLE | --arch ARCH] [--release]` — build the
//! `ahs` binary, cross-compiling for a foreign target through a **project-
//! pinned** zig toolchain. zig is provisioned into `target/tooling/` on first
//! use (never the dev's global/brew zig), so the cross build is self-contained
//! and reproducible. `--arch` is sugar for a static-musl Linux target — the
//! shape we deploy to the Raspberry Pi fleet.

use std::path::PathBuf;

use xshell::{Shell, cmd};

use crate::TaskOutcome;
use crate::util::{ensure_installed, output, repo_root};

/// Pinned zig version — the one `cargo-zigbuild` is validated against here.
/// Bump deliberately (and re-test a cross build) for a new toolchain.
const ZIG_VERSION: &str = "0.16.0";

pub(crate) fn run(
    sh: &Shell,
    target: Option<&str>,
    arch: Option<&str>,
    release: bool,
) -> TaskOutcome {
    let triple = match (target, arch) {
        (Some(_), Some(_)) => return Err("pass only one of --target / --arch".into()),
        (Some(triple), None) => Some(triple.to_owned()),
        // `--arch aarch64` ⇒ `aarch64-unknown-linux-musl` (static, no glibc).
        (None, Some(arch)) => Some(format!("{arch}-unknown-linux-musl")),
        (None, None) => None,
    };
    let profile: &[&str] = if release { &["--release"] } else { &[] };

    let Some(triple) = triple else {
        // Plain host build — no cross toolchain needed.
        cmd!(sh, "cargo build {profile...} --bin ahs").run()?;
        return Ok(());
    };

    // Cross build through the project-pinned zig + cargo-zigbuild.
    let zig_dir = ensure_zig(sh)?;
    ensure_installed(sh, "cargo-zigbuild", &["zigbuild", "--version"]);
    let _ = cmd!(sh, "rustup target add {triple}").quiet().run();

    output::status(
        "Cross",
        &format!("ahs → {triple} (pinned zig {ZIG_VERSION})"),
    );
    // Prepend the pinned zig to PATH so cargo-zigbuild resolves *it*, not a
    // globally-installed zig of some other version.
    let path = format!(
        "{}:{}",
        zig_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    cmd!(
        sh,
        "cargo zigbuild {profile...} --target {triple} --bin ahs"
    )
    .env("PATH", path)
    .run()?;
    let dir = if release { "release" } else { "debug" };
    output::status("Built", &format!("target/{triple}/{dir}/ahs"));
    Ok(())
}

/// Download + cache the pinned zig under `target/tooling/` and return the dir
/// holding the `zig` executable. Downloads once; reused on later builds.
fn ensure_zig(sh: &Shell) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let host = host_zig_infix()?; // e.g. "aarch64-macos"
    let stem = format!("zig-{host}-{ZIG_VERSION}");
    let tooling = repo_root().join("target").join("tooling");
    let dir = tooling.join(&stem);
    let zig = dir.join("zig");
    if zig.exists() {
        return Ok(dir);
    }
    std::fs::create_dir_all(&tooling)?;
    let url = format!("https://ziglang.org/download/{ZIG_VERSION}/{stem}.tar.xz");
    output::status(
        "Fetching",
        &format!("zig {ZIG_VERSION} ({host}) → target/tooling/"),
    );
    let tarball = tooling.join(format!("{stem}.tar.xz"));
    cmd!(sh, "curl -fsSL {url} -o {tarball}").run()?;
    cmd!(sh, "tar -xJf {tarball} -C {tooling}").run()?;
    let _ = std::fs::remove_file(&tarball);
    if !zig.exists() {
        return Err(format!("pinned zig missing at {} after extract", zig.display()).into());
    }
    Ok(dir)
}

/// The host's zig tarball infix (`<arch>-<os>`, zig's ≥0.14 naming).
fn host_zig_infix() -> Result<String, Box<dyn std::error::Error>> {
    let arch = match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        "x86_64" => "x86_64",
        other => return Err(format!("unsupported host arch for zig: {other}").into()),
    };
    let os = match std::env::consts::OS {
        "macos" => "macos",
        "linux" => "linux",
        other => return Err(format!("unsupported host OS for zig: {other}").into()),
    };
    Ok(format!("{arch}-{os}"))
}
