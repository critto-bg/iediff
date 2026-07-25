//! ImprovedAnvil BAF preprocessor: `@@name` marker lines expanded inline from a
//! directory of template fragments, before the source reaches the compiler.
//!
//! This is a *mod* convention, not part of the engine's BAF format — see the
//! "Mod-specific features" note in `CLAUDE.md`. It mirrors the mod's own
//! `lib/tpa/baf_preprocessor.tpa`: a whole line `@@<name>` is replaced by
//! `<dir>/<name>.baf`, less one trailing newline, and a marker naming a template
//! the directory lacks is a hard error.

use std::fmt;
use std::path::{Path, PathBuf};

/// Expand every `@@name` marker line in `source` against `templates_dir`,
/// returning the flattened BAF to compile. A single pass over the source only:
/// a fragment is inserted verbatim, so a `@@` inside one is not re-expanded —
/// matching the mod's one-shot `REPLACE_EVALUATE`.
pub fn expand(source: &[u8], templates_dir: &Path) -> Result<Vec<u8>, PreprocessError> {
    let mut out = Vec::with_capacity(source.len());
    let mut pos = 0;
    while let Some(rest) = source.get(pos..).filter(|r| !r.is_empty()) {
        // One line plus its `\n`, if any; `content` drops the newline for testing.
        let line_len = rest.iter().position(|&b| b == b'\n').map_or(rest.len(), |i| i + 1);
        let line = rest.get(..line_len).unwrap_or_default();
        let content = line.strip_suffix(b"\n").unwrap_or(line);

        match directive_name(content) {
            Some(name) => {
                let path = templates_dir.join(name).with_extension("baf");
                let template = std::fs::read(&path)
                    .map_err(|source| PreprocessError { name: name.to_owned(), path, source })?;
                out.extend_from_slice(strip_trailing_newline(&template));
                // Keep the marker line's own newline so the neighbouring blocks
                // stay separated.
                if line.len() > content.len() {
                    out.push(b'\n');
                }
            }
            None => out.extend_from_slice(line),
        }
        pos += line_len;
    }
    Ok(out)
}

/// The `<name>` of a whole-line `@@<name>` marker, else `None`. The mod's regex
/// is `^@@([a-z0-9_]+)$` — the *entire* line — so an indented or trailing-token
/// `@@` is left alone (and then rejected by the compiler, as WeiDU leaves it).
fn directive_name(content: &[u8]) -> Option<&str> {
    // Tolerate a lone trailing CR so a CRLF-saved script still matches.
    let name = content.strip_prefix(b"@@")?;
    let name = name.strip_suffix(b"\r").unwrap_or(name);
    let allowed = |b: &u8| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_';
    if !name.is_empty() && name.iter().all(allowed) {
        std::str::from_utf8(name).ok()
    } else {
        None
    }
}

/// Drop a single trailing newline, matching the mod's `READ_ASCII 0
/// (SOURCE_SIZE - 1)`: fragments are authored with one, and dropping it keeps
/// the marker line's own newline from doubling.
fn strip_trailing_newline(template: &[u8]) -> &[u8] {
    template.strip_suffix(b"\n").unwrap_or(template)
}

/// A `@@name` marker whose `<name>.baf` template could not be read.
#[derive(Debug)]
pub struct PreprocessError {
    name: String,
    path: PathBuf,
    source: std::io::Error,
}

impl fmt::Display for PreprocessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "@@{}: could not read template {}: {}",
            self.name,
            self.path.display(),
            self.source
        )
    }
}

impl std::error::Error for PreprocessError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    /// A templates directory holding `<name>.baf` fragments, unique per call so
    /// concurrent tests never collide. Removed when the test's binding drops.
    struct Templates {
        dir: PathBuf,
    }

    impl Templates {
        fn with(fragments: &[(&str, &[u8])]) -> Templates {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("iediff-pp-{}-{n}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).expect("create templates dir");
            for (name, body) in fragments {
                fs::write(dir.join(name).with_extension("baf"), body).expect("write fragment");
            }
            Templates { dir }
        }
    }

    impl Drop for Templates {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn a_marker_line_is_replaced_by_its_fragment() {
        let templates = Templates::with(&[("vocalize", b"IF\n  True()\nEND\n")]);
        let out = expand(b"IF\n  A()\nEND\n\n@@vocalize\n\nIF\n  B()\nEND\n", &templates.dir)
            .expect("expands");
        // The fragment's own trailing newline is dropped; the marker line's is
        // kept, so exactly one blank line survives on either side.
        assert_eq!(
            out,
            b"IF\n  A()\nEND\n\nIF\n  True()\nEND\n\nIF\n  B()\nEND\n"
        );
    }

    #[test]
    fn an_indented_or_inline_marker_is_left_untouched() {
        // `^@@name$` matches the whole line only; these stay verbatim (and would
        // then fail to compile, exactly as WeiDU leaves them).
        let templates = Templates::with(&[("vocalize", b"X\n")]);
        let src = b"  @@vocalize\nDoThing(@@vocalize)\n";
        let out = expand(src, &templates.dir).expect("expands");
        assert_eq!(out, src);
    }

    #[test]
    fn an_unknown_marker_names_the_missing_template() {
        let templates = Templates::with(&[]);
        let err = expand(b"@@missing\n", &templates.dir).expect_err("unknown marker");
        assert_eq!(err.name, "missing");
        assert!(err.path.ends_with("missing.baf"));
    }
}
