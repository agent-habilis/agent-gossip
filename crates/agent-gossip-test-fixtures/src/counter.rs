use std::sync::atomic::AtomicU32;

pub(crate) static COUNTER: AtomicU32 = AtomicU32::new(0);
