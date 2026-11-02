use anyhow::Result;

/// `join` has no `--name`: it is encoded in the id
/// identifier and auto-detected. Without this, clap rejects it with
/// a generic "unexpected argument" + a misleading "pass as a value"
/// tip; this gives the real reason instead.
pub(super) fn reject_id_encoded_flag(flag: &str, present: bool) -> Result<()> {
    if present {
        anyhow::bail!(
            "`{flag}` is not valid for `join`: the mesh's name, lookups and \
             transport policy are encoded in the mesh id and auto-detected. \
             Drop `{flag}` — `join` takes only the id and `--nickname`."
        );
    }
    Ok(())
}
