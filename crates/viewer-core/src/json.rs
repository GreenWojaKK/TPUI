//! JSON documents read as written: object keys keep their order and duplicates, numbers keep their text, and every
//! node points back at its bytes in the file. Parsing is iterative, so nesting depth is limited only by memory.

use std::borrow::Cow;

/// Largest file a document can address, since nodes hold 32-bit byte offsets.
const MAX_BYTES: usize = u32::MAX as usize;
/// Marks a node without a key.
const NO_KEY: (u32, u32) = (u32::MAX, u32::MAX);
/// Raw bytes kept per requested preview character: `\uXXXX` is the longest form of one character.
const RAW_BYTES_PER_CHAR: usize = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsonKind {
    Null,
    Bool,
    Number,
    String,
    Array,
    Object,
}

impl JsonKind {
    pub fn is_container(self) -> bool {
        matches!(self, Self::Array | Self::Object)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Bool => "boolean",
            Self::Number => "number",
            Self::String => "string",
            Self::Array => "array",
            Self::Object => "object",
        }
    }
}

/// A node of a [`JsonDocument`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct JsonNodeId(u32);

#[derive(Clone, Debug)]
struct Node {
    kind: JsonKind,
    /// Bytes of the key between its quotes.
    key: (u32, u32),
    /// Bytes of the value. Strings include their quotes.
    value: (u32, u32),
    /// Start and count of the node's entries in `JsonDocument::children`.
    children: (u32, u32),
}

/// Why a text is not valid JSON, and the place where that shows. Line and character column are one-based.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsonError {
    pub offset: usize,
    pub line: usize,
    pub column: usize,
    pub reason: &'static str,
}

/// A parsed JSON file. Holds the file's text and one small record per value; strings are decoded when asked for.
#[derive(Clone, Debug)]
pub struct JsonDocument {
    text: String,
    nodes: Vec<Node>,
    children: Vec<u32>,
}

impl JsonDocument {
    /// Parses a complete JSON text as defined by RFC 8259.
    pub fn parse(text: String) -> Result<Self, JsonError> {
        let located = |text: &str, (offset, reason)| {
            let (line, column) = line_column(text, offset);
            JsonError {
                offset,
                line,
                column,
                reason,
            }
        };
        if text.len() > MAX_BYTES {
            let failure = (0, "the file is larger than 4 GiB");
            return Err(located(&text, failure));
        }
        let mut parser = Parser {
            bytes: text.as_bytes(),
            pos: 0,
            nodes: Vec::new(),
            children: Vec::new(),
            pending: Vec::new(),
            stack: Vec::new(),
        };
        if let Err(failure) = parser.parse() {
            return Err(located(&text, failure));
        }
        let (nodes, children) = (parser.nodes, parser.children);
        Ok(Self {
            text,
            nodes,
            children,
        })
    }

    pub fn root(&self) -> JsonNodeId {
        JsonNodeId(0)
    }

    /// Number of values in the document, at every depth.
    pub fn value_count(&self) -> usize {
        self.nodes.len()
    }

    fn node(&self, id: JsonNodeId) -> &Node {
        &self.nodes[id.0 as usize]
    }

    pub fn kind(&self, id: JsonNodeId) -> JsonKind {
        self.node(id).kind
    }

    /// The node's bytes exactly as they stand in the file. Strings include their quotes.
    pub fn raw(&self, id: JsonNodeId) -> &str {
        let (start, end) = self.node(id).value;
        &self.text[start as usize..end as usize]
    }

    /// The decoded key of an object member. Elements of arrays and the root have none.
    pub fn key(&self, id: JsonNodeId) -> Option<Cow<'_, str>> {
        let (start, end) = self.node(id).key;
        (start != NO_KEY.0).then(|| decode(&self.text[start as usize..end as usize]))
    }

    pub fn child_count(&self, id: JsonNodeId) -> usize {
        self.node(id).children.1 as usize
    }

    pub fn child(&self, id: JsonNodeId, ix: usize) -> Option<JsonNodeId> {
        let (start, count) = self.node(id).children;
        (ix < count as usize).then(|| JsonNodeId(self.children[start as usize + ix]))
    }

    /// The node reached by following child positions from the root.
    pub fn node_at(&self, path: &[usize]) -> Option<JsonNodeId> {
        path.iter()
            .try_fold(self.root(), |node, &ix| self.child(node, ix))
    }

    /// The value as a text without quotes or escapes: the decoded string for strings, the text in the file otherwise.
    pub fn text(&self, id: JsonNodeId) -> Cow<'_, str> {
        let raw = self.raw(id);
        match self.kind(id) {
            JsonKind::String => decode(&raw[1..raw.len() - 1]),
            _ => Cow::Borrowed(raw),
        }
    }

    /// The beginning of a string value, decoded, without reading the rest of a long string. `None` for other kinds.
    /// The second value tells whether the string goes on.
    pub fn string_preview(&self, id: JsonNodeId, max_chars: usize) -> Option<(String, bool)> {
        if self.kind(id) != JsonKind::String {
            return None;
        }
        let raw = self.raw(id);
        let inner = &raw[1..raw.len() - 1];
        let mut end = inner
            .len()
            .min(max_chars.saturating_mul(RAW_BYTES_PER_CHAR));
        while !inner.is_char_boundary(end) {
            end -= 1;
        }
        let cut = end < inner.len();
        let decoded = decode(&inner[..end]);
        let mut chars = decoded.chars();
        let preview: String = chars.by_ref().take(max_chars).collect();
        Some((preview, cut || chars.next().is_some()))
    }
}

/// One-based line and character column of a byte in `text`.
fn line_column(text: &str, offset: usize) -> (usize, usize) {
    let mut end = offset.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let before = &text[..end];
    let line = before.bytes().filter(|&byte| byte == b'\n').count() + 1;
    let start = before.rfind('\n').map_or(0, |newline| newline + 1);
    (line, before[start..].chars().count() + 1)
}

/// Resolves the escapes of a string's inner text. A surrogate that is not half of a pair becomes U+FFFD, and an
/// escape cut off at the end of the text is dropped.
fn decode(inner: &str) -> Cow<'_, str> {
    let Some(first) = inner.find('\\') else {
        return Cow::Borrowed(inner);
    };
    let mut out = String::with_capacity(inner.len());
    out.push_str(&inner[..first]);
    let mut rest = &inner[first..];
    while let Some(backslash) = rest.find('\\') {
        out.push_str(&rest[..backslash]);
        rest = &rest[backslash + 1..];
        let Some(escape) = rest.chars().next() else {
            return Cow::Owned(out);
        };
        rest = &rest[escape.len_utf8()..];
        match escape {
            'b' => out.push('\u{8}'),
            'f' => out.push('\u{c}'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'u' => {
                let Some(unit) = hex4(rest) else {
                    return Cow::Owned(out);
                };
                rest = &rest[4..];
                if (0xD800..0xDC00).contains(&unit)
                    && let Some(low) = rest.strip_prefix("\\u").and_then(hex4)
                    && (0xDC00..0xE000).contains(&low)
                {
                    rest = &rest[6..];
                    let scalar = 0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00);
                    out.push(char::from_u32(scalar).unwrap_or(char::REPLACEMENT_CHARACTER));
                } else {
                    out.push(char::from_u32(unit).unwrap_or(char::REPLACEMENT_CHARACTER));
                }
            }
            other => out.push(other),
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

fn hex4(text: &str) -> Option<u32> {
    let digits = text.get(..4)?;
    digits
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit())
        .then(|| u32::from_str_radix(digits, 16).ok())
        .flatten()
}

/// A parse failure: the byte where it shows, and why.
type Failure = (usize, &'static str);

/// An array or object whose members are being read.
struct Frame {
    node: u32,
    /// Where this container's finished members start in `Parser::pending`.
    pending_start: usize,
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
    nodes: Vec<Node>,
    children: Vec<u32>,
    /// Finished members of the open containers, innermost last.
    pending: Vec<u32>,
    stack: Vec<Frame>,
}

impl Parser<'_> {
    fn error<T>(&self, reason: &'static str) -> Result<T, Failure> {
        Err((self.pos, reason))
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn parse(&mut self) -> Result<(), Failure> {
        let mut key = NO_KEY;
        loop {
            let mut finished = self.parse_value(key)?;
            // Close every container this value completes, until one expects another member.
            loop {
                let Some(frame) = self.stack.last() else {
                    self.skip_whitespace();
                    if self.pos < self.bytes.len() {
                        return self.error("unexpected text after the value");
                    }
                    return Ok(());
                };
                let object = self.nodes[frame.node as usize].kind == JsonKind::Object;
                self.pending.push(finished);
                self.skip_whitespace();
                match self.peek() {
                    Some(b',') => {
                        self.pos += 1;
                        key = if object { self.parse_key()? } else { NO_KEY };
                        break;
                    }
                    Some(b'}') if object => finished = self.close_container(),
                    Some(b']') if !object => finished = self.close_container(),
                    None => return self.error("the text ends inside a container"),
                    _ if object => return self.error("expected `,` or `}`"),
                    _ => return self.error("expected `,` or `]`"),
                }
            }
        }
    }

    /// Reads one value. A container without members is finished at once; another one stays open with its first
    /// member read next, so the node returned is that member's.
    fn parse_value(&mut self, key: (u32, u32)) -> Result<u32, Failure> {
        let mut key = key;
        loop {
            self.skip_whitespace();
            let start = self.pos;
            let kind = match self.peek() {
                None => return self.error("the text ends where a value should start"),
                Some(b'{') => JsonKind::Object,
                Some(b'[') => JsonKind::Array,
                Some(b'"') => {
                    self.parse_string()?;
                    return Ok(self.push_node(JsonKind::String, key, start));
                }
                Some(b't') => return self.parse_literal("true", JsonKind::Bool, key),
                Some(b'f') => return self.parse_literal("false", JsonKind::Bool, key),
                Some(b'n') => return self.parse_literal("null", JsonKind::Null, key),
                Some(b'-' | b'0'..=b'9') => {
                    self.parse_number()?;
                    return Ok(self.push_node(JsonKind::Number, key, start));
                }
                Some(_) => return self.error("expected a value"),
            };
            self.pos += 1;
            let node = self.push_node(kind, key, start);
            self.stack.push(Frame {
                node,
                pending_start: self.pending.len(),
            });
            self.skip_whitespace();
            match (kind, self.peek()) {
                (JsonKind::Object, Some(b'}')) | (JsonKind::Array, Some(b']')) => {
                    return Ok(self.close_container());
                }
                (JsonKind::Object, _) => key = self.parse_key()?,
                _ => key = NO_KEY,
            }
        }
    }

    fn push_node(&mut self, kind: JsonKind, key: (u32, u32), start: usize) -> u32 {
        let id = self.nodes.len() as u32;
        self.nodes.push(Node {
            kind,
            key,
            value: (start as u32, self.pos as u32),
            children: (0, 0),
        });
        id
    }

    /// Ends the innermost container at its closing bracket and moves its members to their final place.
    fn close_container(&mut self) -> u32 {
        self.pos += 1;
        let frame = self.stack.pop().expect("a container is open");
        let start = self.children.len() as u32;
        let count = (self.pending.len() - frame.pending_start) as u32;
        self.children
            .extend(self.pending.drain(frame.pending_start..));
        let node = &mut self.nodes[frame.node as usize];
        node.value.1 = self.pos as u32;
        node.children = (start, count);
        frame.node
    }

    /// Reads `"key" :` and returns the bytes between the quotes.
    fn parse_key(&mut self) -> Result<(u32, u32), Failure> {
        self.skip_whitespace();
        if self.peek() != Some(b'"') {
            return self.error("expected a key in quotes");
        }
        let start = self.pos;
        self.parse_string()?;
        let key = (start as u32 + 1, self.pos as u32 - 1);
        self.skip_whitespace();
        if self.peek() != Some(b':') {
            return self.error("expected `:` after the key");
        }
        self.pos += 1;
        Ok(key)
    }

    fn parse_string(&mut self) -> Result<(), Failure> {
        self.pos += 1;
        loop {
            match self.peek() {
                None => return self.error("this string is not closed"),
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(());
                }
                Some(b'\\') => {
                    self.pos += 1;
                    match self.peek() {
                        Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => {
                            self.pos += 1;
                        }
                        Some(b'u') => {
                            self.pos += 1;
                            for _ in 0..4 {
                                if !self.peek().is_some_and(|byte| byte.is_ascii_hexdigit()) {
                                    return self.error("`\\u` needs four hexadecimal digits");
                                }
                                self.pos += 1;
                            }
                        }
                        None => return self.error("this string is not closed"),
                        Some(_) => return self.error("unknown escape in a string"),
                    }
                }
                Some(0..0x20) => return self.error("control character in a string"),
                Some(_) => self.pos += 1,
            }
        }
    }

    fn parse_literal(
        &mut self,
        literal: &'static str,
        kind: JsonKind,
        key: (u32, u32),
    ) -> Result<u32, Failure> {
        if !self.bytes[self.pos..].starts_with(literal.as_bytes()) {
            return self.error("expected a value");
        }
        let start = self.pos;
        self.pos += literal.len();
        Ok(self.push_node(kind, key, start))
    }

    fn skip_digits(&mut self) -> bool {
        let start = self.pos;
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.pos += 1;
        }
        self.pos > start
    }

    fn parse_number(&mut self) -> Result<(), Failure> {
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.peek() {
            Some(b'0') => {
                self.pos += 1;
                if self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                    return self.error("a number cannot start with 0");
                }
            }
            Some(b'1'..=b'9') => {
                self.skip_digits();
            }
            _ => return self.error("expected a digit"),
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            if !self.skip_digits() {
                return self.error("expected a digit after the decimal point");
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if !self.skip_digits() {
                return self.error("expected a digit in the exponent");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> JsonDocument {
        JsonDocument::parse(text.to_string()).unwrap()
    }

    fn error(text: &str) -> JsonError {
        JsonDocument::parse(text.to_string()).unwrap_err()
    }

    #[test]
    fn keeps_key_order_and_duplicates() {
        let doc = parse(r#"{"z":1,"a":2,"z":3}"#);
        let root = doc.root();
        let keys: Vec<_> = (0..doc.child_count(root))
            .map(|ix| doc.key(doc.child(root, ix).unwrap()).unwrap().into_owned())
            .collect();
        assert_eq!(keys, ["z", "a", "z"]);
        assert_eq!(doc.raw(doc.child(root, 2).unwrap()), "3");
    }

    #[test]
    fn keeps_the_text_of_numbers() {
        let doc = parse("[1.10, 1e3, 123456789012345678901234567890, -0.0]");
        let texts: Vec<_> = (0..4)
            .map(|ix| doc.text(doc.child(doc.root(), ix).unwrap()).into_owned())
            .collect();
        assert_eq!(
            texts,
            ["1.10", "1e3", "123456789012345678901234567890", "-0.0"]
        );
    }

    #[test]
    fn decodes_strings_when_asked() {
        let doc = parse(r#"["줄\n바꿈\t\"따옴표\" \u00e9 \uD83D\uDE00 \uD83D x"]"#);
        let node = doc.child(doc.root(), 0).unwrap();
        assert_eq!(doc.text(node), "줄\n바꿈\t\"따옴표\" é 😀 \u{fffd} x");
        assert!(doc.raw(node).starts_with(r#""줄\n"#));
    }

    #[test]
    fn previews_the_beginning_of_a_string() {
        let doc = parse(r#"["가나다\n라", "ab"]"#);
        let node = doc.child(doc.root(), 0).unwrap();
        assert_eq!(doc.string_preview(node, 3), Some(("가나다".into(), true)));
        assert_eq!(
            doc.string_preview(node, 5),
            Some(("가나다\n라".into(), false))
        );
        let short = doc.child(doc.root(), 1).unwrap();
        assert_eq!(doc.string_preview(short, 5), Some(("ab".into(), false)));
        assert_eq!(doc.string_preview(doc.root(), 5), None);
        // An escape cut off by the limit is dropped, not shown as raw text.
        let doc = parse(r#"["\u00e9\u00e9"]"#);
        let node = doc.child(doc.root(), 0).unwrap();
        assert_eq!(doc.string_preview(node, 1), Some(("é".into(), true)));
    }

    #[test]
    fn addresses_nodes_by_path() {
        let doc = parse(r#"{"a":[{"b":null},[]],"c":{}}"#);
        let node = doc.node_at(&[0, 0, 0]).unwrap();
        assert_eq!(doc.kind(node), JsonKind::Null);
        assert_eq!(doc.key(node).as_deref(), Some("b"));
        assert_eq!(doc.raw(doc.node_at(&[0]).unwrap()), r#"[{"b":null},[]]"#);
        assert_eq!(doc.child_count(doc.node_at(&[0, 1]).unwrap()), 0);
        assert_eq!(doc.child_count(doc.node_at(&[1]).unwrap()), 0);
        assert!(doc.node_at(&[0, 5]).is_none());
        assert_eq!(doc.value_count(), 6);
    }

    #[test]
    fn reads_nesting_deeper_than_any_stack() {
        let depth = 200_000;
        let text = format!("{}1{}", "[".repeat(depth), "]".repeat(depth));
        let doc = parse(&text);
        assert_eq!(doc.value_count(), depth + 1);
    }

    #[test]
    fn scalars_can_be_the_root() {
        for (text, kind) in [
            ("null", JsonKind::Null),
            (" true ", JsonKind::Bool),
            ("-1.5e+2", JsonKind::Number),
            ("\"\"", JsonKind::String),
            ("[]", JsonKind::Array),
            ("{}", JsonKind::Object),
        ] {
            assert_eq!(parse(text).kind(parse(text).root()), kind);
        }
    }

    #[test]
    fn rejects_invalid_text_with_the_place() {
        for (text, offset, reason) in [
            ("", 0, "the text ends where a value should start"),
            ("[1,]", 3, "expected a value"),
            ("{\"a\":1,}", 7, "expected a key in quotes"),
            ("{\"a\" 1}", 5, "expected `:` after the key"),
            ("[1 2]", 3, "expected `,` or `]`"),
            ("{\"a\":1 \"b\":2}", 7, "expected `,` or `}`"),
            ("[1", 2, "the text ends inside a container"),
            ("\"abc", 4, "this string is not closed"),
            ("\"a\nb\"", 2, "control character in a string"),
            ("\"\\x\"", 2, "unknown escape in a string"),
            ("\"\\u12\"", 5, "`\\u` needs four hexadecimal digits"),
            ("01", 1, "a number cannot start with 0"),
            ("1.", 2, "expected a digit after the decimal point"),
            ("1e", 2, "expected a digit in the exponent"),
            ("-", 1, "expected a digit"),
            ("nul", 0, "expected a value"),
            ("1 2", 2, "unexpected text after the value"),
            ("// note\n1", 0, "expected a value"),
        ] {
            let found = error(text);
            assert_eq!((found.offset, found.reason), (offset, reason), "{text}");
        }
    }

    #[test]
    fn finds_lines_and_columns() {
        let text = "{\n  \"가\": x\n}";
        let at = text.find('x').unwrap();
        assert_eq!(line_column(text, at), (2, 8));
        assert_eq!(line_column(text, 0), (1, 1));
        assert_eq!(line_column(text, text.len()), (3, 2));
    }
}
