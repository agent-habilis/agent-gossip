//! Contract test for `docs/agentic-installation.md` — the prompt agents
//! fetch from the raw `GitHub` URL and execute against a user's machine. Nothing
//! compiles it and no one runs it locally, so a renamed subcommand or a sixth
//! harness would leave it quietly wrong for every new user. These tests pin it
//! to the CLI it drives.

use agent_gossip_test_fixtures as common;

const DOC: &str = include_str!("../../../docs/agentic-installation.md");
const README: &str = include_str!("../../../README.md");
const URL: &str = "https://raw.githubusercontent.com/agent-habilis/agent-gossip/main/docs/agentic-installation.md";

/// Everything the doc presents as something to type: fenced-block lines plus
/// inline code spans.
fn typed_lines(doc: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut fenced = false;
    for line in doc.lines() {
        if line.starts_with("```") {
            fenced = !fenced;
        } else if fenced {
            lines.push(line.trim());
        } else {
            // Odd segments of a backtick split are the code spans.
            lines.extend(line.split('`').skip(1).step_by(2));
        }
    }
    lines
}

/// Parse a line through the real CLI. Checking the subcommand name alone would
/// let a renamed flag rot the doc unnoticed — `plug --path` is the one at risk.
fn cli_accepts(argv: &[&str]) -> Result<(), clap::Error> {
    match agent_gossip::cli_command().try_get_matches_from(argv) {
        Ok(_) => Ok(()),
        // `--version` and `--help` "fail" by design: clap hands back the text
        // to print as an error. The CLI accepts the line either way.
        Err(error)
            if matches!(
                error.kind(),
                clap::error::ErrorKind::DisplayVersion | clap::error::ErrorKind::DisplayHelp
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(error),
    }
}

#[test]
fn every_command_the_doc_runs_parses() {
    let mut checked = 0;

    for line in typed_lines(DOC) {
        if !line.starts_with("agent-gossip ") {
            continue;
        }
        let argv: Vec<&str> = line.split_whitespace().collect();
        if let Err(error) = cli_accepts(&argv) {
            panic!("installation doc runs `{line}`, which the CLI rejects:\n{error}");
        }
        checked += 1;
    }

    // A scanner that matches nothing would pass vacuously forever.
    assert!(
        checked >= 4,
        "expected the doc to run several agent-gossip commands, found {checked}"
    );
}

/// The step-3 table tells the user how to load the skills into their harness,
/// one row per harness `plug` installs into. `doctor`'s Integrations section is
/// the same list, rendered — so a sixth harness fails here until the doc grows
/// its row.
#[test]
fn reload_table_covers_every_harness() {
    let output = common::test_cmd()
        .args(["doctor", "--no-probe", "--output", "json"])
        .output()
        .expect("failed to run `agent-gossip doctor`");
    assert!(
        output.status.success(),
        "`agent-gossip doctor --no-probe` should exit 0, got {:?}",
        output.status
    );

    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("doctor --output json emits JSON");
    let integrations = report["sections"]
        .as_array()
        .expect("report has sections")
        .iter()
        .find(|section| section["title"] == "Integrations")
        .expect("machine report has an Integrations section");
    let checks = integrations["checks"]
        .as_array()
        .expect("Integrations has checks");

    for check in checks {
        let detail = check["detail"]
            .as_str()
            .expect("integration check has detail");
        // `<state> (<path>)`, sometimes trailed by a remediation hint.
        let (path, _) = detail
            .split_once('(')
            .and_then(|(_, rest)| rest.split_once(')'))
            .unwrap_or_else(|| panic!("no skills path in integration detail {detail:?}"));
        assert!(
            DOC.contains(path),
            "installation doc never mentions the {} skills path {path}",
            check["name"]
        );
    }

    let rows = DOC
        .lines()
        .filter(|line| line.starts_with('|') && line.contains("/skills`"))
        .count();
    assert_eq!(
        rows,
        checks.len(),
        "the doc's harness table has {rows} rows but agent-gossip supports {} harnesses",
        checks.len()
    );
}

/// The closing banner is the doc's only output template, and it is presented
/// inside a fence it must not reproduce: an agent that copies the block
/// verbatim prints literal backticks instead of inline code. Three ways that
/// breaks, all asserted against the banner section alone so a doc-wide match
/// elsewhere cannot stand in for the real thing: the disclaimer disappears, box
/// art returns whose fixed width a real version string would overrun, or a
/// placeholder re-grows the `/` prefix that only Claude Code and Cursor use.
#[test]
fn the_banner_template_forbids_fencing() {
    let banner = DOC
        .rsplit_once("\n---\n")
        .expect("the doc separates its closing banner with a rule")
        .1;

    assert!(
        banner.contains("only delimits the template inside this document"),
        "the banner section must disclaim its own fence"
    );
    assert!(
        !banner.contains(['│', '─', '┌', '┐', '└', '┘']),
        "the closing banner should be plain markdown, not fixed-width box art"
    );
    assert!(
        !banner.contains("`/<"),
        "the banner's placeholders must not hardcode the `/` command prefix"
    );
}

/// The doc is only reachable through the URL baked into the README and into its
/// own footer; nothing else ties that string to this file's path.
#[test]
fn the_published_url_resolves_to_this_doc() {
    assert!(README.contains(URL), "README does not link the install doc");
    assert!(DOC.contains(URL), "the doc does not cite its own URL");

    let (_, path) = URL
        .rsplit_once("/main/")
        .expect("the URL pins the main branch");
    assert_eq!(path, "docs/agentic-installation.md");
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    assert!(
        root.join(path).is_file(),
        "the published URL points at {path}, which does not exist"
    );
}

/// Step 2 of the doc, run for real against a machine that has nothing installed.
/// `agent::home_dir` reads `$HOME` and nothing else, so a temp dir is a complete
/// and risk-free stand-in for a fresh machine — the operator's own skills are
/// never touched. The only other coverage of `plug` is `plug.rs`'s inline test,
/// which exercises `--path` and never the `$HOME` path the doc actually uses.
#[test]
fn plug_populates_a_clean_machine_and_unplug_reverses_it() {
    // Same tempdir idiom as `plug.rs`'s test — no `tempfile` dev-dependency.
    let home = std::env::temp_dir().join(format!("agent-gossip-clean-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    // Two harnesses present, three absent: the roster must distinguish them.
    std::fs::create_dir_all(home.join(".claude")).expect("seeding a fake claude-code");
    std::fs::create_dir_all(home.join(".codex")).expect("seeding a fake codex");

    let plug = common::test_cmd()
        .env("HOME", &home)
        .arg("plug")
        .output()
        .expect("failed to run `agent-gossip plug`");
    assert!(plug.status.success(), "`plug` failed: {:?}", plug.status);
    let roster = String::from_utf8(plug.stdout).expect("roster is UTF-8");

    // The doc teaches the agent to read these exact roster lines and branch on
    // them, so pin the wording, not just the harness names.
    for (harness, path) in [
        ("claude-code", "~/.claude/skills"),
        ("codex", "~/.codex/skills"),
    ] {
        assert!(
            roster.contains(&format!("Installed {harness} ({path})")),
            "roster should report {harness} installed:\n{roster}"
        );
    }
    for absent in ["pi", "cursor", "opencode"] {
        assert!(
            roster.contains(&format!("Skipped {absent} (not detected)")),
            "roster should skip the absent harness {absent}:\n{roster}"
        );
    }

    let skills = home.join(".claude/skills");
    let installed: Vec<String> = std::fs::read_dir(&skills)
        .expect("plug created the skills dir")
        .map(|entry| entry.expect("readable dir entry").file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| name.starts_with("gossip-"))
        .collect();

    // The doc names four skills and claims "twelve more"; that arithmetic is
    // only true while this count holds.
    assert_eq!(
        installed.len(),
        16,
        "expected 16 gossip skills, found {installed:?}"
    );
    for named in ["gossip-create", "gossip-join", "gossip-msg", "gossip-task"] {
        assert!(
            installed.iter().any(|dir| dir == named),
            "the doc names {named}, which plug did not install"
        );
        assert!(
            skills.join(named).join("SKILL.md").is_file(),
            "{named} has no SKILL.md — the harness would ignore it"
        );
    }
    assert!(
        !home.join(".cursor").exists(),
        "plug scaffolded a config dir for an absent harness"
    );

    // Step 4: the doc tells the agent to read this and expect `up to date`.
    let doctor = common::test_cmd()
        .env("HOME", &home)
        .args(["doctor", "--no-probe", "--output", "json"])
        .output()
        .expect("failed to run `agent-gossip doctor`");
    let report: serde_json::Value =
        serde_json::from_slice(&doctor.stdout).expect("doctor emits JSON");
    let checks = report["sections"]
        .as_array()
        .expect("report has sections")
        .iter()
        .find(|section| section["title"] == "Integrations")
        .and_then(|section| section["checks"].as_array())
        .expect("Integrations section");
    let state = |harness: &str| -> String {
        checks
            .iter()
            .find(|check| check["name"] == harness)
            .and_then(|check| check["detail"].as_str())
            .unwrap_or_else(|| panic!("no {harness} row in the Integrations section"))
            .to_owned()
    };
    assert!(
        state("claude-code").starts_with("up to date"),
        "claude-code should be up to date, got {:?}",
        state("claude-code")
    );
    assert!(
        state("cursor").starts_with("absent"),
        "cursor should be absent, got {:?}",
        state("cursor")
    );

    let unplug = common::test_cmd()
        .env("HOME", &home)
        .arg("unplug")
        .output()
        .expect("failed to run `agent-gossip unplug`");
    assert!(
        unplug.status.success(),
        "`unplug` failed: {:?}",
        unplug.status
    );
    assert!(
        std::fs::read_dir(&skills)
            .expect("unplug left the skills dir in place")
            .next()
            .is_none(),
        "unplug left skills behind in {}",
        skills.display()
    );

    let _ = std::fs::remove_dir_all(&home);
}
