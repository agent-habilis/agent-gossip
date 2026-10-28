use std::path::Path;

use crate::error::Error;
use crate::expand_at::expand_at;

/// Load `path` through `loader` and render it: own-line
/// `<!-- include path="..." ... -->` lines splice the named file (resolved
/// relative to the including file, rendered with exactly the args at that
/// call site), and `<!-- slot -->` directives substitute from `args`. Spliced
/// content is final — it is never re-substituted by the includer.
///
/// # Errors
/// Everything [`render`] returns, plus [`Error::MalformedInclude`] for a bad
/// include line, [`Error::IncludeCycle`] / [`Error::DepthExceeded`] for
/// degenerate include graphs, and [`Error::Load`] when `loader` fails.
pub fn expand(
    path: &Path,
    args: &[(&str, &str)],
    loader: &mut dyn FnMut(&Path) -> Result<String, String>,
) -> Result<String, Error> {
    expand_at(path, args, loader, &mut Vec::new())
}
