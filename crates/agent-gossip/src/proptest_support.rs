use proptest::test_runner::{Config, FileFailurePersistence};

pub(crate) fn config() -> Config {
    Config {
        failure_persistence: Some(Box::new(FileFailurePersistence::SourceParallel(
            "tests/proptest-regressions",
        ))),
        ..Config::default()
    }
}
