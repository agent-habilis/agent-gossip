use crate::test_cmd;

/// The daemon of a worker, as a test drives it through the CLI (`agent-gossip a2a status`).
///
/// A `working` status is a check-in: [`checkin`](Self::checkin) says what changed and how far
/// the work is, and [`waiting_user`](Self::waiting_user) adds that the worker waits on its user.
/// [`state`](Self::state) sends any other state.
#[derive(Debug, Clone, Copy)]
pub struct CliWorker<'caller> {
    mesh: &'caller str,
    nickname: &'caller str,
}

impl<'caller> CliWorker<'caller> {
    /// The worker `nickname` of the mesh `mesh`.
    #[must_use]
    pub fn new(mesh: &'caller str, nickname: &'caller str) -> Self {
        Self { mesh, nickname }
    }

    /// Check in on `task_id`: `text` says what changed, `progress` is a whole percentage.
    ///
    /// # Panics
    ///
    /// Panics if the CLI process fails to spawn or exits with an error.
    pub fn checkin(self, task_id: &str, text: &str, progress: &str) {
        self.send(
            task_id,
            &["--state", "working", "--text", text, "--progress", progress],
        );
    }

    /// Check in as [`checkin`](Self::checkin) does, and say that the worker waits on its user.
    ///
    /// # Panics
    ///
    /// Panics if the CLI process fails to spawn or exits with an error.
    pub fn waiting_user(self, task_id: &str, text: &str, progress: &str) {
        self.send(
            task_id,
            &[
                "--state",
                "working",
                "--text",
                text,
                "--progress",
                progress,
                "--waiting-on-user",
            ],
        );
    }

    /// Move `task_id` to a state other than `working`, with a message if `text` is given.
    ///
    /// # Panics
    ///
    /// Panics if the CLI process fails to spawn or exits with an error.
    pub fn state(self, task_id: &str, state: &str, text: Option<&str>) {
        let mut args = vec!["--state", state];
        if let Some(text) = text {
            args.extend(["--text", text]);
        }
        self.send(task_id, &args);
    }

    fn send(self, task_id: &str, leg: &[&str]) {
        let out = test_cmd()
            .args([
                "a2a",
                "status",
                "--gossip",
                self.mesh,
                "--nickname",
                self.nickname,
                "--task-id",
                task_id,
            ])
            .args(leg)
            .output()
            .expect("a2a status command failed to spawn");
        assert!(
            out.status.success(),
            "a2a status failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
