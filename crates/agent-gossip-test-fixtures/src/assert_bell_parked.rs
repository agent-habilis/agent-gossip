use std::process::Child;

pub fn assert_bell_parked(bell: &mut Child, why: &str) {
    assert!(bell.try_wait().expect("try_wait").is_none(), "{why}");
}
