use std::path::{Path, PathBuf};

use crate::directive_word::directive_word;
use crate::error::Error;
use crate::max_depth::MAX_DEPTH;
use crate::normalize::normalize;
use crate::parse_named_args::parse_named_args;
use crate::render::render;

// Never valid directive text, so the slot pass of the includer leaves it alone.
const SENTINEL: char = '\u{0}';

pub(crate) fn expand_at(
    path: &Path,
    args: &[(&str, &str)],
    loader: &mut dyn FnMut(&Path) -> Result<String, String>,
    stack: &mut Vec<PathBuf>,
) -> Result<String, Error> {
    let path = normalize(path);
    if stack.contains(&path) {
        return Err(Error::IncludeCycle { path });
    }
    if stack.len() >= MAX_DEPTH {
        return Err(Error::DepthExceeded { path });
    }
    let content = loader(&path).map_err(|reason| Error::Load {
        path: path.clone(),
        reason,
    })?;
    let base = path.parent().map(Path::to_path_buf).unwrap_or_default();
    stack.push(path);

    // Pull include lines out first, leaving sentinels (never valid directive
    // text), so the includer's own slot pass cannot touch spliced content.
    let mut text = String::with_capacity(content.len());
    let mut expansions: Vec<String> = Vec::new();
    for (index, line) in content.split_inclusive('\n').enumerate() {
        let trimmed = line.trim();
        let is_include_line = trimmed
            .strip_prefix("<!--")
            .is_some_and(|interior| directive_word(interior) == "include");
        if !is_include_line {
            text.push_str(line);
            continue;
        }
        let number = index + 1;
        let malformed = |reason: String| Error::MalformedInclude {
            line: number,
            reason,
        };
        let after_open = trimmed.strip_prefix("<!--");
        let interior: &str = after_open
            .and_then(|rest| rest.strip_suffix("-->"))
            .ok_or_else(|| malformed("unterminated include comment".to_owned()))?;
        if interior.contains("-->") {
            return Err(malformed(
                "include line carries content after the directive".to_owned(),
            ));
        }
        let tail = interior.trim_start().trim_start_matches("include");
        let mut pairs = parse_named_args(tail).map_err(&malformed)?;
        let target_position = pairs
            .iter()
            .position(|(key, _)| key == "path")
            .ok_or_else(|| malformed("include requires path=\"...\"".to_owned()))?;
        let (_, target) = pairs.remove(target_position);
        let borrowed_pairs = pairs
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()));
        let child_args: Vec<(&str, &str)> = borrowed_pairs.collect();
        let expanded = expand_at(&base.join(target), &child_args, loader, stack)?;
        text.push(SENTINEL);
        if line.ends_with('\n') {
            text.push('\n');
        }
        expansions.push(expanded);
    }

    let rendered = render(&text, args)?;
    let mut pieces = rendered.split(SENTINEL);
    let mut spliced = String::with_capacity(rendered.len());
    spliced.push_str(pieces.next().unwrap_or_default());
    for (expanded, piece) in expansions.iter().zip(pieces) {
        spliced.push_str(expanded);
        spliced.push_str(piece);
    }
    stack.pop();
    Ok(spliced)
}
