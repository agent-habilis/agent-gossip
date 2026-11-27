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
    let sections = report["sections"].as_array().expect("report has sections");
    let integrations: &serde_json::Value = sections
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

    let is_harness_row = |line: &&str| line.starts_with('|') && line.contains("/skills`");
    let rows: usize = DOC.lines().filter(is_harness_row).count();
    assert_eq!(
        rows,
        checks.len(),
        "the doc's harness table has {rows} rows but agent-gossip supports {} harnesses",
        checks.len()
    );
}

/// A skill that points at `references/<name>.md` is only as good as the file
/// that `plug` writes there: a name with no file sends the agent to a path
/// that is not there, and an `unplug` that leaves a reference behind leaves
/// stale rules on the machine.
#[test]
fn plug_writes_every_reference_a_skill_names_and_unplug_removes_them() {
    let home = std::env::temp_dir().join(format!("agent-gossip-refs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join(".codex")).expect("seeding a fake codex");
    let run = |verb: &str| {
        let out = common::test_cmd()
            .env("HOME", &home)
            .arg(verb)
            .output()
            .expect("failed to run agent-gossip");
        assert!(out.status.success(), "`{verb}` failed: {out:?}");
    };

    run("plug");
    let skills = home.join(".codex/skills");
    let mut named_total = 0;
    for entry in std::fs::read_dir(&skills).expect("plug created the skills dir") {
        let dir = entry.expect("readable dir entry").path();
        let body =
            std::fs::read_to_string(dir.join("SKILL.md")).expect("each skill has a SKILL.md");
        for mention in body.split("references/").skip(1) {
            let Some((name, _)) = mention.split_once(".md") else {
                continue;
            };
            if !name.chars().all(|ch| ch.is_ascii_lowercase() || ch == '-') {
                continue;
            }
            let file = dir.join("references").join(format!("{name}.md"));
            assert!(
                file.is_file(),
                "{dir:?} names references/{name}.md, plug wrote none"
            );
            named_total += 1;
        }
    }
    assert!(
        named_total > 0,
        "no skill names a reference, so this test checks nothing"
    );

    run("unplug");
    let mut left: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&skills) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("gossip-") {
                left.push(name);
            }
        }
    }
    assert!(left.is_empty(), "unplug left skills behind: {left:?}");

    std::fs::remove_dir_all(&home).expect("the test temp directory is writable");
}

/// With the user's own `hooks.json` in the agy plugin folder, `plug` installs
/// the skills and no Stop hook. Both `plug` and `doctor` must say so, and say
/// what to do, or the user is told to run `plug` again and nothing changes.
#[test]
fn plug_and_doctor_say_when_the_bell_hook_is_not_installed() {
    let home =
        std::env::temp_dir().join(format!("agent-gossip-foreign-hook-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let root = home.join(".gemini/config/plugins/agent-gossip");
    std::fs::create_dir_all(&root).expect("seeding the plugin folder");
    std::fs::create_dir_all(home.join(".gemini/antigravity-cli")).expect("seeding a fake agy");
    std::fs::write(root.join("hooks.json"), r#"{"mine":{}}"#).expect("seeding a user hooks file");

    let plug = common::test_cmd()
        .env("HOME", &home)
        .args(["plug", "--agent", "antigravity"])
        .output()
        .expect("failed to run agent-gossip plug");
    let warning = String::from_utf8_lossy(&plug.stderr).into_owned();
    assert!(
        warning.contains("NOT installed") && warning.contains("move your file away"),
        "plug must say that the bell Stop hook is not installed, and the way out: {warning}"
    );

    let doctor = common::test_cmd()
        .env("HOME", &home)
        .args(["doctor", "--no-probe", "--output", "json"])
        .output()
        .expect("failed to run agent-gossip doctor");
    let report: serde_json::Value =
        serde_json::from_slice(&doctor.stdout).expect("doctor --output json emits JSON");
    let sections = report["sections"].as_array().expect("report has sections");
    let integrations = sections
        .iter()
        .find(|section| section["title"] == "Integrations")
        .expect("machine report has an Integrations section");
    let checks = integrations["checks"]
        .as_array()
        .expect("Integrations has checks");
    let agy = checks
        .iter()
        .find(|check| check["name"] == "antigravity")
        .expect("the roster lists antigravity");
    let detail = agy["detail"]
        .as_str()
        .expect("an integration check has detail");
    assert!(
        detail.contains("NOT installed") && detail.contains("move your file away"),
        "doctor must say the same, and not only tell the user to run plug: {detail}"
    );

    std::fs::remove_dir_all(&home).expect("the test temp directory is writable");
}

/// A reference is part of the agy install: an edited or a missing one must show
/// as out of date in `doctor`, or the agent reads stale rules after an upgrade.
/// This goes through the agy plugin that this change adds, with the real binary.
#[test]
fn doctor_flags_an_edited_or_missing_agy_reference_as_out_of_date() {
    let home = std::env::temp_dir().join(format!("agent-gossip-agy-drift-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join(".gemini/antigravity-cli")).expect("seeding a fake agy");
    let plug = common::test_cmd()
        .env("HOME", &home)
        .args(["plug", "--agent", "antigravity"])
        .output()
        .expect("failed to run agent-gossip plug");
    assert!(
        plug.status.success(),
        "the drift check needs an agy plugin that was written: {plug:?}"
    );
    let reference = home.join(
        ".gemini/config/plugins/agent-gossip/skills/gossip-ping/references/event-handling.md",
    );
    assert!(
        reference.is_file(),
        "plug wrote the reference that the test edits"
    );

    let agy_detail = || {
        let doctor = common::test_cmd()
            .env("HOME", &home)
            .args(["doctor", "--no-probe", "--output", "json"])
            .output()
            .expect("failed to run agent-gossip doctor");
        let report: serde_json::Value =
            serde_json::from_slice(&doctor.stdout).expect("doctor --output json emits JSON");
        let sections = report["sections"].as_array().expect("report has sections");
        let integrations = sections
            .iter()
            .find(|section| section["title"] == "Integrations")
            .expect("machine report has an Integrations section");
        let checks = integrations["checks"]
            .as_array()
            .expect("Integrations has checks");
        let agy = checks
            .iter()
            .find(|check| check["name"] == "antigravity")
            .expect("the roster lists antigravity");
        agy["detail"]
            .as_str()
            .expect("an integration check has detail")
            .to_owned()
    };

    assert!(
        agy_detail().starts_with("up to date"),
        "a fresh plug is up to date: {}",
        agy_detail()
    );
    std::fs::write(&reference, "stale").expect("the test temp directory is writable");
    assert!(
        agy_detail().starts_with("out of date"),
        "an edited reference is out of date: {}",
        agy_detail()
    );
    std::fs::remove_file(&reference).expect("the test temp directory is writable");
    assert!(
        agy_detail().starts_with("out of date"),
        "a missing reference is out of date: {}",
        agy_detail()
    );

    std::fs::remove_dir_all(&home).expect("the test temp directory is writable");
}

/// In agy 1.5.12 only the namespaced form of a plugin skill expands, so every
/// place that tells a user what to type must give `/agent-gossip:gossip-*` for
/// agy.
#[test]
fn the_docs_name_the_agy_command_prefix() {
    let manual = include_str!("../../../docs/manual.txt");
    let skills_page = include_str!("../../../packages/web/content/docs/skills.mdx");
    for (name, text) in [
        ("README", README),
        ("installation doc", DOC),
        ("manual", manual),
        ("skills page", skills_page),
    ] {
        assert!(
            text.contains("/agent-gossip:gossip-"),
            "the {name} never gives the namespaced agy command"
        );
    }
    let row = DOC
        .lines()
        .find(|line| line.starts_with("| Antigravity"))
        .expect("the installation doc has an Antigravity row");
    assert!(
        row.contains("`/agent-gossip:gossip-create`"),
        "the agy row must show the namespaced command: {row}"
    );
}

/// agy's Standard Sandbox cannot reach the gossip daemon, so the doc the
/// installing agent reads must say that every `agent-gossip` call in agy runs
/// with the sandbox off.
#[test]
fn the_doc_tells_agy_to_bypass_the_sandbox() {
    assert!(
        DOC.contains("BypassSandbox") && DOC.contains("No active gossip server running"),
        "the installation doc never explains the agy sandbox"
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
    assert_eq!(
        path, "docs/agentic-installation.md",
        "the installation doc is the one the skills point at"
    );
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
    let roster = String::from_utf8(plug.stdout)
        .expect("the plug report is human text written by our own printer");

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
    for absent in ["pi", "cursor", "opencode", "antigravity"] {
        assert!(
            roster.contains(&format!("Skipped {absent} (not detected)")),
            "roster should skip the absent harness {absent}:\n{roster}"
        );
    }

    let skills = home.join(".claude/skills");
    let entries = std::fs::read_dir(&skills).expect("plug created the skills dir");
    let file_names = entries.map(|entry| entry.expect("readable dir entry").file_name());
    let names = file_names.map(|name| name.to_string_lossy().into_owned());
    let installed: Vec<String> = names.filter(|name| name.starts_with("gossip-")).collect();

    // The doc names four skills and claims "thirteen more"; that arithmetic is
    // only true while this count holds.
    assert_eq!(
        installed.len(),
        17,
        "expected 17 gossip skills, found {installed:?}"
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
    let sections = report["sections"].as_array().expect("report has sections");
    let integrations_section = sections
        .iter()
        .find(|section| section["title"] == "Integrations");
    let checks: &Vec<serde_json::Value> = integrations_section
        .and_then(|section| section["checks"].as_array())
        .expect("the plug report lists every integration under one section");
    let state: Box<dyn Fn(&str) -> String + '_> = Box::new(|harness| -> String {
        let row = checks.iter().find(|check| check["name"] == harness);
        let detail = row
            .and_then(|check| check["detail"].as_str())
            .unwrap_or_else(|| panic!("no {harness} row in the Integrations section"));
        detail.to_owned()
    });
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
