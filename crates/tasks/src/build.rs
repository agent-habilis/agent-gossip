//! `cargo task build [--target TRIPLE | --arch ARCH] [--release]` — build the
//! `agent-gossip` binary, cross-compiling for a foreign target through a **project-
//! pinned** zig + cargo-zigbuild toolchain. zig is vendored into
//! `target/tooling/` on first use (never the dev's global/brew zig);
//! cargo-zigbuild is a regular crate dependency driven as a *library*, not a
//! global `cargo install`. So the cross build is self-contained and
//! reproducible. `--arch` is sugar for a glibc Linux target — the shape we
//! deploy to the Raspberry Pi fleet.

use std::env::consts;
use std::ffi::OsStr;
use std::path::PathBuf;

use clap::Parser;

use crate::TaskOutcome;
use crate::util::{Invocation, output, repo_root};

/// Pinned zig version — the one `cargo-zigbuild` is validated against here.
/// Bump deliberately (and re-test a cross build) for a new toolchain.
const ZIG_VERSION: &str = "0.16.0";

/// glibc floor for `--arch`, as the `.<version>` suffix cargo-zigbuild reads.
/// Debian 11 Bullseye, so a fleet member on an older image still runs the
/// binary — zig carries every stub set, so reaching back costs nothing.
const GLIBC_FLOOR: &str = "2.31";

pub(crate) fn run(target: Option<&str>, arch: Option<&str>, release: bool) -> TaskOutcome {
    let triple = match (target, arch) {
        (Some(_), Some(_)) => return Err("pass only one of --target / --arch".into()),
        (Some(triple), None) => Some(triple.to_owned()),
        // `--arch aarch64` ⇒ `aarch64-unknown-linux-gnu.2.31`.
        (None, Some(arch)) => Some(format!("{arch}-unknown-linux-gnu.{GLIBC_FLOOR}")),
        (None, None) => None,
    };

    let Some(triple) = triple else {
        // Plain host build — no cross toolchain needed.
        let profile: &[&str] = if release { &["--release"] } else { &[] };
        Invocation::cargo(["build"])
            .args(profile)
            .args(["--bin", "agent-gossip"])
            .run()?;
        return Ok(());
    };

    // Cross build through the vendored pinned zig + cargo-zigbuild (a library
    // dependency, not a global install). cargo-zigbuild locates zig via the
    // `CARGO_ZIGBUILD_ZIG_PATH` env var; setting env in-process is `unsafe`
    // (edition 2024) and forbidden workspace-wide, so re-exec ourselves once
    // with the var injected into the *child* environment (`Command::env`
    // is safe). The child re-enters here with the var set and runs the real build.
    if std::env::var_os("CARGO_ZIGBUILD_ZIG_PATH").is_none() {
        let zig = ensure_zig()?.join("zig");
        let exe = std::env::current_exe()?;
        let rel: &[&str] = if release { &["--release"] } else { &[] };
        let rebuild = Invocation::new(exe).args(["build", "--target", triple.as_str()]);
        rebuild
            .args(rel)
            .env("CARGO_ZIGBUILD_ZIG_PATH", &zig)
            .run()?;
        return Ok(());
    }

    // cargo-zigbuild reads the glibc floor off a `.<version>` suffix and splits
    // it away before rustc sees the triple. rustup and the built path know only
    // the bare one, so telling them about the suffix leaves the target
    // uninstalled and prints a path that does not exist.
    let rust_triple = triple
        .split_once('.')
        .map_or(triple.as_str(), |(bare, _)| bare);

    let _ = Invocation::new("rustup")
        .args(["target", "add", rust_triple])
        .quiet()
        .run();

    output::status(
        "Cross",
        &format!("agent-gossip → {triple} (pinned zig {ZIG_VERSION})"),
    );

    // Drive cargo-zigbuild in-process. Its cross-link wrapper re-execs *this*
    // binary as `<exe> zig cc …` (it resolves itself via `current_exe()`); the
    // hidden `Zig` subcommand in main.rs handles that. `parse_from` feeds the
    // same flags the `cargo zigbuild` CLI would take.
    let flags = [
        "cargo-zigbuild",
        "--target",
        triple.as_str(),
        // `--package` as well as `--bin`: the root manifest is a virtual
        // workspace, so bin selection would otherwise lean on `default-members`.
        "--package",
        "agent-gossip",
        "--bin",
        "agent-gossip",
    ];
    let mut args: Vec<String> = flags.into_iter().map(str::to_owned).collect();
    if release {
        args.push("--release".to_owned());
    }
    let mut build = cargo_zigbuild::Build::parse_from(args);
    build.enable_zig_ar = true; // mirrors cargo-zigbuild's own bin, which sets it for every target
    build
        .execute()
        .map_err(|err| -> Box<dyn std::error::Error> { err.into() })?;

    let dir = if release { "release" } else { "debug" };
    output::status("Built", &format!("target/{rust_triple}/{dir}/agent-gossip"));
    Ok(())
}

/// Download + cache the pinned zig under `target/tooling/` and return the dir
/// holding the `zig` executable. Downloads once; reused on later builds.
fn ensure_zig() -> Result<PathBuf, Box<dyn std::error::Error>> {
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
    Invocation::new("curl")
        .args([
            OsStr::new("-fsSL"),
            url.as_ref(),
            OsStr::new("-o"),
            tarball.as_os_str(),
        ])
        .run()?;
    Invocation::new("tar")
        .args([
            OsStr::new("-xJf"),
            tarball.as_os_str(),
            OsStr::new("-C"),
            tooling.as_os_str(),
        ])
        .run()?;
    let _ = std::fs::remove_file(&tarball);
    if !zig.exists() {
        return Err(format!("pinned zig missing at {} after extract", zig.display()).into());
    }
    Ok(dir)
}

/// The host's zig tarball infix (`<arch>-<os>`, zig's ≥0.14 naming).
fn host_zig_infix() -> Result<String, Box<dyn std::error::Error>> {
    let arch = match consts::ARCH {
        "aarch64" => "aarch64",
        "x86_64" => "x86_64",
        other => return Err(format!("unsupported host arch for zig: {other}").into()),
    };
    let os = match consts::OS {
        "macos" => "macos",
        "linux" => "linux",
        other => return Err(format!("unsupported host OS for zig: {other}").into()),
    };
    Ok(format!("{arch}-{os}"))
}
