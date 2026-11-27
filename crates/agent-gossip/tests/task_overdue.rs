//! Integration tests for the check-in alarm and the worker's reminder.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use agent_gossip_test_fixtures as common;
use common::Node;

const FAST_CHECKIN: [(&str, &str); 2] =
    [("--task-keepalive-secs", "1"), ("--task-checkin-secs", "2")];

/// A worker that accepts a task and then falls silent is reported to the
/// initiator at 2·X. The chain is the real one: the worker's beat carries its
/// check-in age, the initiator measures the silence, and `poll` serves
/// `task_overdue` as a visible event.
#[test]
fn a_worker_that_never_checks_in_again_is_reported_overdue() {
    let _serial = common::serial_guard();
    let (alice, mesh) = Node::create_flags("itest-overdue", &FAST_CHECKIN);
    let bob = Node::join_flags(&mesh, "overdue-bob", &FAST_CHECKIN);
    assert!(
        alice.wait_ready(&mesh),
        "an overdue report needs a ready initiator to receive it"
    );
    assert!(
        bob.wait_ready(&mesh),
        "an overdue report needs a ready worker to fall silent"
    );

    let task_id = common::cli_task_create(&mesh, &alice.nickname, &bob.nickname, "build it");
    common::CliWorker::new(&mesh, &bob.nickname).checkin(&task_id, "accepted: build it", "0");

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut seen = String::new();
    while Instant::now() < deadline {
        seen.push_str(&common::cli_poll(&mesh, &alice.nickname, None));
        if seen.contains("\"event\":\"task_overdue\"") {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    assert!(
        seen.contains("\"event\":\"task_overdue\""),
        "alice never saw task_overdue for {}\nalice poll output:\n{seen}\nbob log:\n{}",
        bob.nickname,
        bob.log_tail(20),
    );
    assert!(
        seen.contains(&format!("task overdue · `<{}>`", bob.nickname)),
        "the event must name the worker, got:\n{seen}"
    );
}

/// The other half: a worker that checks in on time is never reported, and its
/// check-ins do not wake the initiator's bell. The alarm above is then a fact
/// about silence, and not a side effect of the beats.
#[test]
fn a_worker_that_checks_in_is_not_reported_and_does_not_wake_the_sender() {
    let _serial = common::serial_guard();
    let (alice, mesh) = Node::create_flags("itest-checkin", &FAST_CHECKIN);
    let bob = Node::join_flags(&mesh, "checkin-bob", &FAST_CHECKIN);
    assert!(
        alice.wait_ready(&mesh),
        "the no-alarm check is void unless the initiator is ready to receive reports"
    );
    assert!(
        bob.wait_ready(&mesh),
        "the no-alarm check is void unless the worker is ready to check in"
    );

    let task_id = common::cli_task_create(&mesh, &alice.nickname, &bob.nickname, "build it");
    common::CliWorker::new(&mesh, &bob.nickname).checkin(&task_id, "accepted: build it", "0");
    // Everything so far is served, so only a new waking event can ring the bell.
    let mut seen = common::cli_poll(&mesh, &alice.nickname, None);
    let (rang_tx, rang) = mpsc::channel();
    let (poll_mesh, poll_nickname) = (mesh.clone(), alice.nickname.clone());
    std::thread::spawn(move || {
        let (events, _took) = common::cli_poll_long(&poll_mesh, &poll_nickname, None);
        let _ = rang_tx.send(events);
    });

    // Four intervals of 2 s: the silent worker above is reported within two.
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut step = 1_u32;
    while Instant::now() < deadline {
        // Staying inside the first quarter: only a move to another quarter wakes.
        let percent = step.min(24).to_string();
        common::CliWorker::new(&mesh, &bob.nickname).checkin(
            &task_id,
            &format!("step {step}"),
            &percent,
        );
        step += 1;
        std::thread::sleep(Duration::from_millis(700));
    }
    assert!(
        rang.try_recv().is_err(),
        "a routine check-in must not wake the sender's bell"
    );
    seen.push_str(&common::cli_poll(&mesh, &alice.nickname, None));
    assert!(
        seen.contains("step 1"),
        "the check-in goes on the ring, without a wake, got:\n{seen}"
    );
    assert!(
        !seen.contains("\"event\":\"task_overdue\""),
        "a worker that checks in must not be reported, got:\n{seen}"
    );

    // A check-in that moves the progress into another quarter wakes the sender.
    common::CliWorker::new(&mesh, &bob.nickname).checkin(&task_id, "half way there", "55");
    let events = rang
        .recv_timeout(Duration::from_secs(15))
        .expect("a quarter move must ring the sender's bell");
    assert!(
        events.contains("half way there"),
        "the bell rang for the quarter move, got:\n{events}"
    );
}

/// A worker that asks a question and then falls silent after the answer is reported too: the
/// answer hands the task back to the worker, so the sender's check-in clock runs again.
#[test]
fn a_worker_that_goes_silent_after_an_answer_is_reported_overdue() {
    let _serial = common::serial_guard();
    let (alice, mesh) = Node::create_flags("itest-answered", &FAST_CHECKIN);
    let bob = Node::join_flags(&mesh, "answered-bob", &FAST_CHECKIN);
    assert!(
        alice.wait_ready(&mesh),
        "an overdue report needs a ready initiator to receive it"
    );
    assert!(
        bob.wait_ready(&mesh),
        "an overdue report needs a ready worker to fall silent"
    );

    let task_id = common::cli_task_create(&mesh, &alice.nickname, &bob.nickname, "build it");
    common::CliWorker::new(&mesh, &bob.nickname).checkin(&task_id, "accepted: build it", "0");
    common::CliWorker::new(&mesh, &bob.nickname).state(
        &task_id,
        "input-required",
        Some("which port?"),
    );
    let question = common::cli_poll(&mesh, &alice.nickname, None);
    assert!(
        question.contains("which port?"),
        "the question must reach the sender before it answers, got:\n{question}"
    );
    let answer = common::cli_task_followup(
        &common::FollowupParams::new(&mesh, &alice.nickname, &bob.nickname, &task_id)
            .with_text("port 8080"),
    );
    assert_eq!(
        answer["result"]["task"]["id"], task_id,
        "the answer lands on the same task, got: {answer}"
    );

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut seen = String::new();
    while Instant::now() < deadline {
        seen.push_str(&common::cli_poll(&mesh, &alice.nickname, None));
        if seen.contains("\"event\":\"task_overdue\"") {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    assert!(
        seen.contains("\"event\":\"task_overdue\""),
        "alice never saw task_overdue after the answer to {}\nalice poll output:\n{seen}\nbob log:\n{}",
        bob.nickname,
        bob.log_tail(20),
    );
}

/// The worker's reminder repeats the brief it has not answered, so the agent need not look for
/// it, and a check-in answers it: the next reminder carries no text.
#[test]
fn the_workers_reminder_repeats_the_brief_until_it_is_answered() {
    let _serial = common::serial_guard();
    let (alice, mesh) = Node::create_flags("itest-reminder", &FAST_CHECKIN);
    let bob = Node::join_flags(&mesh, "reminder-bob", &FAST_CHECKIN);
    assert!(
        alice.wait_ready(&mesh),
        "the brief needs a ready initiator to send it"
    );
    assert!(
        bob.wait_ready(&mesh),
        "the reminder needs a ready worker to raise it"
    );

    let task_id = common::cli_task_create(&mesh, &alice.nickname, &bob.nickname, "build the thing");

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut seen = String::new();
    while Instant::now() < deadline {
        seen.push_str(&common::cli_poll(&mesh, &bob.nickname, None));
        if seen.contains("\"event\":\"checkin_due\"") {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    assert!(
        seen.contains("\"event\":\"checkin_due\""),
        "bob never saw checkin_due\nbob poll output:\n{seen}\nbob log:\n{}",
        bob.log_tail(20),
    );
    assert!(
        seen.contains("\"text\":\"build the thing\""),
        "the reminder must repeat the brief, got:\n{seen}"
    );

    assert!(
        seen.contains("\"brief\":true"),
        "the first reminder must say that the text is the opening brief, got:\n{seen}"
    );

    common::CliWorker::new(&mesh, &bob.nickname).checkin(&task_id, "accepted: build it", "0");
    // A reminder raised before the check-in may still wait in the ring. Read it past, so that
    // what comes after was raised after the check-in.
    std::thread::sleep(Duration::from_millis(500));
    let _older = common::cli_poll(&mesh, &bob.nickname, None);
    let again_deadline = Instant::now() + Duration::from_secs(30);
    let mut after = String::new();
    while Instant::now() < again_deadline && !after.contains("\"event\":\"checkin_due\"") {
        std::thread::sleep(Duration::from_millis(500));
        after.push_str(&common::cli_poll(&mesh, &bob.nickname, None));
    }
    assert!(
        after.contains("\"event\":\"checkin_due\""),
        "the silent worker is reminded again after its check-in, got:\n{after}"
    );
    assert!(
        !after.contains("build the thing"),
        "a check-in answers the brief, so the next reminder must not repeat it, got:\n{after}"
    );
}

/// A task that the worker never accepts is overdue as "not picked up", so the sender can tell
/// an agent that never looked from one that went quiet.
#[test]
fn an_unaccepted_task_is_reported_as_not_picked_up() {
    let _serial = common::serial_guard();
    let (alice, mesh) = Node::create_flags("itest-unpicked", &FAST_CHECKIN);
    let bob = Node::join_flags(&mesh, "unpicked-bob", &FAST_CHECKIN);
    assert!(
        alice.wait_ready(&mesh),
        "the report needs a ready initiator to receive it"
    );
    assert!(
        bob.wait_ready(&mesh),
        "the brief needs a ready worker to ignore it"
    );

    let _task_id = common::cli_task_create(&mesh, &alice.nickname, &bob.nickname, "build it");

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut seen = String::new();
    while Instant::now() < deadline {
        seen.push_str(&common::cli_poll(&mesh, &alice.nickname, None));
        if seen.contains("\"event\":\"task_overdue\"") {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    assert!(
        seen.contains("\"not_picked_up\":true"),
        "alice never saw an unpicked-up alarm\nalice poll output:\n{seen}\nbob log:\n{}",
        bob.log_tail(20),
    );
    assert!(
        seen.contains("not picked up"),
        "the line must say so, got:\n{seen}"
    );
}

/// A worker that says it waits on its own user is reported, and nothing rings: the sender has
/// nothing to do about it.
#[test]
fn an_alarm_about_a_worker_waiting_on_its_user_does_not_ring_the_bell() {
    let _serial = common::serial_guard();
    let (alice, mesh) = Node::create_flags("itest-waituser", &FAST_CHECKIN);
    let bob = Node::join_flags(&mesh, "waituser-bob", &FAST_CHECKIN);
    assert!(
        alice.wait_ready(&mesh),
        "the report needs a ready initiator to receive it"
    );
    assert!(
        bob.wait_ready(&mesh),
        "the check-in needs a ready worker to send it"
    );

    let task_id = common::cli_task_create(&mesh, &alice.nickname, &bob.nickname, "build it");
    common::CliWorker::new(&mesh, &bob.nickname).waiting_user(&task_id, "asks its user", "10");
    let mut seen = common::cli_poll(&mesh, &alice.nickname, None);
    let (rang_tx, rang) = mpsc::channel();
    let (poll_mesh, poll_nickname) = (mesh.clone(), alice.nickname.clone());
    std::thread::spawn(move || {
        let (events, _took) = common::cli_poll_long(&poll_mesh, &poll_nickname, None);
        let _ = rang_tx.send(events);
    });

    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline && !seen.contains("\"event\":\"task_overdue\"") {
        std::thread::sleep(Duration::from_millis(500));
        seen.push_str(&common::cli_poll(&mesh, &alice.nickname, None));
    }
    assert!(
        seen.contains("\"waiting_user\":true") && seen.contains("waiting on user"),
        "the alarm must say that the worker waits on its user, got:\n{seen}"
    );
    assert!(
        rang.try_recv().is_err(),
        "an alarm about a worker that waits on its user must not ring the bell"
    );
}

/// A brief longer than the cap is repeated cut, and the reminder says so.
#[test]
fn the_reminder_says_when_it_cut_a_long_brief() {
    let _serial = common::serial_guard();
    let (alice, mesh) = Node::create_flags("itest-longbrief", &FAST_CHECKIN);
    let bob = Node::join_flags(&mesh, "longbrief-bob", &FAST_CHECKIN);
    assert!(
        alice.wait_ready(&mesh),
        "the brief needs a ready initiator to send it"
    );
    assert!(
        bob.wait_ready(&mesh),
        "the reminder needs a ready worker to raise it"
    );

    let brief = "x".repeat(2_500);
    let _task_id = common::cli_task_create(&mesh, &alice.nickname, &bob.nickname, &brief);

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut seen = String::new();
    while Instant::now() < deadline && !seen.contains("\"event\":\"checkin_due\"") {
        std::thread::sleep(Duration::from_millis(500));
        seen.push_str(&common::cli_poll(&mesh, &bob.nickname, None));
    }
    assert!(
        seen.contains("\"event\":\"checkin_due\""),
        "bob never saw checkin_due\nbob poll output:\n{seen}"
    );
    assert!(
        seen.contains("\"truncated\":true"),
        "the reminder must say that it cut the brief, got:\n{seen}"
    );
    let after_marker = seen
        .split("\"event\":\"checkin_due\"")
        .nth(1)
        .expect("the poll holds a reminder line");
    let reminder = after_marker
        .split("\"is_visible\"")
        .next()
        .unwrap_or(after_marker);
    assert!(
        !reminder.contains(&brief) && reminder.contains(&brief[..1_000]),
        "the reminder must repeat the start of the brief and cut the rest, got {} bytes",
        reminder.len()
    );
}

/// A check-in interval of zero would raise an alarm on every tick, so the flag refuses it.
#[test]
fn a_zero_check_in_interval_is_refused() {
    let out = common::test_cmd()
        .args(["create", "--task-checkin-secs", "0"])
        .output()
        .expect("the test binary is built before the tests run");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "the flag must reject 0, since an alarm would fire on every tick, got: {stderr}"
    );
    assert!(
        stderr.contains("--task-checkin-secs"),
        "the error must name the flag, got: {stderr}"
    );
}
