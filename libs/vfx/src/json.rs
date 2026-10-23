//! The JSON of the effect descriptions and sheets, read and written as the Python tools do it
//! (`json.load`, `json.dump(..., indent=1)`): keys kept in their order, integers kept apart from
//! floats, floats written as Python writes them (`repr`), non-ASCII text escaped. A file read here
//! and written again without a change is the same file.

use std::fmt::Write as _;

/// A JSON value. A number keeps whether it was written as an integer.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<Value>),
    /// The keys in their order.
    Map(Vec<(String, Value)>),
}

impl Value {
    /// The value under `key` of a map.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Map(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The value under `key`, to change; `None` when there is none.
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        match self {
            Value::Map(entries) => entries.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Sets `key` of a map: in its place when it is there, at the end otherwise.
    pub fn set(&mut self, key: &str, value: Value) {
        if let Value::Map(entries) = self {
            match entries.iter_mut().find(|(k, _)| k == key) {
                Some((_, slot)) => *slot = value,
                None => entries.push((key.to_owned(), value)),
            }
        }
    }

    /// Takes `key` out of a map.
    pub fn remove(&mut self, key: &str) -> Option<Value> {
        match self {
            Value::Map(entries) => {
                let at = entries.iter().position(|(k, _)| k == key)?;
                Some(entries.remove(at).1)
            }
            _ => None,
        }
    }

    pub fn has(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Int(v) => Some(*v as f64),
            Value::Float(v) => Some(*v),
            Value::Bool(v) => Some(f64::from(u8::from(*v))),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(text) => Some(text),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&[Value]> {
        match self {
            Value::List(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_map(&self) -> Option<&[(String, Value)]> {
        match self {
            Value::Map(entries) => Some(entries),
            _ => None,
        }
    }

    /// Python's truth: nothing, false, zero, an empty text, list or map are false.
    pub fn truthy(&self) -> bool {
        match self {
            Value::Null => false,
            Value::Bool(v) => *v,
            Value::Int(v) => *v != 0,
            Value::Float(v) => *v != 0.0,
            Value::Str(text) => !text.is_empty(),
            Value::List(items) => !items.is_empty(),
            Value::Map(entries) => !entries.is_empty(),
        }
    }

    /// A list whose first item is a list: keys over time (`[[t, value], ...]`).
    pub fn is_keys(&self) -> bool {
        matches!(self.as_list(), Some([Value::List(_), ..]))
    }

    /// As Python's `repr` writes it (in messages).
    pub fn repr(&self) -> String {
        match self {
            Value::Null => "None".into(),
            Value::Bool(true) => "True".into(),
            Value::Bool(false) => "False".into(),
            Value::Int(v) => v.to_string(),
            Value::Float(v) => float_repr(*v),
            Value::Str(text) => format!("'{text}'"),
            Value::List(items) => {
                let inner: Vec<String> = items.iter().map(Value::repr).collect();
                format!("[{}]", inner.join(", "))
            }
            Value::Map(entries) => {
                let inner: Vec<String> = entries.iter().map(|(k, v)| format!("'{k}': {}", v.repr())).collect();
                format!("{{{}}}", inner.join(", "))
            }
        }
    }
}

impl From<f64> for Value {
    fn from(value: f64) -> Self {
        Value::Float(value)
    }
}

impl From<i64> for Value {
    fn from(value: i64) -> Self {
        Value::Int(value)
    }
}

impl From<&str> for Value {
    fn from(value: &str) -> Self {
        Value::Str(value.to_owned())
    }
}

impl From<bool> for Value {
    fn from(value: bool) -> Self {
        Value::Bool(value)
    }
}

/// A float as Python's `repr` writes it: the shortest digits that read back the same, in
/// scientific notation below 1e-4 and from 1e16.
pub fn float_repr(v: f64) -> String {
    if v.is_nan() {
        return "NaN".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    if v == 0.0 {
        return if v.is_sign_negative() {
            "-0.0".into()
        } else {
            "0.0".into()
        };
    }
    // The shortest digits, and the exponent of the first one.
    let sci = format!("{:e}", v.abs());
    let (mantissa, exponent) = sci.split_once('e').expect("a mantissa and an exponent");
    let exponent: i32 = exponent.parse().expect("an exponent");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let sign = if v < 0.0 { "-" } else { "" };
    if !(-4..16).contains(&exponent) {
        let rest = &digits[1..];
        let mantissa = if rest.is_empty() {
            digits[..1].to_owned()
        } else {
            format!("{}.{rest}", &digits[..1])
        };
        let esign = if exponent < 0 { '-' } else { '+' };
        return format!("{sign}{mantissa}e{esign}{:02}", exponent.abs());
    }
    if exponent < 0 {
        let zeros = "0".repeat((-exponent - 1) as usize);
        return format!("{sign}0.{zeros}{digits}");
    }
    let point = exponent as usize + 1;
    if digits.len() <= point {
        format!("{sign}{digits}{}.0", "0".repeat(point - digits.len()))
    } else {
        format!("{sign}{}.{}", &digits[..point], &digits[point..])
    }
}

// --- reading ------------------------------------------------------------------------------------

/// What is wrong in a text read as JSON, and where.
#[derive(Clone, Debug, PartialEq)]
pub struct JsonError {
    pub line: usize,
    pub column: usize,
    pub message: String,
}

impl std::fmt::Display for JsonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}, column {}: {}", self.line, self.column, self.message)
    }
}

impl std::error::Error for JsonError {}

struct Parser<'a> {
    text: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn fail<T>(&self, message: impl Into<String>) -> Result<T, JsonError> {
        let before = &self.text[..self.at.min(self.text.len())];
        let line = before.iter().filter(|b| **b == b'\n').count() + 1;
        let column = before.iter().rev().take_while(|b| **b != b'\n').count() + 1;
        Err(JsonError {
            line,
            column,
            message: message.into(),
        })
    }

    fn blank(&mut self) {
        while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.text.get(self.at) {
            self.at += 1;
        }
    }

    fn eat(&mut self, word: &str) -> bool {
        if self.text[self.at..].starts_with(word.as_bytes()) {
            self.at += word.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, JsonError> {
        if depth > 200 {
            return self.fail("nested too deep");
        }
        self.blank();
        match self.text.get(self.at) {
            None => self.fail("a value expected, the end found"),
            Some(b'{') => {
                self.at += 1;
                let mut entries: Vec<(String, Value)> = Vec::new();
                self.blank();
                if self.eat("}") {
                    return Ok(Value::Map(entries));
                }
                loop {
                    self.blank();
                    if self.text.get(self.at) != Some(&b'"') {
                        return self.fail("a key in quotes expected");
                    }
                    let key = self.string()?;
                    self.blank();
                    if !self.eat(":") {
                        return self.fail("':' expected");
                    }
                    let value = self.value(depth + 1)?;
                    // As Python: a key given twice keeps its first place and its last value.
                    match entries.iter_mut().find(|(k, _)| *k == key) {
                        Some((_, slot)) => *slot = value,
                        None => entries.push((key, value)),
                    }
                    self.blank();
                    if self.eat(",") {
                        continue;
                    }
                    if self.eat("}") {
                        return Ok(Value::Map(entries));
                    }
                    return self.fail("',' or '}' expected");
                }
            }
            Some(b'[') => {
                self.at += 1;
                let mut items = Vec::new();
                self.blank();
                if self.eat("]") {
                    return Ok(Value::List(items));
                }
                loop {
                    items.push(self.value(depth + 1)?);
                    self.blank();
                    if self.eat(",") {
                        continue;
                    }
                    if self.eat("]") {
                        return Ok(Value::List(items));
                    }
                    return self.fail("',' or ']' expected");
                }
            }
            Some(b'"') => Ok(Value::Str(self.string()?)),
            _ if self.eat("true") => Ok(Value::Bool(true)),
            _ if self.eat("false") => Ok(Value::Bool(false)),
            _ if self.eat("null") => Ok(Value::Null),
            _ if self.eat("NaN") => Ok(Value::Float(f64::NAN)),
            _ if self.eat("Infinity") => Ok(Value::Float(f64::INFINITY)),
            _ if self.eat("-Infinity") => Ok(Value::Float(f64::NEG_INFINITY)),
            _ => self.number(),
        }
    }

    fn number(&mut self) -> Result<Value, JsonError> {
        let start = self.at;
        let mut float = false;
        if self.text.get(self.at) == Some(&b'-') {
            self.at += 1;
        }
        let digits = |p: &mut Self| {
            let from = p.at;
            while p.text.get(p.at).is_some_and(u8::is_ascii_digit) {
                p.at += 1;
            }
            p.at > from
        };
        if !digits(self) {
            return self.fail("a value expected");
        }
        if self.text.get(self.at) == Some(&b'.') {
            self.at += 1;
            float = true;
            if !digits(self) {
                return self.fail("digits expected after '.'");
            }
        }
        if let Some(b'e' | b'E') = self.text.get(self.at) {
            self.at += 1;
            float = true;
            if let Some(b'+' | b'-') = self.text.get(self.at) {
                self.at += 1;
            }
            if !digits(self) {
                return self.fail("digits expected in the exponent");
            }
        }
        let text = std::str::from_utf8(&self.text[start..self.at]).expect("ASCII");
        if !float && let Ok(v) = text.parse::<i64>() {
            return Ok(Value::Int(v));
        }
        match text.parse::<f64>() {
            Ok(v) => Ok(Value::Float(v)),
            Err(_) => self.fail(format!("number {text}")),
        }
    }

    fn hex4(&mut self) -> Result<u32, JsonError> {
        let Some(hex) = self.text.get(self.at..self.at + 4) else {
            return self.fail("four hexadecimal digits expected");
        };
        let Some(code) = std::str::from_utf8(hex)
            .ok()
            .and_then(|h| u32::from_str_radix(h, 16).ok())
        else {
            return self.fail("four hexadecimal digits expected");
        };
        self.at += 4;
        Ok(code)
    }

    fn string(&mut self) -> Result<String, JsonError> {
        self.at += 1;
        let mut out = String::new();
        loop {
            let start = self.at;
            while let Some(b) = self.text.get(self.at) {
                if *b == b'"' || *b == b'\\' || *b < 0x20 {
                    break;
                }
                self.at += 1;
            }
            match std::str::from_utf8(&self.text[start..self.at]) {
                Ok(part) => out.push_str(part),
                Err(_) => return self.fail("text not in UTF-8"),
            }
            match self.text.get(self.at) {
                None => return self.fail("a text without its end"),
                Some(b'"') => {
                    self.at += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.at += 1;
                    let Some(escape) = self.text.get(self.at).copied() else {
                        return self.fail("an escape without its letter");
                    };
                    self.at += 1;
                    match escape {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let mut code = self.hex4()?;
                            if (0xD800..0xDC00).contains(&code) && self.text[self.at..].starts_with(b"\\u") {
                                let back = self.at;
                                self.at += 2;
                                let low = self.hex4()?;
                                if (0xDC00..0xE000).contains(&low) {
                                    code = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                                } else {
                                    self.at = back;
                                }
                            }
                            out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                        }
                        _ => return self.fail(format!("escape \\{}", escape as char)),
                    }
                }
                Some(_) => return self.fail("a control character in a text"),
            }
        }
    }
}

/// A JSON text read.
pub fn parse(text: &str) -> Result<Value, JsonError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut parser = Parser {
        text: text.as_bytes(),
        at: 0,
    };
    let value = parser.value(0)?;
    parser.blank();
    if parser.at < parser.text.len() {
        return parser.fail("text after the value");
    }
    Ok(value)
}

// --- writing ------------------------------------------------------------------------------------

fn string(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            ' '..='~' => out.push(c),
            _ => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
        }
    }
    out.push('"');
}

fn write_value(out: &mut String, value: &Value, depth: usize, newline: &str) {
    let indent = |out: &mut String, depth: usize| {
        out.push_str(newline);
        out.push_str(&" ".repeat(depth));
    };
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(v) => out.push_str(if *v { "true" } else { "false" }),
        Value::Int(v) => out.push_str(&v.to_string()),
        Value::Float(v) => out.push_str(&float_repr(*v)),
        Value::Str(text) => string(out, text),
        Value::List(items) if items.is_empty() => out.push_str("[]"),
        Value::List(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                indent(out, depth + 1);
                write_value(out, item, depth + 1, newline);
            }
            indent(out, depth);
            out.push(']');
        }
        Value::Map(entries) if entries.is_empty() => out.push_str("{}"),
        Value::Map(entries) => {
            out.push('{');
            for (index, (key, item)) in entries.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                indent(out, depth + 1);
                string(out, key);
                out.push_str(": ");
                write_value(out, item, depth + 1, newline);
            }
            indent(out, depth);
            out.push('}');
        }
    }
}

/// How a file's lines end, to write it again as it was.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Layout {
    pub crlf: bool,
    pub final_newline: bool,
}

impl Layout {
    /// The layout of a text read.
    pub fn of(text: &str) -> Self {
        Self {
            crlf: text.contains("\r\n"),
            final_newline: text.ends_with('\n'),
        }
    }
}

/// The value as `json.dump(value, file, indent=1)` writes it, its lines ended as `layout` says.
pub fn write(value: &Value, layout: Layout) -> String {
    let newline = if layout.crlf { "\r\n" } else { "\n" };
    let mut out = String::new();
    write_value(&mut out, value, 0, newline);
    if layout.final_newline {
        out.push_str(newline);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats_are_written_as_python_writes_them() {
        for (v, text) in [
            (0.1, "0.1"),
            (1.0, "1.0"),
            (-2.5, "-2.5"),
            (100.0, "100.0"),
            (1e-5, "1e-05"),
            (0.0001, "0.0001"),
            (1e16, "1e+16"),
            (1.5e16, "1.5e+16"),
            (123456789012345.6, "123456789012345.6"),
            (0.333, "0.333"),
            (-0.0, "-0.0"),
        ] {
            assert_eq!(float_repr(v), text);
        }
    }

    #[test]
    fn a_file_read_and_written_again_is_the_same() {
        let text = "{\r\n \"name\": \"vfx\",\r\n \"pos\": [\r\n  0,\r\n  1.5,\r\n  -0.25\r\n ],\r\n \"e\": {},\r\n \"l\": [],\r\n \"t\": \"Spells\\\\X \\u00e9\",\r\n \"b\": true\r\n}\r\n";
        let value = parse(text).unwrap();
        assert_eq!(value.get("pos").unwrap().as_list().unwrap()[0], Value::Int(0));
        assert_eq!(value.get("t").unwrap().as_str(), Some("Spells\\X é"));
        assert_eq!(write(&value, Layout::of(text)), text);
    }
}
