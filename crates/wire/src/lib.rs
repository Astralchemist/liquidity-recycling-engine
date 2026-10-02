//! Allocation-free JSON pull scanner for native venue messages, plus exact decimal parsing.
//! It reads borrowed slices of the frame; nothing is copied or allocated. String escapes are
//! rejected (exchange fields used here never contain them), so a borrowed slice is the value.
use fixed_point::{ArithmeticError, PriceTicks, parse_ticks};
pub mod feed;
pub mod window;
pub const MAX_DEPTH: usize = 32;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireError {
    Syntax(usize),
    UnsupportedEscape(usize),
    TooDeep,
    Type(usize),
    Decimal(ArithmeticError),
    TrailingData(usize),
}
impl From<ArithmeticError> for WireError {
    fn from(e: ArithmeticError) -> Self {
        Self::Decimal(e)
    }
}
/// Cursor over one JSON document. Containers are entered explicitly; a per-depth flag tracks
/// whether the next member needs a comma.
pub struct Scanner<'a> {
    b: &'a [u8],
    i: usize,
    started: [bool; MAX_DEPTH],
    depth: usize,
}
impl<'a> Scanner<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        Self {
            b,
            i: 0,
            started: [false; MAX_DEPTH],
            depth: 0,
        }
    }
    fn ws(&mut self) {
        while self
            .b
            .get(self.i)
            .is_some_and(|c| matches!(c, b' ' | b'\n' | b'\r' | b'\t'))
        {
            self.i += 1;
        }
    }
    fn peek(&mut self) -> Option<u8> {
        self.ws();
        self.b.get(self.i).copied()
    }
    fn expect(&mut self, c: u8) -> Result<(), WireError> {
        if self.peek() == Some(c) {
            self.i += 1;
            Ok(())
        } else {
            Err(WireError::Syntax(self.i))
        }
    }
    fn open(&mut self, c: u8) -> Result<(), WireError> {
        self.expect(c)?;
        if self.depth == MAX_DEPTH {
            return Err(WireError::TooDeep);
        }
        self.started[self.depth] = false;
        self.depth += 1;
        Ok(())
    }
    /// Consumes the separator before the next member, or the closing bracket.
    fn member(&mut self, close: u8) -> Result<bool, WireError> {
        let d = self.depth.checked_sub(1).ok_or(WireError::Syntax(self.i))?;
        match self.peek() {
            Some(c) if c == close => {
                self.i += 1;
                self.depth -= 1;
                Ok(false)
            }
            Some(b',') if self.started[d] => {
                self.i += 1;
                Ok(true)
            }
            Some(_) if !self.started[d] => {
                self.started[d] = true;
                Ok(true)
            }
            _ => Err(WireError::Syntax(self.i)),
        }
    }
    /// True when the next value is an array (used where a key's type varies by message).
    pub fn next_is_array(&mut self) -> bool {
        self.peek() == Some(b'[')
    }
    pub fn begin_object(&mut self) -> Result<(), WireError> {
        self.open(b'{')
    }
    pub fn begin_array(&mut self) -> Result<(), WireError> {
        self.open(b'[')
    }
    /// Next key of the current object, or `None` after consuming `}`.
    pub fn key(&mut self) -> Result<Option<&'a [u8]>, WireError> {
        if !self.member(b'}')? {
            return Ok(None);
        }
        let k = self.string()?;
        self.expect(b':')?;
        Ok(Some(k))
    }
    /// True when another array element follows; false after consuming `]`.
    pub fn item(&mut self) -> Result<bool, WireError> {
        self.member(b']')
    }
    pub fn string(&mut self) -> Result<&'a [u8], WireError> {
        self.expect(b'"')?;
        let start = self.i;
        loop {
            match self.b.get(self.i) {
                Some(b'"') => {
                    self.i += 1;
                    return Ok(&self.b[start..self.i - 1]);
                }
                Some(b'\\') => return Err(WireError::UnsupportedEscape(self.i)),
                Some(c) if *c < 0x20 => return Err(WireError::Syntax(self.i)),
                Some(_) => self.i += 1,
                None => return Err(WireError::Syntax(self.i)),
            }
        }
    }
    /// Raw number token, validated by the caller's conversion.
    pub fn number(&mut self) -> Result<&'a [u8], WireError> {
        self.ws();
        let start = self.i;
        while self
            .b
            .get(self.i)
            .is_some_and(|c| c.is_ascii_digit() || matches!(c, b'-' | b'+' | b'.' | b'e' | b'E'))
        {
            self.i += 1;
        }
        if self.i == start {
            return Err(WireError::Type(start));
        }
        Ok(&self.b[start..self.i])
    }
    pub fn boolean(&mut self) -> Result<bool, WireError> {
        self.ws();
        if self.b[self.i..].starts_with(b"true") {
            self.i += 4;
            Ok(true)
        } else if self.b[self.i..].starts_with(b"false") {
            self.i += 5;
            Ok(false)
        } else {
            Err(WireError::Type(self.i))
        }
    }
    /// Unsigned integer from a JSON number or a numeric string.
    pub fn u64(&mut self) -> Result<u64, WireError> {
        let at = self.i;
        let raw = if self.peek() == Some(b'"') {
            self.string()?
        } else {
            self.number()?
        };
        parse_u64(raw).ok_or(WireError::Type(at))
    }
    /// Signed integer from a JSON number or a numeric string.
    pub fn i64(&mut self) -> Result<i64, WireError> {
        let at = self.i;
        let raw = if self.peek() == Some(b'"') {
            self.string()?
        } else {
            self.number()?
        };
        let (negative, digits) = match raw.split_first() {
            Some((b'-', rest)) => (true, rest),
            _ => (false, raw),
        };
        let magnitude = i64::try_from(parse_u64(digits).ok_or(WireError::Type(at))?)
            .map_err(|_| WireError::Type(at))?;
        Ok(if negative { -magnitude } else { magnitude })
    }
    /// Skips any value, including nested containers.
    pub fn skip(&mut self) -> Result<(), WireError> {
        match self.peek() {
            Some(b'{') => {
                self.begin_object()?;
                while self.key()?.is_some() {
                    self.skip()?;
                }
                Ok(())
            }
            Some(b'[') => {
                self.begin_array()?;
                while self.item()? {
                    self.skip()?;
                }
                Ok(())
            }
            Some(b'"') => self.string().map(|_| ()),
            Some(b't' | b'f') => self.boolean().map(|_| ()),
            Some(b'n') if self.b[self.i..].starts_with(b"null") => {
                self.i += 4;
                Ok(())
            }
            _ => self.number().map(|_| ()),
        }
    }
    /// Only whitespace may follow the top-level value.
    pub fn finish(&mut self) -> Result<(), WireError> {
        if self.depth != 0 || self.peek().is_some() {
            return Err(WireError::TrailingData(self.i));
        }
        Ok(())
    }
}
fn parse_u64(raw: &[u8]) -> Option<u64> {
    if raw.is_empty() || raw.len() > 20 || !raw.iter().all(u8::is_ascii_digit) {
        return None;
    }
    raw.iter().try_fold(0_u64, |acc, d| {
        acc.checked_mul(10)?.checked_add(u64::from(d - b'0'))
    })
}
/// Exact decimal to integer units: `raw × 10^decimals / atoms` must be an integer. Trailing
/// fractional zeros beyond `decimals` are exact and accepted; any other excess is rejected.
pub fn decimal(raw: &[u8], decimals: u32, atoms: i64) -> Result<i64, WireError> {
    let text = std::str::from_utf8(raw)
        .map_err(|_| WireError::Decimal(ArithmeticError::InvalidDecimal))?;
    let trimmed = match text.split_once('.') {
        Some((_, fraction)) if fraction.len() > decimals as usize => {
            let keep = text.len() - fraction.len() + decimals as usize;
            if !text.as_bytes()[keep..].iter().all(|&c| c == b'0') {
                return Err(WireError::Decimal(ArithmeticError::Inexact));
            }
            text[..keep].trim_end_matches('.')
        }
        _ => text,
    };
    let PriceTicks(units) = parse_ticks(trimmed, decimals, atoms)?;
    Ok(units)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn walks_nested_documents_without_copying() {
        let doc = br#" {"stream":"x","data":{"b":[["1.50","2"],["1.40","0.000"]],"u":42,"m":true,"z":null,"n":-7,"s":"9"}} "#;
        let mut s = Scanner::new(doc);
        s.begin_object().unwrap();
        assert_eq!(s.key().unwrap(), Some(&b"stream"[..]));
        assert_eq!(s.string().unwrap(), b"x");
        assert_eq!(s.key().unwrap(), Some(&b"data"[..]));
        s.begin_object().unwrap();
        let mut levels = [(0, 0); 4];
        let mut n = 0;
        let (mut u, mut m, mut neg, mut text) = (0, false, 0, 0);
        while let Some(k) = s.key().unwrap() {
            match k {
                b"b" => {
                    s.begin_array().unwrap();
                    while s.item().unwrap() {
                        s.begin_array().unwrap();
                        assert!(s.item().unwrap());
                        let p = decimal(s.string().unwrap(), 2, 1).unwrap();
                        assert!(s.item().unwrap());
                        let q = decimal(s.string().unwrap(), 3, 1).unwrap();
                        assert!(!s.item().unwrap());
                        levels[n] = (p, q);
                        n += 1;
                    }
                }
                b"u" => u = s.u64().unwrap(),
                b"m" => m = s.boolean().unwrap(),
                b"n" => neg = s.i64().unwrap(),
                b"s" => text = s.u64().unwrap(),
                _ => s.skip().unwrap(),
            }
        }
        assert!(s.key().unwrap().is_none());
        s.finish().unwrap();
        assert_eq!(&levels[..n], &[(150, 2000), (140, 0)]);
        assert_eq!((u, m, neg, text), (42, true, -7, 9));
    }
    #[test]
    fn rejects_malformed_input_and_inexact_decimals() {
        for bad in [
            &br#"{"a":1,}"#[..],
            br#"{"a" 1}"#,
            br#"{"a":"x\"y"}"#,
            br#"[1 2]"#,
            br#"{"a":1} x"#,
            br#"{"a":"#,
        ] {
            let mut s = Scanner::new(bad);
            let result = (|| {
                s.skip()?;
                s.finish()
            })();
            assert!(result.is_err(), "{}", String::from_utf8_lossy(bad));
        }
        let deep = [b'['; MAX_DEPTH + 1];
        assert_eq!(Scanner::new(&deep).skip(), Err(WireError::TooDeep));
        assert_eq!(decimal(b"60123.10", 1, 1), Ok(601_231));
        assert_eq!(decimal(b"60123.1", 1, 1), Ok(601_231));
        assert_eq!(decimal(b"60123", 1, 1), Ok(601_230));
        assert_eq!(
            decimal(b"60123.15", 1, 1),
            Err(WireError::Decimal(ArithmeticError::Inexact))
        );
        assert!(decimal(b"1e5", 1, 1).is_err());
        assert!(decimal(b"", 1, 1).is_err());
        assert_eq!(decimal(b"0.0100", 2, 1), Ok(1));
        assert_eq!(
            Scanner::new(b"18446744073709551616").u64(),
            Err(WireError::Type(0))
        );
    }
}
