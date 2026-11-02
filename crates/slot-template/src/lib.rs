//! Pure-function markdown templating over two HTML-comment directives, so a
//! template source stays valid, cleanly-rendering markdown:
//!
//! - `<!-- include path="..." key="value" ... -->` — alone on its own line:
//!   splice another file, resolved relative to the file containing the line,
//!   rendered with EXACTLY the args named at that call site.
//! - `<!-- slot name="..." -->` — anywhere: the substitution point an include
//!   arg fills.
//!
//! No engine, no ambient context, no escaping of substituted values. Strict
//! in both directions — a slot without an arg and an arg without a slot are
//! both errors — so call sites are checked like function signatures. A
//! comment that starts as a directive but fails the grammar is an error,
//! never silently literal; every other HTML comment and every other byte
//! passes through byte-exact.
//!
//! # Example
//!
//! ```
//! use std::path::Path;
//!
//! use slot_template::expand;
//!
//! let template = "Hello <!-- slot name=\"who\" -->\n";
//! let mut loader = |_: &Path| Ok(template.to_owned());
//! let out = expand(Path::new("greeting.md"), &[("who", "world")], &mut loader);
//! assert_eq!(out.expect("the template renders"), "Hello world\n");
//! ```

pub use self::error::Error;
pub use self::expand::expand;

mod directive_word;
mod error;
mod expand;
mod expand_at;
mod is_name_char;
mod max_depth;
mod normalize;
mod parse_named_args;
mod parse_quoted_value;
mod render;
#[cfg(test)]
mod tests;
