use std::path::{Component, Path, PathBuf};

/// Lexical path normalization (`a/../b` → `b`) so cycle detection and the
/// loader see one canonical spelling per file, without touching the fs.
pub(crate) fn normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            // `..` may only cancel a *named* component. Popping blindly makes a
            // leading `..` cancel the one before it (`../../x` → `x`), and root
            // has no parent to climb to (`/..` → `/`).
            Component::ParentDir => match normalized.components().next_back() {
                Some(Component::Normal(_)) => {
                    normalized.pop();
                }
                Some(Component::RootDir) => {}
                _ => normalized.push(".."),
            },
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component);
            }
        }
    }
    normalized
}
