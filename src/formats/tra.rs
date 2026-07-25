//! TRA (translation) file parser: `@N = ~text~` entries mapping a numeric
//! reference to the string a `@N` token stands for in D/BAF source.
//!
//! WeiDU substitutes each `@N` with its text before compiling, then resolves a
//! string that lands in a strref slot to a `dialog.tlk` index. We only need the
//! `@N -> text` map here; the tlk lookup lives in [`crate::resource`]. Grammar:
//! `md/weidu/04-d-dialogue-file-format.md` (`String`, `text`).

use std::collections::HashMap;
use std::fmt;

/// The five-tilde string delimiter, for text containing a lone `~` or `"`.
const FENCE: &[u8] = b"~~~~~";

/// Parse a TRA file into its `@N -> text` map. A repeated `@N` keeps the last,
/// as WeiDU does ("the last definition of the translation string ... is used").
/// Only the first (male) string of an entry is kept: that is the one resolved
/// against `DIALOG.TLK`; a trailing `[WAVEFILE]` and female variant are skipped.
pub fn parse(bytes: &[u8]) -> Result<HashMap<u32, String>, TraError> {
    let mut p = Parser {
        data: bytes,
        pos: 0,
    };
    let mut out = HashMap::new();
    p.skip_trivia();
    while !p.at_end() {
        let (index, text) = p.entry()?;
        out.insert(index, text);
        p.skip_trivia();
    }
    Ok(out)
}

/// A cursor over the TRA text.
struct Parser<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    /// One `@N = <String> [WAVEFILE] [<String> [WAVEFILE]]` entry, returning the
    /// reference and its primary string; trailing sound/female forms are read to
    /// stay in step but discarded.
    fn entry(&mut self) -> Result<(u32, String), TraError> {
        self.expect(b'@')?;
        let index = self.integer()?;
        self.skip_trivia();
        self.expect(b'=')?;
        let text = self.string()?;
        self.skip_wavefile();
        // A second string is the female variant; it shares the reference.
        if matches!(self.peek_nontrivia(), Some(b'~' | b'"' | b'%')) {
            self.string()?;
            self.skip_wavefile();
        }
        Ok((index, text))
    }

    /// A double-quoted, tilde-quoted, five-tilde, or percent-quoted string. The
    /// format has no escapes; the delimiter simply cannot occur in the body.
    fn string(&mut self) -> Result<String, TraError> {
        self.skip_trivia();
        let at = self.pos;
        if self.data.get(self.pos..self.pos + FENCE.len()) == Some(FENCE) {
            self.pos += FENCE.len();
            return self.take_until(FENCE, at);
        }
        match self.peek() {
            Some(delim @ (b'~' | b'"' | b'%')) => {
                self.pos += 1;
                self.take_until(&[delim], at)
            }
            _ => Err(TraError::Malformed {
                at,
                what: "expected a string",
            }),
        }
    }

    /// The bytes up to the next `close`, consuming it. Errors if it never comes.
    fn take_until(&mut self, close: &[u8], open_at: usize) -> Result<String, TraError> {
        let start = self.pos;
        while !self.at_end() {
            if self.data.get(self.pos..self.pos + close.len()) == Some(close) {
                let body = self.data.get(start..self.pos).unwrap_or_default();
                self.pos += close.len();
                return Ok(String::from_utf8_lossy(body).into_owned());
            }
            self.pos += 1;
        }
        Err(TraError::Malformed {
            at: open_at,
            what: "unterminated string",
        })
    }

    /// A non-negative decimal reference number.
    fn integer(&mut self) -> Result<u32, TraError> {
        let start = self.pos;
        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.pos += 1;
        }
        std::str::from_utf8(self.data.get(start..self.pos).unwrap_or_default())
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or(TraError::Malformed {
                at: start,
                what: "expected a number after @",
            })
    }

    /// Skip an optional `[soundref]` following a string.
    fn skip_wavefile(&mut self) {
        self.skip_trivia();
        if self.peek() == Some(b'[') {
            while self.peek().is_some_and(|b| b != b']') {
                self.pos += 1;
            }
            self.pos += 1; // the ']' itself, or a no-op at end of input
        }
    }

    /// Skip whitespace and comments (`//` to end of line, `/* … */` blocks).
    fn skip_trivia(&mut self) {
        loop {
            match self.peek() {
                Some(b) if b.is_ascii_whitespace() => self.pos += 1,
                Some(b'/') if self.data.get(self.pos + 1) == Some(&b'/') => {
                    while self.peek().is_some_and(|b| b != b'\n') {
                        self.pos += 1;
                    }
                }
                Some(b'/') if self.data.get(self.pos + 1) == Some(&b'*') => {
                    self.pos += 2;
                    while self.peek().is_some()
                        && self.data.get(self.pos..self.pos + 2) != Some(b"*/")
                    {
                        self.pos += 1;
                    }
                    self.pos = (self.pos + 2).min(self.data.len());
                }
                _ => break,
            }
        }
    }

    fn at_end(&self) -> bool {
        self.pos >= self.data.len()
    }

    fn peek(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    /// The next byte that is not whitespace or a comment, without consuming.
    fn peek_nontrivia(&mut self) -> Option<u8> {
        self.skip_trivia();
        self.peek()
    }

    fn expect(&mut self, byte: u8) -> Result<(), TraError> {
        self.skip_trivia();
        if self.peek() == Some(byte) {
            self.pos += 1;
            Ok(())
        } else {
            Err(TraError::Malformed {
                at: self.pos,
                what: "unexpected byte",
            })
        }
    }
}

/// Anything that can go wrong parsing a TRA file.
#[derive(Debug, PartialEq, Eq)]
pub enum TraError {
    Malformed { at: usize, what: &'static str },
}

impl fmt::Display for TraError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TraError::Malformed { at, what } => write!(f, "malformed TRA: {what} at byte {at}"),
        }
    }
}

impl std::error::Error for TraError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_tilde_and_quote_entries_keeping_the_last_alias() {
        let tra = b"// journal\n@5521 = ~Buried Ambitions\nAt Spellhold.~\n\
                    @5680 = \"...\"\n@5521 = ~later wins~\n";
        let map = parse(tra).expect("parses");
        assert_eq!(map.get(&5521).map(String::as_str), Some("later wins"));
        assert_eq!(map.get(&5680).map(String::as_str), Some("..."));
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn skips_wavefile_and_female_variant() {
        // The male string is kept; the sound and female form are consumed but
        // dropped, so the following entry still parses.
        let tra = b"@10 = ~male~ [SOUND] ~female~ [SNDF]\n@11 = ~next~\n";
        let map = parse(tra).expect("parses");
        assert_eq!(map.get(&10).map(String::as_str), Some("male"));
        assert_eq!(map.get(&11).map(String::as_str), Some("next"));
    }

    #[test]
    fn five_tilde_holds_a_bare_tilde() {
        let tra = b"@1 = ~~~~~a ~ b \" c~~~~~\n";
        let map = parse(tra).expect("parses");
        assert_eq!(map.get(&1).map(String::as_str), Some("a ~ b \" c"));
    }

    #[test]
    fn an_unterminated_string_errors_not_panics() {
        assert!(matches!(
            parse(b"@1 = ~oops"),
            Err(TraError::Malformed { .. })
        ));
    }
}
