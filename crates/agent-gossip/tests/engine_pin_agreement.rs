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

#[test]
fn every_habilis_network_pin_in_the_wasm_client_matches_the_app() {
    let app_rev = app_engine_rev();
    let engine_lines: Vec<&str> = WASM_MANIFEST
        .lines()
        .filter(|line| line.contains("habilis-network"))
        .collect();
    let wasm_pins: Vec<&str> = engine_lines.into_iter().filter_map(rev_of_line).collect();
    assert!(!wasm_pins.is_empty(), "the wasm client pins the engine");
    for rev in wasm_pins {
        assert_eq!(
            rev, app_rev,
            "a wasm client pin differs from the app's engine rev"
        );
    }
}
