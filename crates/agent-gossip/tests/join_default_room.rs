//! Integration tests for the join target that `AGENT_GOSSIP_DEFAULT_ROOM` supplies.

use std::io::{BufRead, BufReader};
use std::process::{Child, Output, Stdio};
use std::sync::mpsc;

use agent_gossip_test_fixtures::{CONNECT_TIMEOUT, Node, test_cmd};
use serde_json::Value;

const DEFAULT_ROOM_ENV: &str = "AGENT_GOSSIP_DEFAULT_ROOM";

struct Killed(Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn join_without_target(env: Option<&str>) -> Output {
    let mut command = test_cmd();
    command.arg("join");
    match env {
        Some(value) => {
            command.env(DEFAULT_ROOM_ENV, value);
        }
        None => {
            command.env_remove(DEFAULT_ROOM_ENV);
        }
    }
    command.output().expect("join spawns")
}

fn spawn_join(args: &[&str], env: &str) -> Killed {
    let mut command = test_cmd();
    command.arg("join").args(args).env(DEFAULT_ROOM_ENV, env);
    command.stdout(Stdio::piped()).stderr(Stdio::null());
    let child = command.spawn().expect("join spawns");
    Killed(child)
}

/// The `gossip` of the daemon's `ready` event, or `None` when none arrives in time.
fn ready_gossip(joiner: &mut Killed) -> Option<String> {
    let stdout = joiner.0.stdout.take().expect("stdout is piped");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if value["event"] == "ready" {
                let _ = tx.send(value["gossip"].as_str().map(str::to_owned));
            }
        }
    });
    rx.recv_timeout(CONNECT_TIMEOUT).ok().flatten()
}

// The variable goes on each child only. Setting it in this process would leak
// it into the other tests, which run in parallel.
#[test]
fn no_target_and_no_environment_names_the_variable() {
    let out = join_without_target(None);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "join without a target must fail");
    assert!(
        stderr.contains(DEFAULT_ROOM_ENV),
        "stderr must name {DEFAULT_ROOM_ENV}, got: {stderr}"
    );
}

#[test]
fn empty_environment_counts_as_no_target() {
    let out = join_without_target(Some(""));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "join with an empty env must fail");
    assert!(
        stderr.contains(DEFAULT_ROOM_ENV),
        "stderr must name {DEFAULT_ROOM_ENV}, got: {stderr}"
    );
}

#[test]
fn malformed_environment_value_points_at_topic() {
    let out = join_without_target(Some("github.com/alice/proj"));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "a malformed env value must fail");
    assert!(
        stderr.contains("agent-gossip topic"),
        "stderr must point at `agent-gossip topic`, got: {stderr}"
    );
}

#[test]
fn environment_hash_joins_the_mesh() {
    let (_creator, mesh) = Node::create_named("itest-default-room-env");
    let mut joiner = spawn_join(&[], &mesh);
    assert_eq!(
        ready_gossip(&mut joiner).as_deref(),
        Some(mesh.as_str()),
        "join with no target must join the mesh named by the environment"
    );
}

#[test]
fn argument_wins_over_environment() {
    let (_arg_creator, arg_mesh) = Node::create_named("itest-default-room-arg");
    let (_env_creator, env_mesh) = Node::create_named("itest-default-room-other");
    assert_ne!(arg_mesh, env_mesh, "the two meshes must differ");
    let mut joiner = spawn_join(&[arg_mesh.as_str()], &env_mesh);
    assert_eq!(
        ready_gossip(&mut joiner).as_deref(),
        Some(arg_mesh.as_str()),
        "the argument's mesh must be joined, not the environment's"
    );
}
