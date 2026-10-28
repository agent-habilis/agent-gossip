/// Install both process tunings from one flag set. The flags are a single
/// surface to an operator, but they land in two homes: engine knobs in
/// `habilis_network::util::tuning`, the task/long-poll knobs in
/// `a2a::tuning`. Kept together here so a new flag cannot reach one and miss
/// the other.
use habilis_network::runtime::tuning;

use crate::a2a::tuning as a2a_tuning;
pub(super) fn install_tuning(options: &super::args::tuning_opts::TuningOpts) {
    tuning::init(options.tuning());
    a2a_tuning::init(options.a2a_tuning());
}
