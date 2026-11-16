/// Whether `args` names a lookup, as `--lookup value` or as `--lookup=value`.
///
/// A spawned `create` that names none gets `--lookup loopback`, because the
/// default lookup reaches the public network.
#[must_use]
pub fn names_lookup(args: &[&str]) -> bool {
    args.iter()
        .any(|arg| *arg == "--lookup" || arg.starts_with("--lookup="))
}

#[cfg(test)]
mod tests {
    use super::names_lookup;

    #[test]
    fn both_spellings_of_the_flag_name_a_lookup() {
        assert!(names_lookup(&["--lookup", "relay"]), "the split spelling");
        assert!(names_lookup(&["--lookup=mdns,dht"]), "the joined spelling");
    }

    #[test]
    fn other_flags_name_no_lookup() {
        assert!(
            !names_lookup(&["--transport", "udp", "--lookups"]),
            "a flag that only starts like it is not the lookup flag"
        );
    }
}
