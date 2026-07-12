//! Object parser built on the [`Lexer`], including indirect objects and
//! streams (ISO 32000 §7.3.8/§7.3.10).

use crate::error::{Error, Result};
use crate::lexer::{Lexer, Token};
use crate::object::{Dict, ObjRef, Object, Stream};

/// Maximum container (array/dictionary) nesting depth. Deeper input is
/// rejected with a syntax error: the parser recurses per nesting level, so
/// without a bound a tiny crafted file (e.g. 100k `[`s) overflows the
/// stack and aborts the whole process. Real documents nest a handful of
/// levels; 128 is far beyond anything legitimate.
const MAX_NESTING_DEPTH: usize = 128;

/// Resolves indirect references while parsing (e.g. an indirect `/Length`).
pub trait Resolve {
    /// Returns the referenced object, or `None` if it cannot be resolved.
    fn resolve_ref(&self, r: ObjRef) -> Option<Object>;
}

/// A [`Resolve`] implementation that never resolves anything.
pub struct NoResolve;

impl Resolve for NoResolve {
    fn resolve_ref(&self, _r: ObjRef) -> Option<Object> {
        None
    }
}

/// Recursive-descent parser over a byte slice.
pub struct Parser<'a> {
    lexer: Lexer<'a>,
}

impl<'a> Parser<'a> {
    /// Creates a parser at the start of `data`.
    pub fn new(data: &'a [u8]) -> Self {
        Parser {
            lexer: Lexer::new(data),
        }
    }

    /// Creates a parser positioned at byte offset `pos`.
    pub fn at(data: &'a [u8], pos: usize) -> Self {
        Parser {
            lexer: Lexer::at(data, pos),
        }
    }

    /// Current byte offset.
    pub fn pos(&self) -> usize {
        self.lexer.pos()
    }

    /// Moves the cursor to byte offset `pos`.
    pub fn seek(&mut self, pos: usize) {
        self.lexer.seek(pos);
    }

    /// Parses one object at the current position. `int int R` becomes
    /// [`Object::Ref`] via lookahead with backtracking; dictionaries
    /// followed by `stream` become [`Object::Stream`] (using `resolver`
    /// for an indirect `/Length`, with `endstream` scan recovery).
    pub fn parse_object(&mut self, resolver: &dyn Resolve) -> Result<Object> {
        let token = self.lexer.next_token()?;
        self.parse_from_token(token, resolver, 0)
    }

    /// Builds a [`Error::Syntax`] at the current position.
    fn syntax(&self, msg: impl Into<String>) -> Error {
        Error::Syntax {
            offset: self.lexer.pos(),
            msg: msg.into(),
        }
    }

    /// Parses the object that `token` begins. `depth` counts enclosing
    /// containers (bounded by `MAX_NESTING_DEPTH`).
    fn parse_from_token(
        &mut self,
        token: Token,
        resolver: &dyn Resolve,
        depth: usize,
    ) -> Result<Object> {
        match token {
            Token::Int(i) => Ok(self.try_reference(i)),
            Token::Real(r) => Ok(Object::Real(r)),
            Token::Name(n) => Ok(Object::Name(n)),
            Token::LitString(s) | Token::HexString(s) => Ok(Object::String(s)),
            Token::ArrayOpen => self.parse_array(resolver, depth),
            Token::DictOpen => self.parse_dict_or_stream(resolver, depth),
            Token::Keyword(k) => match k.as_slice() {
                b"true" => Ok(Object::Bool(true)),
                b"false" => Ok(Object::Bool(false)),
                b"null" => Ok(Object::Null),
                _ => Err(self.syntax(format!(
                    "unexpected keyword `{}`",
                    String::from_utf8_lossy(&k)
                ))),
            },
            Token::ArrayClose => Err(self.syntax("unexpected `]`")),
            Token::DictClose => Err(self.syntax("unexpected `>>`")),
            Token::Eof => Err(self.syntax("unexpected end of input")),
        }
    }

    /// After an integer has been read: if `int R` follows, the three tokens
    /// form an indirect reference; otherwise the lexer is rewound so the
    /// integer stands alone.
    fn try_reference(&mut self, num: i64) -> Object {
        let save = self.lexer.pos();
        if (0..=i64::from(u32::MAX)).contains(&num) {
            if let Ok(Token::Int(gen)) = self.lexer.next_token() {
                if (0..=i64::from(u16::MAX)).contains(&gen)
                    && matches!(self.lexer.next_token(),
                                Ok(Token::Keyword(ref k)) if k.as_slice() == b"R")
                {
                    return Object::Ref(ObjRef {
                        num: num as u32,
                        gen: gen as u16,
                    });
                }
            }
        }
        self.lexer.seek(save);
        Object::Int(num)
    }

    /// Parses array elements up to `]` (leniently also up to end of input).
    fn parse_array(&mut self, resolver: &dyn Resolve, depth: usize) -> Result<Object> {
        if depth >= MAX_NESTING_DEPTH {
            return Err(self.syntax("container nesting too deep"));
        }
        let mut items = Vec::new();
        loop {
            let token = self.lexer.next_token()?;
            match token {
                Token::ArrayClose | Token::Eof => break,
                other => items.push(self.parse_from_token(other, resolver, depth + 1)?),
            }
        }
        Ok(Object::Array(items))
    }

    /// Parses dictionary entries up to `>>`; if the `stream` keyword follows,
    /// the dictionary becomes a stream's dictionary and the stream data is
    /// read as well.
    fn parse_dict_or_stream(&mut self, resolver: &dyn Resolve, depth: usize) -> Result<Object> {
        if depth >= MAX_NESTING_DEPTH {
            return Err(self.syntax("container nesting too deep"));
        }
        let mut dict = Dict::new();
        loop {
            match self.lexer.next_token()? {
                Token::DictClose | Token::Eof => break,
                Token::Name(key) => {
                    let token = self.lexer.next_token()?;
                    if token == Token::DictClose {
                        // Lenient: a key with no value maps to null.
                        dict.insert(key, Object::Null);
                        break;
                    }
                    let value = self.parse_from_token(token, resolver, depth + 1)?;
                    dict.insert(key, value);
                }
                // Lenient: a non-name key is consumed as an object and dropped.
                other => {
                    self.parse_from_token(other, resolver, depth + 1)?;
                }
            }
        }
        if matches!(self.lexer.peek_token(),
                    Ok(Token::Keyword(ref k)) if k.as_slice() == b"stream")
        {
            self.lexer.next_token()?;
            return self.parse_stream_body(dict, resolver);
        }
        Ok(Object::Dict(dict))
    }

    /// Reads stream data after the `stream` keyword (ISO 32000 §7.3.8):
    /// skip the EOL after the keyword, take `/Length` bytes (resolving an
    /// indirect length via `resolver`), and verify `endstream` follows. When
    /// `/Length` is missing or wrong, recover by scanning for the nearest
    /// `endstream` and trimming one trailing EOL from the data.
    fn parse_stream_body(&mut self, dict: Dict, resolver: &dyn Resolve) -> Result<Object> {
        let data = self.lexer.data();
        let mut start = self.lexer.pos();
        // The keyword should be followed by CRLF or LF; tolerate a lone CR.
        if data.get(start) == Some(&b'\r') {
            start += 1;
        }
        if data.get(start) == Some(&b'\n') {
            start += 1;
        }
        let declared = match dict.get("Length") {
            Some(Object::Int(n)) => Some(*n),
            Some(Object::Ref(r)) => resolver.resolve_ref(*r).and_then(|o| o.as_int()),
            _ => None,
        };
        if let Some(len) = declared.filter(|&l| l >= 0) {
            if let Some(end) = start.checked_add(len as usize).filter(|&e| e <= data.len()) {
                let mut probe = Lexer::at(data, end);
                if matches!(probe.next_token(),
                            Ok(Token::Keyword(ref k)) if k.as_slice() == b"endstream")
                {
                    self.lexer.seek(probe.pos());
                    return Ok(Object::Stream(Stream {
                        dict,
                        data: data[start..end].to_vec(),
                    }));
                }
            }
        }
        match memchr::memmem::find(&data[start..], b"endstream") {
            Some(idx) => {
                let mut end = start + idx;
                if end > start && data[end - 1] == b'\n' {
                    end -= 1;
                }
                if end > start && data[end - 1] == b'\r' {
                    end -= 1;
                }
                self.lexer.seek(start + idx + b"endstream".len());
                Ok(Object::Stream(Stream {
                    dict,
                    data: data[start..end].to_vec(),
                }))
            }
            None => {
                // Lenient: an unterminated stream takes the rest of the input.
                self.lexer.seek(data.len());
                Ok(Object::Stream(Stream {
                    dict,
                    data: data[start..].to_vec(),
                }))
            }
        }
    }

    /// Expects `N G obj ... endobj` at the current position and returns the
    /// reference plus the contained object. Lenient: a missing `endobj` is
    /// accepted at the next `obj` or end of input.
    pub fn parse_indirect(&mut self, resolver: &dyn Resolve) -> Result<(ObjRef, Object)> {
        let num = match self.lexer.next_token()? {
            Token::Int(n) if (0..=i64::from(u32::MAX)).contains(&n) => n as u32,
            _ => return Err(self.syntax("expected object number")),
        };
        let gen = match self.lexer.next_token()? {
            Token::Int(g) if (0..=i64::from(u16::MAX)).contains(&g) => g as u16,
            _ => return Err(self.syntax("expected generation number")),
        };
        match self.lexer.next_token()? {
            Token::Keyword(ref k) if k.as_slice() == b"obj" => {}
            _ => return Err(self.syntax("expected `obj`")),
        }
        // Lenient: an empty body (`N G obj endobj`) yields null.
        let object = match self.lexer.peek_token() {
            Ok(Token::Keyword(ref k)) if k.as_slice() == b"endobj" => Object::Null,
            _ => self.parse_object(resolver)?,
        };
        // Lenient: consume `endobj` when present; a missing `endobj` is
        // accepted as-is (at the next `N G obj` header or end of input).
        let save = self.lexer.pos();
        match self.lexer.next_token() {
            Ok(Token::Keyword(ref k)) if k.as_slice() == b"endobj" => {}
            _ => self.lexer.seek(save),
        }
        Ok((ObjRef { num, gen }, object))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::Name;

    fn parse(data: &[u8]) -> Object {
        Parser::new(data).parse_object(&NoResolve).unwrap()
    }

    #[test]
    fn atom_null_and_bools() {
        assert_eq!(parse(b"null"), Object::Null);
        assert_eq!(parse(b"true"), Object::Bool(true));
        assert_eq!(parse(b"false"), Object::Bool(false));
    }

    #[test]
    fn atom_numbers() {
        assert_eq!(parse(b"42"), Object::Int(42));
        assert_eq!(parse(b"-17"), Object::Int(-17));
        assert_eq!(parse(b"+5"), Object::Int(5));
        assert_eq!(parse(b"3.5"), Object::Real(3.5));
        assert_eq!(parse(b"-.25"), Object::Real(-0.25));
    }

    #[test]
    fn atom_strings() {
        assert_eq!(parse(b"(hello)"), Object::String(b"hello".to_vec()));
        assert_eq!(parse(b"<48690A>"), Object::String(b"Hi\n".to_vec()));
    }

    #[test]
    fn atom_name() {
        assert_eq!(parse(b"/Type"), Object::Name(Name("Type".into())));
    }

    #[test]
    fn nested_arrays_and_dicts() {
        let obj = parse(b"<< /A [1 2 [3]] /B << /C /D >> >>");
        let dict = obj.as_dict().unwrap();
        let a = dict.get_array("A").unwrap();
        assert_eq!(a[0], Object::Int(1));
        assert_eq!(a[1], Object::Int(2));
        assert_eq!(a[2], Object::Array(vec![Object::Int(3)]));
        let b = dict.get_dict("B").unwrap();
        assert_eq!(b.get_name("C"), Some(&Name("D".into())));
    }

    #[test]
    fn reference_from_lookahead() {
        assert_eq!(parse(b"12 0 R"), Object::Ref(ObjRef { num: 12, gen: 0 }));
        assert_eq!(
            parse(b"[12 0 R 7 3 R]"),
            Object::Array(vec![
                Object::Ref(ObjRef { num: 12, gen: 0 }),
                Object::Ref(ObjRef { num: 7, gen: 3 }),
            ])
        );
    }

    #[test]
    fn two_ints_without_r_stay_ints() {
        let mut p = Parser::new(b"12 0");
        assert_eq!(p.parse_object(&NoResolve).unwrap(), Object::Int(12));
        assert_eq!(p.parse_object(&NoResolve).unwrap(), Object::Int(0));
        assert_eq!(
            parse(b"[12 0]"),
            Object::Array(vec![Object::Int(12), Object::Int(0)])
        );
        // `RG` is a different keyword, not `R`: no reference.
        let mut p = Parser::new(b"12 0 RG");
        assert_eq!(p.parse_object(&NoResolve).unwrap(), Object::Int(12));
        assert_eq!(p.parse_object(&NoResolve).unwrap(), Object::Int(0));
    }

    #[test]
    fn indirect_round_trip() {
        let mut p = Parser::new(b"1 0 obj << /Type /Test /N 3 >> endobj");
        let (r, obj) = p.parse_indirect(&NoResolve).unwrap();
        assert_eq!(r, ObjRef { num: 1, gen: 0 });
        let dict = obj.as_dict().unwrap();
        assert_eq!(dict.get_name("Type"), Some(&Name("Test".into())));
        assert_eq!(dict.get_int("N"), Some(3));
        assert_eq!(p.lexer.next_token().unwrap(), Token::Eof);
    }

    #[test]
    fn stream_with_direct_length() {
        let mut p = Parser::new(b"<< /Length 5 >>\nstream\nhello\nendstream");
        let obj = p.parse_object(&NoResolve).unwrap();
        let s = obj.as_stream().unwrap();
        assert_eq!(s.data, b"hello");
        assert_eq!(p.lexer.next_token().unwrap(), Token::Eof);
    }

    /// Resolves exactly one reference, for indirect `/Length` tests.
    struct OneResolve(ObjRef, Object);

    impl Resolve for OneResolve {
        fn resolve_ref(&self, r: ObjRef) -> Option<Object> {
            (r == self.0).then(|| self.1.clone())
        }
    }

    #[test]
    fn stream_with_indirect_length() {
        let resolver = OneResolve(ObjRef { num: 9, gen: 0 }, Object::Int(7));
        let mut p = Parser::new(b"<< /Length 9 0 R >>stream\r\nhello!!\r\nendstream");
        let obj = p.parse_object(&resolver).unwrap();
        assert_eq!(obj.as_stream().unwrap().data, b"hello!!");
    }

    #[test]
    fn stream_with_wrong_length_recovers() {
        let mut p = Parser::new(b"<< /Length 3 >>stream\nhello world\nendstream endobj");
        let obj = p.parse_object(&NoResolve).unwrap();
        assert_eq!(obj.as_stream().unwrap().data, b"hello world");
        // Recovery leaves the cursor right after `endstream`.
        assert_eq!(
            p.lexer.next_token().unwrap(),
            Token::Keyword(b"endobj".to_vec())
        );
    }

    #[test]
    fn stream_with_overlong_length_recovers() {
        let mut p = Parser::new(b"<< /Length 999 >>stream\nxy\r\nendstream");
        let obj = p.parse_object(&NoResolve).unwrap();
        assert_eq!(obj.as_stream().unwrap().data, b"xy");
    }

    #[test]
    fn stream_without_length_recovers() {
        let mut p = Parser::new(b"<< /Type /XObject >>stream\nabc\nendstream");
        let obj = p.parse_object(&NoResolve).unwrap();
        let s = obj.as_stream().unwrap();
        assert_eq!(s.data, b"abc");
        assert_eq!(s.dict.get_name("Type"), Some(&Name("XObject".into())));
    }

    #[test]
    fn stream_with_unresolvable_length_recovers() {
        // The resolver knows nothing, so the indirect length falls through
        // to the `endstream` scan.
        let mut p = Parser::new(b"<< /Length 8 0 R >>stream\ndata\nendstream");
        let obj = p.parse_object(&NoResolve).unwrap();
        assert_eq!(obj.as_stream().unwrap().data, b"data");
    }

    #[test]
    fn missing_endobj_accepted_at_next_obj() {
        let mut p = Parser::new(b"1 0 obj 42 2 0 obj (next) endobj");
        let (r1, o1) = p.parse_indirect(&NoResolve).unwrap();
        assert_eq!(r1, ObjRef { num: 1, gen: 0 });
        assert_eq!(o1, Object::Int(42));
        let (r2, o2) = p.parse_indirect(&NoResolve).unwrap();
        assert_eq!(r2, ObjRef { num: 2, gen: 0 });
        assert_eq!(o2, Object::String(b"next".to_vec()));
    }

    #[test]
    fn missing_endobj_accepted_at_eof() {
        let mut p = Parser::new(b"5 0 obj << /A 1 >>");
        let (r, obj) = p.parse_indirect(&NoResolve).unwrap();
        assert_eq!(r, ObjRef { num: 5, gen: 0 });
        assert_eq!(obj.as_dict().unwrap().get_int("A"), Some(1));
    }

    #[test]
    fn empty_indirect_body_is_null() {
        let mut p = Parser::new(b"3 1 obj endobj");
        let (r, obj) = p.parse_indirect(&NoResolve).unwrap();
        assert_eq!(r, ObjRef { num: 3, gen: 1 });
        assert_eq!(obj, Object::Null);
        assert_eq!(p.lexer.next_token().unwrap(), Token::Eof);
    }

    #[test]
    fn indirect_stream_object() {
        let data = b"4 0 obj << /Length 3 >> stream\nxyz\nendstream endobj";
        let mut p = Parser::new(data);
        let (r, obj) = p.parse_indirect(&NoResolve).unwrap();
        assert_eq!(r, ObjRef { num: 4, gen: 0 });
        assert_eq!(obj.as_stream().unwrap().data, b"xyz");
        assert_eq!(p.lexer.next_token().unwrap(), Token::Eof);
    }

    #[test]
    fn dict_value_reference() {
        let obj = parse(b"<< /Parent 6 0 R /Count 2 >>");
        let dict = obj.as_dict().unwrap();
        assert_eq!(dict.get_ref("Parent"), Some(ObjRef { num: 6, gen: 0 }));
        assert_eq!(dict.get_int("Count"), Some(2));
    }

    /// Runs `f` on a deliberately small stack so that unbounded recursion
    /// would abort the test binary instead of silently passing on a large
    /// main-thread stack.
    fn on_small_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(f)
            .expect("spawn test thread")
            .join()
            .expect("parser must not overflow the stack")
    }

    #[test]
    fn deeply_nested_array_is_rejected_not_stack_overflow() {
        let mut data = vec![b'['; 200_000];
        data.extend(std::iter::repeat_n(b']', 200_000));
        let result = on_small_stack(move || Parser::new(&data).parse_object(&NoResolve));
        assert!(matches!(result, Err(Error::Syntax { .. })));
    }

    #[test]
    fn deeply_nested_dict_is_rejected_not_stack_overflow() {
        let mut data = Vec::new();
        for _ in 0..100_000 {
            data.extend_from_slice(b"<</K");
        }
        let result = on_small_stack(move || Parser::new(&data).parse_object(&NoResolve));
        assert!(matches!(result, Err(Error::Syntax { .. })));
    }

    #[test]
    fn nesting_within_the_limit_still_parses() {
        let mut data = vec![b'['; 100];
        data.extend_from_slice(b" 7 ");
        data.extend(std::iter::repeat_n(b']', 100));
        let mut obj = parse(&data);
        for _ in 0..100 {
            let items = obj.as_array().expect("nested array").to_vec();
            assert_eq!(items.len(), 1);
            obj = items.into_iter().next().unwrap();
        }
        assert_eq!(obj, Object::Int(7));
    }
}
