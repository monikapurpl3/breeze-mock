//! A strict reader over roxmltree.
//!
//! A blueprint is a test fixture, and a fixture that is quietly reinterpreted
//! makes a test pass for the wrong reason. So this is as strict as XML allows
//! and then some, and it says exactly what `schema/blueprint.xsd` says:
//!
//! * the document starts with `<?xml version="1.0" encoding="UTF-8"?>`;
//! * no DTD, no entity declarations, no processing instructions;
//! * every element is in [`NAMESPACE`], and no attribute is (the one exception
//!   is `xsi:schemaLocation` on the root, so editors can find the schema);
//! * children appear in the documented order and number ([`Seq`]);
//! * every attribute is either read or refused; required ones are required;
//! * values match their type exactly: no surrounding whitespace, `true` and
//!   `false` only, units on every duration and power, one spelling per
//!   enumeration, lowercase hex of the exact length;
//! * no text where there should be none.
//!
//! Every error carries a file, line and column.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The blueprint namespace. Versioned: a future incompatible format is a new
/// namespace, so an old file is never read under new rules by accident.
pub const NAMESPACE: &str = "urn:breeze-mock:blueprint:1";
const XSI: &str = "http://www.w3.org/2001/XMLSchema-instance";

#[derive(Debug, Clone)]
pub struct Error {
    pub file: PathBuf,
    pub line: u32,
    pub col: u32,
    pub message: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}:{}: {}",
            self.file.display(),
            self.line,
            self.col,
            self.message
        )
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

fn at(file: &Path, line: u32, col: u32, message: impl Into<String>) -> Error {
    Error {
        file: file.to_path_buf(),
        line,
        col,
        message: message.into(),
    }
}

/// Read a file and check everything that is about the document rather than
/// any one element.
pub fn parse_document<'i>(file: &Path, text: &'i str) -> Result<roxmltree::Document<'i>> {
    let body = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    check_declaration(file, body)?;
    let opts = roxmltree::ParsingOptions {
        // A DTD can declare entities, defaults and attribute types that change
        // what the document means without appearing in it. Refused outright.
        allow_dtd: false,
        ..Default::default()
    };
    let doc = roxmltree::Document::parse_with_options(text, opts).map_err(|e| {
        let p = e.pos();
        at(file, p.row, p.col, format!("not well-formed XML: {e}"))
    })?;
    for n in doc.descendants() {
        if n.is_pi() {
            let p = doc.text_pos_at(n.range().start);
            return Err(at(
                file,
                p.row,
                p.col,
                "processing instructions are not allowed",
            ));
        }
    }
    Ok(doc)
}

/// The declaration is required, and must say version 1.0 and UTF-8. roxmltree
/// accepts a document without one; the schema's promise is stronger.
fn check_declaration(file: &Path, body: &str) -> Result<()> {
    let Some(rest) = body.strip_prefix("<?xml") else {
        return Err(at(
            file,
            1,
            1,
            "a blueprint must start with <?xml version=\"1.0\" encoding=\"UTF-8\"?>",
        ));
    };
    let end = rest
        .find("?>")
        .ok_or_else(|| at(file, 1, 1, "unterminated XML declaration"))?;
    let decl = &rest[..end];
    let has = |name: &str, value: &str| {
        decl.contains(&format!("{name}=\"{value}\"")) || decl.contains(&format!("{name}='{value}'"))
    };
    if !has("version", "1.0") {
        return Err(at(
            file,
            1,
            1,
            "the XML declaration must say version=\"1.0\"",
        ));
    }
    if !(has("encoding", "UTF-8") || has("encoding", "utf-8")) {
        return Err(at(
            file,
            1,
            1,
            "the XML declaration must say encoding=\"UTF-8\"",
        ));
    }
    Ok(())
}

/// Where an element was written, kept past parsing so checks that need the
/// whole blueprint can still point at the element they are about.
#[derive(Debug, Clone)]
pub struct Origin {
    pub file: PathBuf,
    pub line: u32,
    pub col: u32,
    pub element: String,
}

impl Origin {
    pub fn err(&self, message: impl Into<String>) -> Error {
        at(
            &self.file,
            self.line,
            self.col,
            format!("<{}>: {}", self.element, message.into()),
        )
    }
}

/// One element, with the bookkeeping to notice what nobody asked for.
pub struct El<'a, 'i> {
    pub node: roxmltree::Node<'a, 'i>,
    file: &'a Path,
    /// Attributes that were present and read.
    used: RefCell<BTreeSet<String>>,
    /// Every attribute name the element's reader asked about, present or not:
    /// the vocabulary typos are compared against.
    asked: RefCell<BTreeSet<String>>,
}

impl<'a, 'i> El<'a, 'i> {
    /// Wrap an element, refusing it if it is not in the blueprint namespace.
    pub fn new(node: roxmltree::Node<'a, 'i>, file: &'a Path) -> Result<Self> {
        let el = Self {
            node,
            file,
            used: RefCell::new(BTreeSet::new()),
            asked: RefCell::new(BTreeSet::new()),
        };
        match node.tag_name().namespace() {
            Some(NAMESPACE) => Ok(el),
            Some(other) => Err(el.err(format!(
                "is in namespace {other:?}; blueprint elements belong to {NAMESPACE:?}"
            ))),
            None => Err(el.err(format!(
                "is in no namespace; declare xmlns=\"{NAMESPACE}\" on the root"
            ))),
        }
    }

    pub fn file(&self) -> &'a Path {
        self.file
    }

    pub fn name(&self) -> &str {
        self.node.tag_name().name()
    }

    pub fn origin(&self) -> Origin {
        let pos = self.node.document().text_pos_at(self.node.range().start);
        Origin {
            file: self.file.to_path_buf(),
            line: pos.row,
            col: pos.col,
            element: self.name().to_string(),
        }
    }

    pub fn err(&self, message: impl Into<String>) -> Error {
        let pos = self.node.document().text_pos_at(self.node.range().start);
        at(
            self.file,
            pos.row,
            pos.col,
            format!("<{}>: {}", self.name(), message.into()),
        )
    }

    /// An attribute's raw value, if present. Marks it asked, and used if there.
    pub fn raw(&self, name: &str) -> Option<&'a str> {
        self.asked.borrow_mut().insert(name.to_string());
        let v = self
            .node
            .attributes()
            .find(|a| a.name() == name && a.namespace().is_none())
            .map(|a| a.value());
        if v.is_some() {
            self.used.borrow_mut().insert(name.to_string());
        }
        v
    }

    /// An attribute parsed as `T`, with the attribute named in any error.
    /// Surrounding whitespace is always an error: XML would keep it, and a
    /// value that only works because it was trimmed is not the value written.
    pub fn get<T>(
        &self,
        name: &str,
        parse: impl Fn(&str) -> std::result::Result<T, String>,
    ) -> Result<Option<T>> {
        match self.raw(name) {
            None => Ok(None),
            Some(raw) => {
                if raw.is_empty() || raw.starts_with(is_xml_space) || raw.ends_with(is_xml_space) {
                    return Err(self.err(format!(
                        "{name}={raw:?}: empty, or has surrounding whitespace"
                    )));
                }
                parse(raw)
                    .map(Some)
                    .map_err(|e| self.err(format!("{name}={raw:?}: {e}")))
            }
        }
    }

    /// A required attribute. When it is missing but something close to it is
    /// there, the message points at the likely typo rather than only at what
    /// is absent.
    pub fn need<T>(
        &self,
        name: &str,
        parse: impl Fn(&str) -> std::result::Result<T, String>,
    ) -> Result<T> {
        if let Some(v) = self.get(name, parse)? {
            return Ok(v);
        }
        let asked = self.asked.borrow();
        let typo = self.node.attributes().find(|a| {
            a.namespace().is_none() && !asked.contains(a.name()) && distance(a.name(), name) <= 2
        });
        Err(self.err(match typo {
            Some(a) => format!(
                "needs the {name}=\"...\" attribute (is {}={:?} a misspelling of it?)",
                a.name(),
                a.value()
            ),
            None => format!("needs the {name}=\"...\" attribute"),
        }))
    }

    /// The element's text content. Only for elements whose content is text:
    /// child elements, or text with surrounding whitespace, are errors.
    pub fn text<T>(&self, parse: impl Fn(&str) -> std::result::Result<T, String>) -> Result<T> {
        let mut text = String::new();
        for c in self.node.children() {
            if c.is_element() {
                return Err(self.err("holds text only, not elements"));
            }
            if c.is_text() {
                text.push_str(c.text().unwrap_or(""));
            }
        }
        if text.is_empty() || text.starts_with(is_xml_space) || text.ends_with(is_xml_space) {
            return Err(self.err(format!(
                "text {text:?} is empty, or has surrounding whitespace"
            )));
        }
        parse(&text).map_err(|e| self.err(format!("{text:?}: {e}")))
    }

    /// Refuses any child element and any text: for empty elements.
    pub fn empty(&self) -> Result<()> {
        for c in self.node.children() {
            if c.is_element() {
                return Err(El::new(c, self.file)?
                    .err(format!("not allowed: <{}> is always empty", self.name())));
            }
            if c.is_text() && !c.text().unwrap_or("").chars().all(is_xml_space) {
                return Err(self.err("is always empty, but holds text"));
            }
        }
        Ok(())
    }

    /// Checks every attribute was read. Call last.
    pub fn done(&self) -> Result<()> {
        let used = self.used.borrow();
        let asked = self.asked.borrow();
        let is_root = self.node.parent().is_some_and(|p| p.is_root());
        for a in self.node.attributes() {
            match a.namespace() {
                None if used.contains(a.name()) => {}
                None => {
                    let known: Vec<&str> = asked.iter().map(String::as_str).collect();
                    return Err(self.err(format!(
                        "unknown attribute {}={:?}{}",
                        a.name(),
                        a.value(),
                        suggest(a.name(), &known)
                    )));
                }
                Some(XSI) if is_root && a.name() == "schemaLocation" => {}
                Some(ns) => {
                    return Err(self.err(format!(
                        "attribute {} in namespace {ns:?} is not allowed{}",
                        a.name(),
                        if ns == XSI {
                            " (xsi:schemaLocation belongs on the root only)"
                        } else {
                            ""
                        }
                    )))
                }
            }
        }
        Ok(())
    }
}

/// An `xs:sequence`: the children an element may have, in order, each with a
/// minimum and maximum count. Walked once; anything out of order, repeated
/// too often, missing or unknown is an error at the child that broke it.
pub struct Seq<'p, 'a, 'i> {
    parent: &'p El<'a, 'i>,
    /// Children in document order; taken ones become `None`.
    items: Vec<Option<El<'a, 'i>>>,
    names: Vec<String>,
    pos: usize,
    order: &'static [&'static str],
}

impl<'p, 'a, 'i> Seq<'p, 'a, 'i> {
    pub fn new(parent: &'p El<'a, 'i>, order: &'static [&'static str]) -> Result<Self> {
        let mut items = Vec::new();
        for c in parent.node.children() {
            if c.is_element() {
                items.push(El::new(c, parent.file)?);
            } else if c.is_text() && !c.text().unwrap_or("").trim().is_empty() {
                return Err(parent.err(format!(
                    "unexpected text {:?}",
                    c.text().unwrap_or("").trim()
                )));
            }
        }
        // Unknown names are reported before order, so a typo reads as a typo.
        for it in &items {
            if !order.contains(&it.name()) {
                return Err(it.err(format!(
                    "not allowed inside <{}>{}",
                    parent.name(),
                    suggest(it.name(), order)
                )));
            }
        }
        let names = items.iter().map(|e| e.name().to_string()).collect();
        Ok(Self {
            parent,
            items: items.into_iter().map(Some).collect(),
            names,
            pos: 0,
            order,
        })
    }

    /// Take the next run of `name`, between `min` and `max` of them.
    pub fn take(&mut self, name: &'static str, min: usize, max: usize) -> Result<Vec<El<'a, 'i>>> {
        let mut out = Vec::new();
        while self.pos < self.names.len() && self.names[self.pos] == name {
            let el = self.items[self.pos]
                .take()
                .expect("each child is taken once");
            if out.len() == max {
                return Err(el.err(format!(
                    "<{}> allows {} <{name}>",
                    self.parent.name(),
                    if max == 1 {
                        "only one".to_string()
                    } else {
                        format!("at most {max}")
                    },
                )));
            }
            out.push(el);
            self.pos += 1;
        }
        if out.len() < min {
            // Present but further on is out of order, not missing -- say so,
            // at the element that is in the wrong place.
            if let Some(i) = (self.pos..self.names.len()).find(|&i| self.names[i] == name) {
                let here = self.names[self.pos].clone();
                let el = self.items[i].take().expect("not yet taken");
                return Err(el.err(format!(
                    "is out of order: inside <{}> it must come before <{here}> (order: {})",
                    self.parent.name(),
                    self.order.join(", ")
                )));
            }
            return Err(self.parent.err(if min == 1 {
                format!("needs a <{name}>")
            } else {
                format!("needs at least {min} <{name}> elements")
            }));
        }
        Ok(out)
    }

    pub fn one(&mut self, name: &'static str) -> Result<El<'a, 'i>> {
        Ok(self.take(name, 1, 1)?.remove(0))
    }

    pub fn maybe(&mut self, name: &'static str) -> Result<Option<El<'a, 'i>>> {
        Ok(self.take(name, 0, 1)?.pop())
    }

    pub fn many(&mut self, name: &'static str) -> Result<Vec<El<'a, 'i>>> {
        self.take(name, 0, usize::MAX)
    }

    /// Everything must have been consumed, in order. Anything left is out of
    /// place, and the message says where it belongs.
    pub fn end(mut self) -> Result<()> {
        if self.pos < self.names.len() {
            let name = self.names[self.pos].clone();
            let it = self.items[self.pos].take().expect("not yet taken");
            let i = self.order.iter().position(|n| *n == name).unwrap_or(0);
            let later_seen = self.order[i + 1..]
                .iter()
                .find(|n| self.names[..self.pos].iter().any(|x| x == *n));
            return Err(it.err(match later_seen {
                Some(b) => format!(
                    "is out of order: inside <{}> it must come before <{b}> (order: {})",
                    self.parent.name(),
                    self.order.join(", ")
                ),
                None => format!(
                    "is out of place inside <{}> (order: {})",
                    self.parent.name(),
                    self.order.join(", ")
                ),
            }));
        }
        Ok(())
    }
}

/// " (did you mean X?)" when something is close enough to be a typo, and
/// nothing otherwise.
pub fn did_you_mean(got: &str, known: &[&str]) -> String {
    known
        .iter()
        .map(|k| (distance(got, k), *k))
        .filter(|(d, k)| *d <= 2.max(k.len() / 3))
        .min()
        .map(|(_, k)| format!(" (did you mean {k:?}?)"))
        .unwrap_or_default()
}

/// ", did you mean X?" when something is close enough to be a typo, or the
/// list of what is accepted when it is short enough to be worth reading.
pub fn suggest(got: &str, known: &[&str]) -> String {
    let best = known
        .iter()
        .map(|k| (distance(got, k), *k))
        .filter(|(d, k)| *d <= 2.max(k.len() / 3))
        .min();
    match best {
        Some((_, k)) => format!(" (did you mean {k:?}?)"),
        None if !known.is_empty() && known.len() <= 16 => {
            format!(" (expected one of: {})", known.join(", "))
        }
        None => String::new(),
    }
}

fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        prev = cur;
    }
    prev[b.len()]
}

// ------------------------------------------------------------ value types
//
// Each of these is the Rust side of a simpleType in the schema, and accepts
// exactly what that type's pattern does -- no more.

/// XML's whitespace: space, tab, CR and LF -- what XSD's `\s` means, and
/// nothing else (a no-break space is text).
pub fn is_xml_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\r' | '\n')
}

/// A whole number, optionally negative, no leading zeros: `-1`, `0`, `2`.
pub fn integer(s: &str) -> std::result::Result<i64, String> {
    let (neg, body) = match s.strip_prefix('-') {
        Some(b) => (true, b),
        None => (false, s),
    };
    let v: i64 = unsigned(body)?;
    Ok(if neg { -v } else { v })
}

/// `xs:boolean` narrowed to its two canonical forms.
pub fn boolean(s: &str) -> std::result::Result<bool, String> {
    match s {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err("expected true or false".into()),
    }
}

/// Digits, optionally with a fractional part: `12`, `2.6`. No sign, no
/// exponent, no leading dot.
pub fn unsigned_decimal(s: &str) -> Option<f64> {
    let (int, frac) = match s.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (s, None),
    };
    let digits = |t: &str| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit());
    if !digits(int) || frac.is_some_and(|f| !digits(f)) {
        return None;
    }
    s.parse().ok()
}

/// A decimal, optionally negative: `-4.5`.
pub fn decimal(s: &str) -> std::result::Result<f64, String> {
    let (neg, body) = match s.strip_prefix('-') {
        Some(b) => (true, b),
        None => (false, s),
    };
    let v = unsigned_decimal(body).ok_or("expected a decimal number like 21 or -4.5")?;
    Ok(if neg { -v } else { v })
}

pub fn positive(s: &str) -> std::result::Result<f64, String> {
    let v = unsigned_decimal(s).ok_or("expected a positive decimal number like 14 or 2.6")?;
    if v <= 0.0 {
        return Err("must be greater than zero".into());
    }
    Ok(v)
}

pub fn unsigned<T: std::str::FromStr>(s: &str) -> std::result::Result<T, String> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) || (s.len() > 1 && s.starts_with('0'))
    {
        return Err("expected a whole number without leading zeros".into());
    }
    s.parse::<T>().map_err(|_| "out of range".to_string())
}

/// A number followed by exactly one unit: `720ms`, `3s`, `5m`, `2h`, `1d`.
pub fn duration(s: &str) -> std::result::Result<Duration, String> {
    let split = s
        .find(|c: char| c.is_ascii_alphabetic())
        .ok_or("a duration needs a unit: 720ms, 3s, 5m, 2h or 1d")?;
    let (num, unit) = s.split_at(split);
    let v = unsigned_decimal(num).ok_or("expected a duration like 720ms, 3s, 5m, 2h or 1d")?;
    let secs = match unit {
        "ms" => v / 1000.0,
        "s" => v,
        "m" => v * 60.0,
        "h" => v * 3600.0,
        "d" => v * 86400.0,
        other => return Err(format!("unknown unit {other:?}; use ms, s, m, h or d")),
    };
    Ok(Duration::from_secs_f64(secs))
}

/// A whole number of s, m, h or d with a mandatory sign: `+600s`, `-2m`.
pub fn signed_seconds(s: &str) -> std::result::Result<i64, String> {
    let (neg, rest) = if let Some(r) = s.strip_prefix('-') {
        (true, r)
    } else if let Some(r) = s.strip_prefix('+') {
        (false, r)
    } else {
        return Err("needs a sign: +600s or -2m".into());
    };
    let split = rest
        .find(|c: char| !c.is_ascii_digit())
        .ok_or("needs a unit: s, m, h or d")?;
    let (num, unit) = rest.split_at(split);
    let n: i64 = unsigned(num)?;
    let factor = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        _ => return Err("a signed duration is whole s, m, h or d, like +2h".into()),
    };
    Ok(if neg { -n * factor } else { n * factor })
}

/// `3.5kW` or `3500W`, case as written.
pub fn watts(s: &str) -> std::result::Result<f64, String> {
    let (num, factor) = if let Some(n) = s.strip_suffix("kW") {
        (n, 1000.0)
    } else if let Some(n) = s.strip_suffix('W') {
        (n, 1.0)
    } else {
        return Err("a power needs a unit: 3.5kW or 3500W".into());
    };
    Ok(unsigned_decimal(num).ok_or("expected a power like 3.5kW or 3500W")? * factor)
}

/// A probability: `1/30`, `0.05` or `5%`, between 0 and 1.
pub fn rate(s: &str) -> std::result::Result<f64, String> {
    let v = if let Some((a, b)) = s.split_once('/') {
        let a: u64 = unsigned(a)?;
        let b: u64 = unsigned(b)?;
        if b == 0 {
            return Err("division by zero".into());
        }
        a as f64 / b as f64
    } else if let Some(p) = s.strip_suffix('%') {
        unsigned_decimal(p).ok_or("expected a percentage like 5%")? / 100.0
    } else {
        unsigned_decimal(s).ok_or("expected a rate like 1/30, 0.05 or 5%")?
    };
    if v > 1.0 {
        return Err("a rate cannot exceed 1".into());
    }
    Ok(v)
}

/// Degrees `from-to`: `16-30`, `17.5-30`.
pub fn range(s: &str) -> std::result::Result<(f64, f64), String> {
    let (a, b) = s.split_once('-').ok_or("expected a range like 16-30")?;
    let a = unsigned_decimal(a).ok_or("bad lower bound")?;
    let b = unsigned_decimal(b).ok_or("bad upper bound")?;
    if a > b {
        return Err("the lower bound is above the upper".into());
    }
    Ok((a, b))
}

/// Lowercase hex, an even number of digits, nothing else.
pub fn hex(s: &str) -> std::result::Result<Vec<u8>, String> {
    if !s.len().is_multiple_of(2) {
        return Err("an odd number of hex digits".into());
    }
    if !s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err("only lowercase hex digits 0-9 a-f".into());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

/// Exactly `n` bytes of lowercase hex.
pub fn hex_exact(n: usize) -> impl Fn(&str) -> std::result::Result<Vec<u8>, String> {
    move |s| {
        let b = hex(s)?;
        if b.len() != n {
            return Err(format!(
                "must be exactly {} hex digits ({n} bytes), not {}",
                2 * n,
                s.len()
            ));
        }
        Ok(b)
    }
}

/// `0x` and four lowercase hex digits: a capability id.
pub fn hex16(s: &str) -> std::result::Result<u16, String> {
    let digits = s
        .strip_prefix("0x")
        .ok_or("expected 0x and four hex digits, like 0x0040")?;
    let b = hex(digits)?;
    if b.len() != 2 {
        return Err("expected exactly four hex digits".into());
    }
    Ok(u16::from_be_bytes([b[0], b[1]]))
}

/// `0x` and two lowercase hex digits: a byte.
pub fn hex8(s: &str) -> std::result::Result<u8, String> {
    let digits = s
        .strip_prefix("0x")
        .ok_or("expected 0x and two hex digits, like 0xac")?;
    let b = hex(digits)?;
    if b.len() != 1 {
        return Err("expected exactly two hex digits".into());
    }
    Ok(b[0])
}

/// `HH:MM`, 00:00 to 24:00, as fractional hours.
pub fn clock(s: &str) -> std::result::Result<f64, String> {
    let bytes = s.as_bytes();
    if bytes.len() != 5
        || bytes[2] != b':'
        || !s[..2]
            .bytes()
            .chain(s[3..].bytes())
            .all(|b| b.is_ascii_digit())
    {
        return Err("expected HH:MM, like 07:30".into());
    }
    let h: u32 = s[..2].parse().map_err(|_| "bad hour")?;
    let m: u32 = s[3..].parse().map_err(|_| "bad minute")?;
    if h > 24 || m > 59 || (h == 24 && m != 0) {
        return Err("not a time of day".into());
    }
    Ok(h as f64 + m as f64 / 60.0)
}

/// An identifier: a lowercase letter, then lowercase letters, digits and
/// hyphens. What keys, ids and names are, so they are safe in URLs, shell
/// arguments and interface names alike.
pub fn ident(s: &str) -> std::result::Result<String, String> {
    let mut chars = s.chars();
    let ok = chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !s.ends_with('-')
        && !s.contains("--");
    if !ok {
        return Err(
            "expected a lowercase identifier like lijeva-soba (a-z, 0-9, single hyphens)".into(),
        );
    }
    if s.len() > 32 {
        return Err("at most 32 characters".into());
    }
    Ok(s.to_string())
}

/// One of a fixed set of words, spelt exactly.
pub fn one_of<'e>(
    options: &'e [&'e str],
) -> impl Fn(&str) -> std::result::Result<String, String> + 'e {
    move |s| {
        if options.contains(&s) {
            Ok(s.to_string())
        } else {
            Err(format!(
                "expected one of: {}{}",
                options.join(", "),
                did_you_mean(s, options)
            ))
        }
    }
}

/// Free text: anything, as long as it is not empty and not padded. Control
/// characters are refused, since these end up in JSON, logs and headers.
pub fn text(s: &str) -> std::result::Result<String, String> {
    if s.chars().any(|c| c.is_control()) {
        return Err("control characters are not allowed".into());
    }
    Ok(s.to_string())
}

/// A space-separated list of `item`s, each spelt as `item` accepts, with no
/// duplicates and single spaces.
pub fn list<T: PartialEq>(
    item: impl Fn(&str) -> std::result::Result<T, String>,
) -> impl Fn(&str) -> std::result::Result<Vec<T>, String> {
    move |s| {
        if s.contains("  ") {
            return Err("separate items with single spaces".into());
        }
        let mut out: Vec<T> = Vec::new();
        for part in s.split(' ') {
            let v = item(part).map_err(|e| format!("{part:?}: {e}"))?;
            if out.contains(&v) {
                return Err(format!("{part:?} is listed twice"));
            }
            out.push(v);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_accept_exactly_their_patterns() {
        assert_eq!(duration("720ms").unwrap(), Duration::from_millis(720));
        assert_eq!(duration("5m").unwrap(), Duration::from_secs(300));
        assert!(duration("30").is_err(), "a unit is mandatory");
        assert!(duration("1e3ms").is_err());
        assert!(duration("5 m").is_err());
        assert!(duration("5min").is_err());
        assert_eq!(signed_seconds("-2m").unwrap(), -120);
        assert_eq!(signed_seconds("+600s").unwrap(), 600);
        assert!(signed_seconds("600s").is_err(), "the sign is mandatory");
        assert_eq!(watts("3.5kW").unwrap(), 3500.0);
        assert!(watts("3.5kw").is_err(), "units are case-sensitive");
        assert!(watts("3500").is_err());
        assert!((rate("1/30").unwrap() - 1.0 / 30.0).abs() < 1e-12);
        assert_eq!(rate("5%").unwrap(), 0.05);
        assert!(rate("2").is_err());
        assert_eq!(range("16-30").unwrap(), (16.0, 30.0));
        assert_eq!(hex("deadbeef").unwrap(), vec![0xde, 0xad, 0xbe, 0xef]);
        assert!(hex("DEADBEEF").is_err(), "lowercase only");
        assert!(hex("de:ad").is_err());
        assert!(hex_exact(32)("ab").is_err());
        assert_eq!(hex16("0x0040").unwrap(), 0x0040);
        assert!(hex16("0x40").is_err());
        assert_eq!(clock("07:30").unwrap(), 7.5);
        assert!(clock("7:30").is_err());
        assert!(clock("25:00").is_err());
        assert!(boolean("yes").is_err());
        assert!(boolean("1").is_err());
        assert!(boolean("True").is_err());
        assert_eq!(decimal("-4.5").unwrap(), -4.5);
        assert!(decimal("+4").is_err());
        assert!(decimal(".5").is_err());
        assert!(decimal("4.").is_err());
        assert!(unsigned::<u16>("08").is_err());
        assert_eq!(ident("lijeva-soba").unwrap(), "lijeva-soba");
        assert!(ident("Lijeva").is_err());
        assert!(ident("a--b").is_err());
        assert!(list(one_of(&["a", "b"]))("a  b").is_err());
        assert!(list(one_of(&["a", "b"]))("a a").is_err());
    }

    #[test]
    fn typos_are_named() {
        assert_eq!(
            suggest("tokn", &["token", "key", "port"]),
            " (did you mean \"token\"?)"
        );
        assert!(suggest("zzzz", &["token"]).contains("expected one of"));
    }

    fn doc_err(text: &str) -> String {
        match parse_document(Path::new("t.xml"), text) {
            Ok(_) => "ok".into(),
            Err(e) => e.message,
        }
    }

    #[test]
    fn documents_need_the_declaration_and_no_dtd() {
        assert!(doc_err("<blueprint/>").contains("must start with"));
        assert!(doc_err("<?xml version=\"1.1\" encoding=\"UTF-8\"?><a/>").contains("version"));
        assert!(doc_err("<?xml version=\"1.0\" encoding=\"ISO-8859-2\"?><a/>").contains("UTF-8"));
        assert!(doc_err(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><!DOCTYPE a [<!ENTITY x \"y\">]><a/>"
        )
        .contains("not well-formed"));
        assert!(
            doc_err("<?xml version=\"1.0\" encoding=\"UTF-8\"?><a><?pi x?></a>")
                .contains("processing")
        );
        assert_eq!(
            doc_err("<?xml version=\"1.0\" encoding=\"UTF-8\"?><a/>"),
            "ok"
        );
    }
}
