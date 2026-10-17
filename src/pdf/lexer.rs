//! Tolerant tokenizer for PDF content streams.
//!
//! Never fails: malformed input produces best-effort tokens and always makes
//! progress, so callers can simply loop until [`Tok::End`].

/// A content-stream operand.
#[derive(Debug, Clone, PartialEq)]
pub enum Operand {
    Num(f64),
    Name(Vec<u8>),
    Str(Vec<u8>),
    Array(Vec<Operand>),
    Dict(Vec<(Vec<u8>, Operand)>),
    Bool(bool),
    Null,
}

impl Operand {
    pub fn as_num(&self) -> Option<f64> {
        match self {
            Operand::Num(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_name(&self) -> Option<&[u8]> {
        match self {
            Operand::Name(n) => Some(n),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Tok<'a> {
    Operand(Operand),
    Op(&'a [u8]),
    /// A complete inline image (`BI ... ID <data> EI`), already skipped.
    InlineImage,
    End,
}

/// Maximum nesting depth of arrays / dictionaries.
const MAX_DEPTH: usize = 32;

pub struct Lexer<'a> {
    data: &'a [u8],
    pos: usize,
}

fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'\x0c' | b'\0')
}

fn is_delim(b: u8) -> bool {
    matches!(b, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%')
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Parse a PDF number token leniently (e.g. "-.5", "3.", "+2"). Returns
/// `None` if the token contains no digits at all.
pub fn parse_number(tok: &[u8]) -> Option<f64> {
    let mut i = 0;
    let mut neg = false;
    // Tolerate repeated signs ("--5" is produced by some buggy writers).
    while i < tok.len() && (tok[i] == b'-' || tok[i] == b'+') {
        if tok[i] == b'-' {
            neg = !neg;
        }
        i += 1;
    }
    let body = &tok[i..];
    let digits = body.iter().filter(|b| b.is_ascii_digit()).count();
    let dots = body.iter().filter(|b| **b == b'.').count();
    if digits == 0 || dots > 1 || digits + dots != body.len() {
        return None;
    }
    let v: f64 = std::str::from_utf8(body).ok()?.parse().ok()?;
    let v = if neg { -v } else { v };
    if v.is_finite() { Some(v) } else { None }
}

impl<'a> Lexer<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn peek(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    fn skip_ws_and_comments(&mut self) {
        while let Some(b) = self.peek() {
            if is_ws(b) {
                self.pos += 1;
            } else if b == b'%' {
                while let Some(b) = self.peek() {
                    if b == b'\n' || b == b'\r' {
                        break;
                    }
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    fn regular_token(&mut self) -> &'a [u8] {
        let start = self.pos;
        while let Some(b) = self.peek() {
            if is_ws(b) || is_delim(b) {
                break;
            }
            self.pos += 1;
        }
        &self.data[start..self.pos]
    }

    fn name(&mut self) -> Vec<u8> {
        // Leading '/' already consumed.
        let raw = self.regular_token();
        let mut out = Vec::with_capacity(raw.len());
        let mut i = 0;
        while i < raw.len() {
            if raw[i] == b'#'
                && i + 2 < raw.len()
                && let (Some(h), Some(l)) = (hex_val(raw[i + 1]), hex_val(raw[i + 2]))
            {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
            out.push(raw[i]);
            i += 1;
        }
        out
    }

    fn literal_string(&mut self) -> Vec<u8> {
        // Leading '(' already consumed.
        let mut out = Vec::new();
        let mut depth = 1usize;
        while let Some(b) = self.peek() {
            self.pos += 1;
            match b {
                b'(' => {
                    depth += 1;
                    out.push(b);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return out;
                    }
                    out.push(b);
                }
                b'\\' => {
                    let Some(e) = self.peek() else { break };
                    self.pos += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'\r' => {
                            if self.peek() == Some(b'\n') {
                                self.pos += 1;
                            }
                        }
                        b'\n' => {}
                        b'0'..=b'7' => {
                            let mut v: u32 = (e - b'0') as u32;
                            for _ in 0..2 {
                                match self.peek() {
                                    Some(d @ b'0'..=b'7') => {
                                        v = v * 8 + (d - b'0') as u32;
                                        self.pos += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push((v & 0xFF) as u8);
                        }
                        other => out.push(other),
                    }
                }
                _ => out.push(b),
            }
        }
        out
    }

    fn hex_string(&mut self) -> Vec<u8> {
        // Leading '<' already consumed.
        let mut out = Vec::new();
        let mut hi: Option<u8> = None;
        while let Some(b) = self.peek() {
            self.pos += 1;
            if b == b'>' {
                break;
            }
            if let Some(v) = hex_val(b) {
                match hi.take() {
                    Some(h) => out.push(h * 16 + v),
                    None => hi = Some(v),
                }
            }
        }
        if let Some(h) = hi {
            out.push(h * 16);
        }
        out
    }

    /// Parse one operand-ish object for use inside arrays and dictionaries.
    /// Returns `None` at a closing delimiter (not consumed) or end of data.
    fn object(&mut self, depth: usize, close: u8) -> Option<Operand> {
        loop {
            self.skip_ws_and_comments();
            let b = self.peek()?;
            if b == close {
                return None;
            }
            match self.atom(depth) {
                Some(Tok::Operand(o)) => return Some(o),
                Some(_) => continue, // stray operator inside an array: ignore
                None => return None,
            }
        }
    }

    fn array(&mut self, depth: usize) -> Operand {
        // '[' already consumed.
        if depth >= MAX_DEPTH {
            return Operand::Null;
        }
        let mut items = Vec::new();
        while let Some(o) = self.object(depth + 1, b']') {
            items.push(o);
        }
        if self.peek() == Some(b']') {
            self.pos += 1;
        }
        Operand::Array(items)
    }

    fn dict(&mut self, depth: usize) -> Operand {
        // '<<' already consumed.
        if depth >= MAX_DEPTH {
            return Operand::Null;
        }
        let mut items = Vec::new();
        let mut key: Option<Vec<u8>> = None;
        loop {
            self.skip_ws_and_comments();
            match self.peek() {
                None => break,
                Some(b'>') => {
                    self.pos += 1;
                    if self.peek() == Some(b'>') {
                        self.pos += 1;
                    }
                    break;
                }
                _ => {}
            }
            let Some(o) = self.object(depth + 1, b'>') else { continue };
            match key.take() {
                None => {
                    if let Operand::Name(n) = o {
                        key = Some(n);
                    }
                }
                Some(k) => items.push((k, o)),
            }
        }
        Operand::Dict(items)
    }

    /// Lex one token starting at a non-whitespace byte. Returns `None` only
    /// at end of data. Always consumes at least one byte otherwise.
    fn atom(&mut self, depth: usize) -> Option<Tok<'a>> {
        let b = self.peek()?;
        match b {
            b'/' => {
                self.pos += 1;
                Some(Tok::Operand(Operand::Name(self.name())))
            }
            b'(' => {
                self.pos += 1;
                Some(Tok::Operand(Operand::Str(self.literal_string())))
            }
            b'<' => {
                self.pos += 1;
                if self.peek() == Some(b'<') {
                    self.pos += 1;
                    Some(Tok::Operand(self.dict(depth)))
                } else {
                    Some(Tok::Operand(Operand::Str(self.hex_string())))
                }
            }
            b'[' => {
                self.pos += 1;
                Some(Tok::Operand(self.array(depth)))
            }
            b')' | b'>' | b']' | b'{' | b'}' => {
                // Stray delimiter: skip it, report as an (ignored) operator.
                self.pos += 1;
                Some(Tok::Op(&self.data[self.pos - 1..self.pos]))
            }
            _ => {
                let tok = self.regular_token();
                if tok.is_empty() {
                    // Should not happen (delimiters handled above) but guarantee progress.
                    self.pos += 1;
                    return Some(Tok::Op(&self.data[self.pos - 1..self.pos]));
                }
                let first = tok[0];
                if (first.is_ascii_digit() || matches!(first, b'+' | b'-' | b'.'))
                    && let Some(n) = parse_number(tok)
                {
                    return Some(Tok::Operand(Operand::Num(n)));
                }
                Some(match tok {
                    b"true" => Tok::Operand(Operand::Bool(true)),
                    b"false" => Tok::Operand(Operand::Bool(false)),
                    b"null" => Tok::Operand(Operand::Null),
                    _ => Tok::Op(tok),
                })
            }
        }
    }

    /// Skip an inline image body after the `BI` operator.
    fn skip_inline_image(&mut self) {
        // Dictionary entries until the `ID` operator.
        loop {
            self.skip_ws_and_comments();
            match self.atom(0) {
                None => return,
                Some(Tok::Op(b"ID")) => break,
                Some(Tok::Op(b"EI")) => return,
                Some(_) => {}
            }
        }
        // Single white-space byte after ID, then binary data up to "EI"
        // delimited by white space on both sides (or end of data).
        if self.peek().is_some_and(is_ws) {
            self.pos += 1;
        }
        let d = self.data;
        let mut i = self.pos;
        while i + 1 < d.len() {
            if d[i] == b'E'
                && d[i + 1] == b'I'
                && (i == self.pos || is_ws(d[i - 1]))
                && (i + 2 >= d.len() || is_ws(d[i + 2]) || is_delim(d[i + 2]))
            {
                self.pos = i + 2;
                return;
            }
            i += 1;
        }
        self.pos = d.len();
    }

    pub fn next_tok(&mut self) -> Tok<'a> {
        self.skip_ws_and_comments();
        match self.atom(0) {
            None => Tok::End,
            Some(Tok::Op(b"BI")) => {
                self.skip_inline_image();
                Tok::InlineImage
            }
            Some(t) => t,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(s: &[u8]) -> Vec<Tok<'_>> {
        let mut l = Lexer::new(s);
        let mut v = Vec::new();
        loop {
            match l.next_tok() {
                Tok::End => break,
                t => v.push(t),
            }
        }
        v
    }

    #[test]
    fn numbers() {
        assert_eq!(parse_number(b"12"), Some(12.0));
        assert_eq!(parse_number(b"-.5"), Some(-0.5));
        assert_eq!(parse_number(b"3."), Some(3.0));
        assert_eq!(parse_number(b"+4.25"), Some(4.25));
        assert_eq!(parse_number(b"--2"), Some(2.0));
        assert_eq!(parse_number(b"-"), None);
        assert_eq!(parse_number(b"1.2.3"), None);
    }

    #[test]
    fn basic_ops() {
        let t = toks(b"1 0 0 1 10.5 -2 cm /GS0 gs q Q % comment\n0 0 m");
        assert_eq!(t[4], Tok::Operand(Operand::Num(10.5)));
        assert_eq!(t[6], Tok::Op(b"cm"));
        assert_eq!(t[7], Tok::Operand(Operand::Name(b"GS0".to_vec())));
        assert_eq!(t[8], Tok::Op(b"gs"));
        assert_eq!(t.last(), Some(&Tok::Op(b"m")));
        assert_eq!(t.len(), 14);
    }

    #[test]
    fn strings_arrays_dicts() {
        let t = toks(b"(a\\(b\\)c (nested)) <48 65 6c6> [1 [2 3] /N] << /A 1 /B [true] >> \"");
        assert_eq!(t[0], Tok::Operand(Operand::Str(b"a(b)c (nested)".to_vec())));
        assert_eq!(t[1], Tok::Operand(Operand::Str(vec![0x48, 0x65, 0x6c, 0x60])));
        assert_eq!(
            t[2],
            Tok::Operand(Operand::Array(vec![
                Operand::Num(1.0),
                Operand::Array(vec![Operand::Num(2.0), Operand::Num(3.0)]),
                Operand::Name(b"N".to_vec())
            ]))
        );
        assert_eq!(
            t[3],
            Tok::Operand(Operand::Dict(vec![
                (b"A".to_vec(), Operand::Num(1.0)),
                (b"B".to_vec(), Operand::Array(vec![Operand::Bool(true)]))
            ]))
        );
        assert_eq!(t[4], Tok::Op(b"\""));
    }

    #[test]
    fn name_escapes() {
        let t = toks(b"/Cut#20Contour /A#");
        assert_eq!(t[0], Tok::Operand(Operand::Name(b"Cut Contour".to_vec())));
        assert_eq!(t[1], Tok::Operand(Operand::Name(b"A#".to_vec())));
    }

    #[test]
    fn inline_image_skipped() {
        let t = toks(b"q BI /W 2 /H 1 /BPC 8 /CS /G ID \x00EI\xff EI Q");
        assert_eq!(t, vec![Tok::Op(b"q"), Tok::InlineImage, Tok::Op(b"Q")]);
        // Unterminated inline image consumes the rest without hanging.
        let t = toks(b"BI /W 1 ID abc");
        assert_eq!(t, vec![Tok::InlineImage]);
    }

    #[test]
    fn garbage_terminates() {
        let mut data = Vec::new();
        for i in 0..5000u32 {
            data.push((i.wrapping_mul(2654435761) >> 24) as u8);
        }
        data.extend_from_slice(b"[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[(((<<<</");
        let _ = toks(&data);
        let deep = vec![b'['; 100_000];
        let _ = toks(&deep);
        let _ = toks(b"<< /A << /B");
        let _ = toks(b"(unterminated \\");
    }
}
