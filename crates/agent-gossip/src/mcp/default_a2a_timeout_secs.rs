use crate::a2a::tuning;
pub(crate) fn default_a2a_timeout_secs() -> u64 {
    tuning::CALL_TIMEOUT_SECS
}
