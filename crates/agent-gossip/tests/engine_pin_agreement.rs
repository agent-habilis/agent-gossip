//! The wasm client builds against the same engine rev as the app.

const APP_MANIFEST: &str = include_str!("../../../Cargo.toml");
const WASM_MANIFEST: &str = include_str!("../../agent-gossip-wasm-client/Cargo.toml");

fn rev_of_line(line: &str) -> Option<&str> {
    let after: &str = line.split("rev = \"").nth(1)?;
    after.split('"').next()
}

fn app_engine_rev() -> &'static str {
    let line: &str = APP_MANIFEST
        .lines()
        .find(|line| line.starts_with("habilis-network = "))
        .expect("the app pins habilis-network by rev");
    rev_of_line(line).expect("the app's habilis-network line carries a rev")
}

/// Every habilis-network line in `manifest` must carry a rev equal to `app_rev`.
fn wasm_pin_problems(manifest: &str, app_rev: &str) -> Vec<String> {
    let mut problems = Vec::new();
    for line in manifest
        .lines()
        .filter(|line| line.contains("habilis-network"))
    {
        match rev_of_line(line) {
            Some(rev) if rev == app_rev => {}
            Some(rev) => problems.push(format!("rev {rev} differs from the app's {app_rev}")),
            None => problems.push(format!("no rev on the line: {line}")),
        }
    }
    problems
}

#[test]
fn every_habilis_network_pin_in_the_wasm_client_matches_the_app() {
    let app_rev = app_engine_rev();
    let problems = wasm_pin_problems(WASM_MANIFEST, app_rev);
    assert!(
        problems.is_empty(),
        "the wasm client pins disagree with the app: {problems:?}"
    );
}

#[test]
fn a_wasm_engine_line_without_a_rev_is_a_problem() {
    let manifest =
        "habilis-network = { git = \"https://github.com/agent-habilis/habilis-network\" }\n";
    let problems = wasm_pin_problems(manifest, "a280a890");
    assert!(
        !problems.is_empty(),
        "a habilis-network line with no rev must be reported, not skipped"
    );
}
