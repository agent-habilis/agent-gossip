//! Render the multi-file `skills/` sources into the single-file SKILL.md tree
//! the binary embeds (`src/cli/agent.rs` `include_dir!($OUT_DIR/skills)`), and
//! emit a fingerprint of the *generated* output so editing a source or the
//! renderer itself forces a rebuild (`include_dir!` is otherwise untracked on
//! stable, and fingerprinting the sources would miss renderer-only changes).
//!
//! One `SKILL.md` per skill is the runtime contract: an agent invoking a skill
//! gets the whole procedure in the file it was already handed — zero extra
//! read round-trips. The sources stay multi-file for reuse; each SKILL.md
//! template declares its own sources with `<!-- include path="..." -->`
//! directives that `slot-template` expands at build time, so adding a skill
//! needs no change here.
//!
//! The git version stamp (`VERGEN_GIT_*`, feeding `util::version::VERSION`)
//! lives in the engine crate's build script (`agent-habilis-mesh/build.rs`),
//! since `util::version` is an engine module.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

/// Names never staged/embedded — shared verbatim with `src/cli/agent.rs`'s
/// write-out filter via one `include!`d fragment, so staging and write-out can
/// never disagree (the binary never carries a file it would discard on install).
///
/// `include!` forces a compile-time `env!`, which bakes the path into the
/// cached build-script binary: after the checkout moves, this reads the *old*
/// location until something recompiles the script (`cargo clean -p
/// agent-gossip`). Tolerable here because the fragment rarely changes and a
/// deleted old path fails the compile loudly — but never copy this pattern
/// for paths read at script runtime; see [`skills_dir`].
const SKIP: &[&str] = include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/cli/embed_skip.rs"
));

fn main() {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    let generated = out_dir.join("skills");
    render_skills(&skills_dir(), &generated);
    emit_embed_fingerprint(&generated);
}

/// `skills/` sits at the workspace root, two levels above this package. Anchored
/// to `CARGO_MANIFEST_DIR` rather than the CWD, which cargo only *happens* to set
/// to the package dir — an absolute source path also keeps the `<!-- include -->`
/// directives inside each template resolving against a real base.
///
/// Read at script *runtime*, never via `env!`: the compile-time macro bakes
/// the path into the cached build-script binary, and after the checkout moves
/// the script keeps rendering the old location's skills — fresh mtimes, stale
/// content, no error as long as the old path exists.
fn skills_dir() -> PathBuf {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    Path::new(&manifest_dir).join("../../skills")
}

/// Expand every `skills/gossip-*/SKILL.md` template into `dest`, one
/// self-contained `SKILL.md` per skill. `shared/` is never emitted.
fn render_skills(src: &Path, dest: &Path) {
    if dest.exists() {
        std::fs::remove_dir_all(dest)
            .unwrap_or_else(|error| panic!("clear {}: {error}", dest.display()));
    }

    let mut skills: Vec<PathBuf> = std::fs::read_dir(src)
        .unwrap_or_else(|error| panic!("read {}: {error}", src.display()))
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_dir()
                && path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("gossip-"))
        })
        .collect();
    skills.sort();

    let mut loader = |path: &Path| std::fs::read_to_string(path).map_err(|error| error.to_string());
    for skill_dir in skills {
        let skill = skill_dir
            .file_name()
            .expect("skill dir has a name")
            .to_string_lossy()
            .into_owned();
        let rendered = slot_template::expand(&skill_dir.join("SKILL.md"), &[], &mut loader)
            .unwrap_or_else(|error| panic!("skills/{skill}/SKILL.md: {error}"));
        let out = dest.join(&skill);
        std::fs::create_dir_all(&out)
            .unwrap_or_else(|error| panic!("mkdir {}: {error}", out.display()));
        std::fs::write(out.join("SKILL.md"), rendered)
            .unwrap_or_else(|error| panic!("write {}/SKILL.md: {error}", out.display()));
    }
}

/// Hash the generated tree and publish it as `AGENT_GOSSIP_EMBED_FINGERPRINT`.
/// `agent.rs` reads it via `env!`, so a changed fingerprint recompiles that
/// module and re-expands the `include_dir!` embed. Hashing the *output* (not
/// the `skills/` sources) also catches renderer changes in this script and
/// the `slot-template` crate; `rerun-if-changed` makes the script re-run when
/// a source changes.
fn emit_embed_fingerprint(generated: &Path) {
    println!("cargo:rerun-if-changed=../../skills");
    let mut hasher = DefaultHasher::new();
    hash_dir(generated, generated, &mut hasher);
    println!(
        "cargo:rustc-env=AGENT_GOSSIP_EMBED_FINGERPRINT={:016x}",
        hasher.finish()
    );
}

/// Feed `dir`'s skip-filtered tree (root-relative paths + file bytes) into
/// `hasher`, in a deterministic order so the fingerprint is stable across
/// builds and machines (`OUT_DIR` itself never taints the hash).
fn hash_dir(root: &Path, dir: &Path, hasher: &mut DefaultHasher) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = read.flatten().collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        if SKIP.iter().any(|skip| entry.file_name() == **skip) {
            continue;
        }
        let path = entry.path();
        path.strip_prefix(root)
            .expect("entry lives under the hashed root")
            .to_string_lossy()
            .hash(hasher);
        if path.is_dir() {
            hash_dir(root, &path, hasher);
        } else if let Ok(bytes) = std::fs::read(&path) {
            bytes.hash(hasher);
        }
    }
}
