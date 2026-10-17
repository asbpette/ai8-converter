//! Low-level DXF group-code reader (ASCII and binary DXF).
//!
//! A DXF file is a flat sequence of *group code / value* pairs. ASCII files
//! put the code and the value on two lines; binary files (`AutoCAD Binary
//! DXF` sentinel) store the code as 1 byte (R12, 255 = escape to a 16-bit
//! code) or 2 bytes (R13+) followed by a value whose encoding depends on the
//! code range.

use anyhow::{Result, bail};
use std::borrow::Cow;

/// Sentinel at the start of a binary DXF file.
pub const BINARY_SENTINEL: &[u8] = b"AutoCAD Binary DXF\r\n\x1a\0";

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    F(f64),
    I(i64),
    Bin(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pair {
    pub code: i32,
    pub value: Value,
}

impl Pair {
    pub fn f(&self) -> Option<f64> {
        match &self.value {
            Value::F(v) => Some(*v),
            Value::I(v) => Some(*v as f64),
            Value::Str(s) => s.trim().parse::<f64>().ok(),
            Value::Bin(_) => None,
        }
        .filter(|v| v.is_finite())
    }

    pub fn i(&self) -> Option<i64> {
        match &self.value {
            Value::I(v) => Some(*v),
            Value::F(v) if v.is_finite() => Some(v.round() as i64),
            Value::Str(s) => {
                let t = s.trim();
                t.parse::<i64>().ok().or_else(|| {
                    t.parse::<f64>().ok().filter(|v| v.is_finite() && v.abs() < 9.0e18).map(|v| v.round() as i64)
                })
            }
            _ => None,
        }
    }

    pub fn s(&self) -> Cow<'_, str> {
        match &self.value {
            Value::Str(s) => Cow::Borrowed(s.as_str()),
            Value::F(v) => Cow::Owned(v.to_string()),
            Value::I(v) => Cow::Owned(v.to_string()),
            Value::Bin(_) => Cow::Borrowed(""),
        }
    }

    pub fn is(&self, code: i32, s: &str) -> bool {
        self.code == code && matches!(&self.value, Value::Str(v) if v.eq_ignore_ascii_case(s))
    }
}

/// Does `head` look like the start of a DXF file (ASCII or binary)?
pub fn looks_like_dxf(head: &[u8]) -> bool {
    if head.starts_with(BINARY_SENTINEL) {
        return true;
    }
    let head = head.strip_prefix(b"\xEF\xBB\xBF".as_slice()).unwrap_or(head);
    let text = String::from_utf8_lossy(&head[..head.len().min(4096)]);
    let mut lines = text.lines().map(str::trim);
    // Optional leading 999 comments.
    loop {
        let Some(code) = lines.next() else { return false };
        let Some(value) = lines.next() else { return false };
        match code {
            "999" => continue,
            "0" => return value.eq_ignore_ascii_case("SECTION"),
            _ => return false,
        }
    }
}

/// Read all group pairs of an ASCII or binary DXF file.
pub fn read_pairs(data: &[u8]) -> Result<Vec<Pair>> {
    if let Some(rest) = data.strip_prefix(BINARY_SENTINEL) {
        read_binary(rest)
    } else {
        read_ascii(data)
    }
}

fn decode_text(data: &[u8]) -> Cow<'_, str> {
    let data = data.strip_prefix(b"\xEF\xBB\xBF".as_slice()).unwrap_or(data);
    match std::str::from_utf8(data) {
        Ok(s) => Cow::Borrowed(s),
        // Pre-2007 DXF files use the drawing's ANSI code page; Latin-1 keeps
        // every byte (only names and text are affected, never geometry).
        Err(_) => Cow::Owned(data.iter().map(|&b| b as char).collect()),
    }
}

fn read_ascii(data: &[u8]) -> Result<Vec<Pair>> {
    let text = decode_text(data);
    let mut lines = text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).enumerate();
    let mut out = Vec::new();
    while let Some((lineno, code_line)) = lines.next() {
        let code_str = code_line.trim();
        if code_str.is_empty() {
            // Trailing blank lines after EOF are common.
            if lines.clone().all(|(_, l)| l.trim().is_empty()) {
                break;
            }
            bail!("malformed DXF: empty group code on line {}", lineno + 1);
        }
        let code: i32 = match code_str.parse() {
            Ok(c) => c,
            Err(_) => bail!(
                "malformed DXF: expected a numeric group code on line {}, found {:?}",
                lineno + 1,
                truncate(code_str, 40)
            ),
        };
        let Some((_, value)) = lines.next() else {
            bail!("malformed DXF: group code {code} on line {} has no value (file truncated?)", lineno + 1);
        };
        if code == 999 {
            continue;
        }
        out.push(Pair { code, value: Value::Str(value.trim().to_string()) });
        if code == 0 && value.trim() == "EOF" {
            break;
        }
    }
    Ok(out)
}

fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

#[derive(Clone, Copy)]
enum Kind {
    Str,
    F64,
    I16,
    I32,
    I64,
    Bool,
    Bin,
}

fn kind_of(code: i32) -> Option<Kind> {
    Some(match code {
        0..=9 => Kind::Str,
        10..=59 => Kind::F64,
        60..=79 => Kind::I16,
        90..=99 => Kind::I32,
        100..=102 | 105 => Kind::Str,
        110..=149 => Kind::F64,
        160..=169 => Kind::I64,
        170..=179 => Kind::I16,
        210..=239 => Kind::F64,
        270..=289 => Kind::I16,
        290..=299 => Kind::Bool,
        300..=309 => Kind::Str,
        310..=319 => Kind::Bin,
        320..=369 => Kind::Str,
        370..=389 => Kind::I16,
        390..=399 => Kind::Str,
        400..=409 => Kind::I16,
        410..=419 => Kind::Str,
        420..=429 => Kind::I32,
        430..=439 => Kind::Str,
        440..=459 => Kind::I32,
        460..=469 => Kind::F64,
        470..=481 => Kind::Str,
        999 => Kind::Str,
        1004 => Kind::Bin,
        1000..=1009 => Kind::Str,
        1010..=1059 => Kind::F64,
        1060..=1070 => Kind::I16,
        1071 => Kind::I32,
        _ => return None,
    })
}

struct Bin<'a> {
    d: &'a [u8],
    pos: usize,
}

impl Bin<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8]> {
        if self.pos + n > self.d.len() {
            bail!("malformed binary DXF: unexpected end of file at byte {}", self.pos + BINARY_SENTINEL.len());
        }
        let s = &self.d[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn i16(&mut self) -> Result<i16> {
        let b = self.take(2)?;
        Ok(i16::from_le_bytes([b[0], b[1]]))
    }
    fn string(&mut self) -> Result<String> {
        let rest = &self.d[self.pos..];
        let Some(end) = rest.iter().position(|&b| b == 0) else {
            bail!("malformed binary DXF: unterminated string at byte {}", self.pos + BINARY_SENTINEL.len());
        };
        let s = decode_text(&rest[..end]).into_owned();
        self.pos += end + 1;
        Ok(s)
    }
}

fn read_binary(data: &[u8]) -> Result<Vec<Pair>> {
    let mut r = Bin { d: data, pos: 0 };
    // R13+ files use 2-byte codes: the first pair is code 0 "SECTION", so
    // the second byte is 0 for 2-byte codes and 'S' for 1-byte codes.
    let two_byte = data.len() >= 2 && data[0] == 0 && data[1] == 0;
    let mut out = Vec::new();
    while r.pos < data.len() {
        let code = if two_byte {
            r.i16()? as i32
        } else {
            match r.u8()? {
                255 => r.i16()? as i32,
                c => c as i32,
            }
        };
        let Some(kind) = kind_of(code) else {
            bail!(
                "malformed binary DXF: unknown group code {code} at byte {}",
                r.pos + BINARY_SENTINEL.len()
            );
        };
        let value = match kind {
            Kind::Str => Value::Str(r.string()?),
            Kind::F64 => {
                let b = r.take(8)?;
                Value::F(f64::from_le_bytes(b.try_into().unwrap_or([0; 8])))
            }
            Kind::I16 => Value::I(r.i16()? as i64),
            Kind::I32 => {
                let b = r.take(4)?;
                Value::I(i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as i64)
            }
            Kind::I64 => {
                let b = r.take(8)?;
                Value::I(i64::from_le_bytes(b.try_into().unwrap_or([0; 8])))
            }
            Kind::Bool => {
                if two_byte {
                    Value::I(r.u8()? as i64)
                } else {
                    Value::I(r.i16()? as i64)
                }
            }
            Kind::Bin => {
                let n = r.u8()? as usize;
                Value::Bin(r.take(n)?.to_vec())
            }
        };
        let eof = code == 0 && matches!(&value, Value::Str(s) if s == "EOF");
        if code != 999 {
            out.push(Pair { code, value });
        }
        if eof {
            break;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects() {
        assert!(looks_like_dxf(b"  0\r\nSECTION\r\n  2\r\nHEADER\r\n"));
        assert!(looks_like_dxf(b"0\nSECTION\n"));
        assert!(looks_like_dxf(b"999\ncomment\n  0\nSECTION\n"));
        assert!(looks_like_dxf(b"AutoCAD Binary DXF\r\n\x1a\0\0\0SECTION\0"));
        assert!(!looks_like_dxf(b"%PDF-1.4"));
        assert!(!looks_like_dxf(b"0\nLINE\n"));
    }

    #[test]
    fn ascii_pairs() {
        let p = read_pairs(b"  0\r\nSECTION\r\n  2\r\nENTITIES\r\n 10\r\n1.5\r\n999\r\nhi\r\n  0\r\nEOF\r\n\r\n").unwrap();
        assert_eq!(p.len(), 4);
        assert_eq!(p[2].f(), Some(1.5));
        assert!(p[3].is(0, "EOF"));
        assert!(read_pairs(b"  0\nSECTION\n 2").is_err());
        assert!(read_pairs(b"abc\nSECTION\n").is_err());
    }

    #[test]
    fn binary_pairs() {
        let mut d = BINARY_SENTINEL.to_vec();
        // 2-byte codes
        d.extend([0, 0]);
        d.extend(b"SECTION\0");
        d.extend([10, 0]);
        d.extend(2.5f64.to_le_bytes());
        d.extend([70, 0]);
        d.extend(7i16.to_le_bytes());
        d.extend([0xC2, 0x01]);
        d.extend(37i32.to_le_bytes());
        let p = read_pairs(&d).unwrap();
        assert_eq!(p[0].s(), "SECTION");
        assert_eq!(p[1].f(), Some(2.5));
        assert_eq!(p[2].i(), Some(7));
        assert_eq!((p[3].code, p[3].i()), (450, Some(37)));
        // 1-byte codes (R12)
        let mut d = BINARY_SENTINEL.to_vec();
        d.push(0);
        d.extend(b"SECTION\0");
        d.push(255);
        d.extend(1071i16.to_le_bytes());
        d.extend(5i32.to_le_bytes());
        let p = read_pairs(&d).unwrap();
        assert_eq!((p[1].code, p[1].i()), (1071, Some(5)));
        // truncated
        d.pop();
        assert!(read_pairs(&d).is_err());
    }
}
