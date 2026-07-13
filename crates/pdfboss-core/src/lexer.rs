//! Byte-level tokenizer for PDF syntax (ISO 32000 §7.2/§7.3), zero-copy
//! where possible.
//!
//! Rules: whitespace is NUL/HT/LF/FF/CR/SP; delimiters are `( ) < > [ ] { }
//! / %`; comments run from `%` to end of line; numbers may lead with `+ - .`
//! (lenient); names decode `#xx` (bad hex kept literal); literal strings
//! balance nested unescaped parentheses and support the standard escapes,
//! 1-3 digit octal, backslash-EOL continuation, and raw EOL normalized to
//! `\n`; hex strings ignore whitespace and pad an odd digit count with `0`;
//! every other regular-character run is a [`Token::Keyword`].

use crate::error::Result;
use crate::object::Name;

/// A single token produced by the [`Lexer`].
#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Int(i64),
    Real(f64),
    Name(Name),
    /// Literal string `(...)`, escapes already processed.
    LitString(Vec<u8>),
    /// Hex string `<...>`, decoded to bytes.
    HexString(Vec<u8>),
    ArrayOpen,
    ArrayClose,
    DictOpen,
    DictClose,
    /// Any bare regular-character run, e.g. `obj`, `endobj`, `stream`,
    /// `endstream`, `R`, `xref`, `trailer`, `startxref`, `true`, `false`,
    /// `null`, `n`, `f`.
    Keyword(Vec<u8>),
    Eof,
}

/// Whether `b` is PDF whitespace (ISO 32000 §7.2.2, Table 1).
pub(crate) fn is_whitespace(b: u8) -> bool {
    matches!(b, b'\0' | b'\t' | b'\n' | b'\x0C' | b'\r' | b' ')
}

/// Whether `b` is a PDF delimiter character (ISO 32000 §7.2.2, Table 2).
pub(crate) fn is_delimiter(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

/// Whether `b` is a regular character (neither whitespace nor delimiter).
pub(crate) fn is_regular(b: u8) -> bool {
    !is_whitespace(b) && !is_delimiter(b)
}

/// Value of an ASCII hex digit, if `b` is one.
fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Tokenizer over a byte slice.
pub struct Lexer<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Lexer<'a> {
    /// Creates a lexer at the start of `data`.
    pub fn new(data: &'a [u8]) -> Self {
        Lexer { data, pos: 0 }
    }

    /// Creates a lexer positioned at byte offset `pos`.
    pub fn at(data: &'a [u8], pos: usize) -> Self {
        Lexer { data, pos }
    }

    /// Current byte offset.
    pub fn pos(&self) -> usize {
        self.pos
    }

    /// Moves the cursor to byte offset `pos`.
    pub fn seek(&mut self, pos: usize) {
        self.pos = pos;
    }

    /// Consumes and returns the next token.
    pub fn next_token(&mut self) -> Result<Token> {
        self.skip_whitespace_and_comments();
        let Some(&b) = self.data.get(self.pos) else {
            return Ok(Token::Eof);
        };
        match b {
            b'[' => {
                self.pos += 1;
                Ok(Token::ArrayOpen)
            }
            b']' => {
                self.pos += 1;
                Ok(Token::ArrayClose)
            }
            b'<' => {
                if self.data.get(self.pos + 1) == Some(&b'<') {
                    self.pos += 2;
                    Ok(Token::DictOpen)
                } else {
                    self.pos += 1;
                    Ok(self.lex_hex_string())
                }
            }
            b'>' => {
                if self.data.get(self.pos + 1) == Some(&b'>') {
                    self.pos += 2;
                    Ok(Token::DictClose)
                } else {
                    // Stray `>`: surfaced leniently as a one-byte keyword.
                    self.pos += 1;
                    Ok(Token::Keyword(vec![b'>']))
                }
            }
            b'(' => {
                self.pos += 1;
                Ok(self.lex_literal_string())
            }
            b'/' => {
                self.pos += 1;
                Ok(self.lex_name())
            }
            // Stray delimiters with no token of their own: kept lenient.
            b')' | b'{' | b'}' => {
                self.pos += 1;
                Ok(Token::Keyword(vec![b]))
            }
            b'0'..=b'9' | b'+' | b'-' | b'.' => Ok(self.lex_number_or_keyword()),
            _ => Ok(self.lex_keyword()),
        }
    }

    /// Returns the next token without consuming it.
    pub fn peek_token(&mut self) -> Result<Token> {
        let save = self.pos;
        let token = self.next_token();
        self.pos = save;
        token
    }

    /// Advances past whitespace and `%` comments.
    pub fn skip_whitespace_and_comments(&mut self) {
        loop {
            while self.data.get(self.pos).is_some_and(|&b| is_whitespace(b)) {
                self.pos += 1;
            }
            if self.data.get(self.pos) == Some(&b'%') {
                while self
                    .data
                    .get(self.pos)
                    .is_some_and(|&b| b != b'\r' && b != b'\n')
                {
                    self.pos += 1;
                }
            } else {
                return;
            }
        }
    }

    /// The underlying input.
    pub fn data(&self) -> &'a [u8] {
        self.data
    }

    /// Consumes the run of regular characters starting at the cursor.
    fn take_regular_run(&mut self) -> &'a [u8] {
        let start = self.pos;
        while self.data.get(self.pos).is_some_and(|&b| is_regular(b)) {
            self.pos += 1;
        }
        &self.data[start..self.pos]
    }

    /// Lexes a run starting with a digit, sign, or period: a number when the
    /// run contains only numeric characters, otherwise a keyword (lenient).
    fn lex_number_or_keyword(&mut self) -> Token {
        let run = self.take_regular_run();
        if !run
            .iter()
            .all(|&b| matches!(b, b'0'..=b'9' | b'+' | b'-' | b'.'))
        {
            return Token::Keyword(run.to_vec());
        }
        // Fast path for well-formed numbers (the overwhelming majority):
        // parse directly off the borrowed slice with no intermediate String.
        // Integers with no `.` go to `i64`; anything else (including overflow)
        // to `f64`. Malformed runs (multiple signs/dots, bare sign) fall
        // through to the lenient cleaner below, preserving its exact result.
        if let Ok(s) = std::str::from_utf8(run) {
            if !run.contains(&b'.') {
                if let Ok(value) = s.parse::<i64>() {
                    return Token::Int(value);
                }
            }
            if let Ok(value) = s.parse::<f64>() {
                return Token::Real(value);
            }
        }
        // Lenient numeric parse: honor the first sign, then keep digits and
        // the first period; any further signs or periods are ignored.
        let mut bytes = run.iter().copied();
        let negative = match run.first() {
            Some(b'-') => {
                bytes.next();
                true
            }
            Some(b'+') => {
                bytes.next();
                false
            }
            _ => false,
        };
        let mut digits = String::new();
        let mut seen_dot = false;
        for b in bytes {
            match b {
                b'0'..=b'9' => digits.push(char::from(b)),
                b'.' if !seen_dot => {
                    seen_dot = true;
                    digits.push('.');
                }
                _ => {}
            }
        }
        if seen_dot {
            let value = if digits == "." {
                0.0
            } else {
                digits.parse::<f64>().unwrap_or(0.0)
            };
            Token::Real(if negative { -value } else { value })
        } else if digits.is_empty() {
            // A bare sign; degrade to zero rather than erroring.
            Token::Int(0)
        } else if let Ok(value) = digits.parse::<i64>() {
            Token::Int(if negative { -value } else { value })
        } else {
            // Magnitude exceeds i64: degrade to a real.
            let value = digits.parse::<f64>().unwrap_or(0.0);
            Token::Real(if negative { -value } else { value })
        }
    }

    /// Lexes a keyword: any other run of regular characters.
    fn lex_keyword(&mut self) -> Token {
        Token::Keyword(self.take_regular_run().to_vec())
    }

    /// Lexes a name after the leading `/`, decoding `#xx` escapes. A `#`
    /// not followed by two hex digits is kept literally.
    fn lex_name(&mut self) -> Token {
        let start = self.pos;
        while let Some(&b) = self.data.get(self.pos) {
            if !is_regular(b) {
                break;
            }
            self.pos += 1;
        }
        let run = &self.data[start..self.pos];
        // Fast path: no `#` escapes, so the name bytes are exactly the run —
        // convert the borrowed slice directly without a per-byte copy.
        if !run.contains(&b'#') {
            return Token::Name(Name(String::from_utf8_lossy(run).into_owned()));
        }
        // Escape path: decode `#xx`; a `#` not followed by two hex digits (or
        // at the very end of the run) is kept literally. Hex digits are regular
        // name characters, so any real `#xx` pair lies wholly within the run.
        let mut out = Vec::with_capacity(run.len());
        let mut i = 0;
        while i < run.len() {
            let b = run[i];
            if b == b'#' && i + 2 < run.len() {
                if let (Some(hi), Some(lo)) = (hex_val(run[i + 1]), hex_val(run[i + 2])) {
                    out.push((hi << 4) | lo);
                    i += 3;
                    continue;
                }
            }
            out.push(b);
            i += 1;
        }
        Token::Name(Name(String::from_utf8_lossy(&out).into_owned()))
    }

    /// Lexes a literal string after the opening `(`: balanced unescaped
    /// parentheses, all standard escapes, 1-3 digit octal, backslash-EOL
    /// line continuation, and raw EOL normalized to `\n`. An unterminated
    /// string yields whatever was accumulated (lenient).
    fn lex_literal_string(&mut self) -> Token {
        let mut out = Vec::new();
        let mut depth = 1usize;
        while let Some(&b) = self.data.get(self.pos) {
            self.pos += 1;
            match b {
                b'\\' => {
                    let Some(&esc) = self.data.get(self.pos) else {
                        break; // trailing backslash at end of input
                    };
                    self.pos += 1;
                    match esc {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(0x08),
                        b'f' => out.push(0x0C),
                        b'(' => out.push(b'('),
                        b')' => out.push(b')'),
                        b'\\' => out.push(b'\\'),
                        b'0'..=b'7' => {
                            let mut value = u32::from(esc - b'0');
                            for _ in 0..2 {
                                match self.data.get(self.pos) {
                                    Some(&d @ b'0'..=b'7') => {
                                        value = value * 8 + u32::from(d - b'0');
                                        self.pos += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push((value & 0xFF) as u8);
                        }
                        b'\r' => {
                            // Line continuation; a following LF belongs to it.
                            if self.data.get(self.pos) == Some(&b'\n') {
                                self.pos += 1;
                            }
                        }
                        b'\n' => {}               // line continuation
                        other => out.push(other), // unknown escape: byte kept, backslash dropped
                    }
                }
                b'(' => {
                    depth += 1;
                    out.push(b'(');
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Token::LitString(out);
                    }
                    out.push(b')');
                }
                b'\r' => {
                    // Raw EOL (CR or CRLF) normalizes to a single LF.
                    if self.data.get(self.pos) == Some(&b'\n') {
                        self.pos += 1;
                    }
                    out.push(b'\n');
                }
                other => out.push(other),
            }
        }
        Token::LitString(out)
    }

    /// Lexes a hex string after the opening `<`: whitespace is ignored, a
    /// trailing odd digit is padded with `0`, non-hex bytes are skipped
    /// (lenient), and a missing `>` terminates at end of input.
    fn lex_hex_string(&mut self) -> Token {
        let mut out = Vec::new();
        let mut pending: Option<u8> = None;
        while let Some(&b) = self.data.get(self.pos) {
            self.pos += 1;
            if b == b'>' {
                break;
            }
            let Some(v) = hex_val(b) else {
                continue; // whitespace and invalid bytes are skipped
            };
            match pending.take() {
                Some(hi) => out.push((hi << 4) | v),
                None => pending = Some(v),
            }
        }
        if let Some(hi) = pending {
            out.push(hi << 4);
        }
        Token::HexString(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lexes `src` to completion, asserting no errors, dropping the `Eof`.
    fn toks(src: &[u8]) -> Vec<Token> {
        let mut lexer = Lexer::new(src);
        let mut out = Vec::new();
        loop {
            let token = lexer.next_token().expect("lexing must not fail");
            if token == Token::Eof {
                return out;
            }
            out.push(token);
        }
    }

    fn one(src: &[u8]) -> Token {
        let mut all = toks(src);
        assert_eq!(all.len(), 1, "expected exactly one token in {src:?}");
        all.pop().unwrap()
    }

    #[test]
    fn numeric_forms() {
        assert_eq!(
            toks(b"+17 -98 34.5 -3.62 .5 4. -.002"),
            vec![
                Token::Int(17),
                Token::Int(-98),
                Token::Real(34.5),
                Token::Real(-3.62),
                Token::Real(0.5),
                Token::Real(4.0),
                Token::Real(-0.002),
            ]
        );
        assert_eq!(one(b"0"), Token::Int(0));
        assert_eq!(one(b"123"), Token::Int(123));
        assert_eq!(one(b"0.0"), Token::Real(0.0));
    }

    #[test]
    fn lenient_numbers() {
        // First sign wins; later signs are ignored.
        assert_eq!(one(b"--5"), Token::Int(-5));
        assert_eq!(one(b"+-3"), Token::Int(3));
        // A second period is ignored.
        assert_eq!(one(b"1.2.3"), Token::Real(1.23));
        // A lone period is 0.0; a lone sign is 0.
        assert_eq!(one(b"."), Token::Real(0.0));
        assert_eq!(one(b"-"), Token::Int(0));
        // i64 overflow degrades to a real.
        assert_eq!(one(b"99999999999999999999"), Token::Real(1e20));
        // A numeric-looking run with letters is a keyword.
        assert_eq!(one(b"1e5"), Token::Keyword(b"1e5".to_vec()));
    }

    #[test]
    fn structural_delimiters() {
        assert_eq!(
            toks(b"[]<<>>"),
            vec![
                Token::ArrayOpen,
                Token::ArrayClose,
                Token::DictOpen,
                Token::DictClose,
            ]
        );
        assert_eq!(
            toks(b"<< /Type /Page >>"),
            vec![
                Token::DictOpen,
                Token::Name(Name("Type".into())),
                Token::Name(Name("Page".into())),
                Token::DictClose,
            ]
        );
    }

    #[test]
    fn stray_delimiters_are_lenient_keywords() {
        assert_eq!(one(b")"), Token::Keyword(b")".to_vec()));
        assert_eq!(one(b"{"), Token::Keyword(b"{".to_vec()));
        assert_eq!(one(b"}"), Token::Keyword(b"}".to_vec()));
    }

    #[test]
    fn names() {
        assert_eq!(one(b"/Name1"), Token::Name(Name("Name1".into())));
        assert_eq!(one(b"/A#42"), Token::Name(Name("AB".into())));
        assert_eq!(one(b"/Bad#zz"), Token::Name(Name("Bad#zz".into())));
        assert_eq!(
            one(b"/Lime#20Green"),
            Token::Name(Name("Lime Green".into()))
        );
        assert_eq!(
            one(b"/paired#28#29parentheses"),
            Token::Name(Name("paired()parentheses".into()))
        );
        // Lowercase hex digits decode too.
        assert_eq!(one(b"/A#6f"), Token::Name(Name("Ao".into())));
        // Truncated escape at end of input is kept literally.
        assert_eq!(one(b"/A#4"), Token::Name(Name("A#4".into())));
        // The empty name is valid.
        assert_eq!(one(b"/"), Token::Name(Name(String::new())));
        // Names end at delimiters; the escape check must not read past one.
        assert_eq!(
            toks(b"/A#4/B"),
            vec![
                Token::Name(Name("A#4".into())),
                Token::Name(Name("B".into())),
            ]
        );
    }

    #[test]
    fn literal_string_basics() {
        assert_eq!(one(b"()"), Token::LitString(Vec::new()));
        assert_eq!(one(b"(hello)"), Token::LitString(b"hello".to_vec()));
        assert_eq!(one(b"(a(b)c)"), Token::LitString(b"a(b)c".to_vec()));
        assert_eq!(
            one(b"(deep(er(and(deeper))))"),
            Token::LitString(b"deep(er(and(deeper)))".to_vec())
        );
    }

    #[test]
    fn literal_string_every_escape_form() {
        assert_eq!(
            one(b"(\\n\\r\\t\\b\\f\\(\\)\\\\)"),
            Token::LitString(vec![b'\n', b'\r', b'\t', 0x08, 0x0C, b'(', b')', b'\\'])
        );
        // An unknown escape drops the backslash and keeps the byte.
        assert_eq!(one(b"(\\q)"), Token::LitString(b"q".to_vec()));
    }

    #[test]
    fn literal_string_octal_escapes() {
        assert_eq!(one(b"(\\053)"), Token::LitString(b"+".to_vec()));
        assert_eq!(one(b"(\\53)"), Token::LitString(b"+".to_vec()));
        assert_eq!(one(b"(\\5)"), Token::LitString(vec![0x05]));
        // Exactly three digits are consumed; the fourth is literal.
        assert_eq!(one(b"(\\0053)"), Token::LitString(vec![0x05, b'3']));
        // High octal values wrap to one byte.
        assert_eq!(one(b"(\\400)"), Token::LitString(vec![0x00]));
        assert_eq!(one(b"(\\777)"), Token::LitString(vec![0xFF]));
        // An octal escape terminated by a non-octal byte.
        assert_eq!(one(b"(\\1x)"), Token::LitString(vec![0x01, b'x']));
    }

    #[test]
    fn literal_string_line_continuations() {
        assert_eq!(one(b"(ab\\\ncd)"), Token::LitString(b"abcd".to_vec()));
        assert_eq!(one(b"(ab\\\rcd)"), Token::LitString(b"abcd".to_vec()));
        assert_eq!(one(b"(ab\\\r\ncd)"), Token::LitString(b"abcd".to_vec()));
    }

    #[test]
    fn literal_string_eol_normalization() {
        assert_eq!(one(b"(a\nb)"), Token::LitString(b"a\nb".to_vec()));
        assert_eq!(one(b"(a\rb)"), Token::LitString(b"a\nb".to_vec()));
        assert_eq!(one(b"(a\r\nb)"), Token::LitString(b"a\nb".to_vec()));
    }

    #[test]
    fn literal_string_unterminated_is_lenient() {
        assert_eq!(one(b"(abc"), Token::LitString(b"abc".to_vec()));
        assert_eq!(one(b"(abc\\"), Token::LitString(b"abc".to_vec()));
    }

    #[test]
    fn hex_strings() {
        assert_eq!(one(b"<>"), Token::HexString(Vec::new()));
        assert_eq!(one(b"<901FA3>"), Token::HexString(vec![0x90, 0x1F, 0xA3]));
        // Odd digit count pads a trailing zero.
        assert_eq!(one(b"<901FA>"), Token::HexString(vec![0x90, 0x1F, 0xA0]));
        // Whitespace inside is ignored.
        assert_eq!(
            one(b"<48 65\n6C\t6C 6F>"),
            Token::HexString(b"Hello".to_vec())
        );
        // Lowercase digits decode too.
        assert_eq!(
            one(b"<deadBEEF>"),
            Token::HexString(vec![0xDE, 0xAD, 0xBE, 0xEF])
        );
        // Missing `>` terminates at end of input (lenient).
        assert_eq!(one(b"<41"), Token::HexString(vec![0x41]));
    }

    #[test]
    fn comments() {
        assert_eq!(
            toks(b"1 % comment ( with ) delimiters <</junk>>\n2"),
            vec![Token::Int(1), Token::Int(2)]
        );
        assert_eq!(toks(b"%PDF-1.7\n42"), vec![Token::Int(42)]);
        // CR also ends a comment.
        assert_eq!(toks(b"% c\r7"), vec![Token::Int(7)]);
        // A comment running to end of input leaves only Eof.
        assert_eq!(toks(b"5 % trailing"), vec![Token::Int(5)]);
        assert_eq!(toks(b"%%EOF"), Vec::new());
    }

    #[test]
    fn keywords() {
        let src = b"obj endobj stream endstream R xref trailer startxref true false null n f";
        let expected: Vec<Token> = src
            .split(|&b| b == b' ')
            .map(|w| Token::Keyword(w.to_vec()))
            .collect();
        assert_eq!(toks(src), expected);
    }

    #[test]
    fn indirect_reference_shape() {
        assert_eq!(
            toks(b"12 0 R"),
            vec![Token::Int(12), Token::Int(0), Token::Keyword(b"R".to_vec()),]
        );
    }

    #[test]
    fn eof_behavior() {
        let mut lexer = Lexer::new(b"");
        assert_eq!(lexer.next_token().unwrap(), Token::Eof);
        // Eof is sticky: repeated calls keep returning it.
        assert_eq!(lexer.next_token().unwrap(), Token::Eof);

        let mut lexer = Lexer::new(b"  \t\r\n \x00\x0C ");
        assert_eq!(lexer.next_token().unwrap(), Token::Eof);

        let mut lexer = Lexer::new(b"1");
        assert_eq!(lexer.next_token().unwrap(), Token::Int(1));
        assert_eq!(lexer.next_token().unwrap(), Token::Eof);
        assert_eq!(lexer.next_token().unwrap(), Token::Eof);

        // Seeking past the end is Eof, not a panic.
        let mut lexer = Lexer::new(b"abc");
        lexer.seek(100);
        assert_eq!(lexer.next_token().unwrap(), Token::Eof);
    }

    #[test]
    fn peek_does_not_consume() {
        let mut lexer = Lexer::new(b"/A 1");
        let before = lexer.pos();
        assert_eq!(lexer.peek_token().unwrap(), Token::Name(Name("A".into())));
        assert_eq!(lexer.pos(), before, "peek must not move the cursor");
        assert_eq!(lexer.peek_token().unwrap(), Token::Name(Name("A".into())));
        assert_eq!(lexer.next_token().unwrap(), Token::Name(Name("A".into())));
        assert_eq!(lexer.peek_token().unwrap(), Token::Int(1));
        assert_eq!(lexer.next_token().unwrap(), Token::Int(1));
        assert_eq!(lexer.peek_token().unwrap(), Token::Eof);
    }

    #[test]
    fn seek_round_trips() {
        let src = b"[ /Key (val) 42 ]";
        let mut lexer = Lexer::new(src);
        assert_eq!(lexer.next_token().unwrap(), Token::ArrayOpen);
        let mark = lexer.pos();
        assert_eq!(lexer.next_token().unwrap(), Token::Name(Name("Key".into())));
        assert_eq!(
            lexer.next_token().unwrap(),
            Token::LitString(b"val".to_vec())
        );
        // Rewind and replay the same tokens.
        lexer.seek(mark);
        assert_eq!(lexer.next_token().unwrap(), Token::Name(Name("Key".into())));
        assert_eq!(
            lexer.next_token().unwrap(),
            Token::LitString(b"val".to_vec())
        );
        assert_eq!(lexer.next_token().unwrap(), Token::Int(42));
        assert_eq!(lexer.next_token().unwrap(), Token::ArrayClose);

        // `at` starts mid-buffer at the same place `seek` would reach.
        let mut resumed = Lexer::at(src, mark);
        assert_eq!(
            resumed.next_token().unwrap(),
            Token::Name(Name("Key".into()))
        );
        assert_eq!(resumed.data(), src);
    }

    #[test]
    fn skip_whitespace_and_comments_stops_at_token() {
        let mut lexer = Lexer::new(b"  % one\n % two\r\n  7");
        lexer.skip_whitespace_and_comments();
        assert_eq!(lexer.data()[lexer.pos()], b'7');
        // Idempotent when already at a token.
        let pos = lexer.pos();
        lexer.skip_whitespace_and_comments();
        assert_eq!(lexer.pos(), pos);
    }

    #[test]
    fn mixed_stream_of_tokens() {
        assert_eq!(
            toks(b"<</N 3/Root 1 0 R>>[(a)<62>/c true]"),
            vec![
                Token::DictOpen,
                Token::Name(Name("N".into())),
                Token::Int(3),
                Token::Name(Name("Root".into())),
                Token::Int(1),
                Token::Int(0),
                Token::Keyword(b"R".to_vec()),
                Token::DictClose,
                Token::ArrayOpen,
                Token::LitString(b"a".to_vec()),
                Token::HexString(b"b".to_vec()),
                Token::Name(Name("c".into())),
                Token::Keyword(b"true".to_vec()),
                Token::ArrayClose,
            ]
        );
    }

    #[test]
    fn character_classes() {
        for b in [0x00u8, b'\t', b'\n', 0x0C, b'\r', b' '] {
            assert!(is_whitespace(b), "{b:#04x} should be whitespace");
            assert!(!is_regular(b));
        }
        for b in *b"()<>[]{}/%" {
            assert!(is_delimiter(b), "{} should be a delimiter", b as char);
            assert!(!is_regular(b));
        }
        for b in *b"aZ09+-.#_*'\"" {
            assert!(is_regular(b), "{} should be regular", b as char);
        }
    }
}
