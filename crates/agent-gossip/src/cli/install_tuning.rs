/// Install both process tunings from one flag set. The flags are a single
/// surface to an operator, but they land in two homes: engine knobs in
/// `habilis_network::util::tuning`, the task/long-poll knobs in
/// `a2a::tuning`. Kept together here so a new flag cannot reach one and miss
/// the other.
pub(super) fn install_tuning(options: &super::args::tuning_opts::TuningOpts) {
    habilis_network::runtime::tuning::init(options.tuning());
    crate::a2a::tuning::init(options.a2a_tuning());
}
