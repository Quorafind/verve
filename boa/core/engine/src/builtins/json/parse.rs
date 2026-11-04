//! The reader `JSON.parse` builds its values with.
//!
//! A JSON text is read once, straight from its code units into the values it describes, instead of
//! being validated by one parser and then read again by the script parser and the bytecode
//! compiler. Containers are kept on an explicit stack, so nested text costs heap, not Rust stack.
//! A reviver asks for the text each primitive came from, so the reader can also keep a
//! [`JsonNode`] tree beside the values: the text of every primitive, the members of every
//! container.

use rustc_hash::FxHashMap;

use crate::{
    Context, JsError, JsNativeError, JsResult, JsString, JsValue,
    builtins::Array,
    object::JsObject,
    string::{JsStr, JsStrVariant},
};

/// A text at most this long is quoted whole by an `Unexpected token` message.
const QUOTE_WHOLE: usize = 20;
/// Code units quoted before a token in a longer text.
const QUOTE_BEFORE: usize = 10;

/// The code units of a text, in the Latin-1 or UTF-16 form it is stored as.
struct Units<'a> {
    inner: JsStrVariant<'a>,
}

impl<'a> Units<'a> {
    fn len(&self) -> usize {
        match self.inner {
            JsStrVariant::Latin1(bytes) => bytes.len(),
            JsStrVariant::Utf16(units) => units.len(),
        }
    }

    fn at(&self, index: usize) -> u16 {
        match self.inner {
            JsStrVariant::Latin1(bytes) => u16::from(bytes[index]),
            JsStrVariant::Utf16(units) => units[index],
        }
    }

    fn slice(&self, start: usize, end: usize) -> JsStr<'a> {
        match self.inner {
            JsStrVariant::Latin1(bytes) => JsStr::latin1(&bytes[start..end]),
            JsStrVariant::Utf16(units) => JsStr::utf16(&units[start..end]),
        }
    }

    fn units(&self, start: usize, end: usize) -> Vec<u16> {
        (start..end).map(|index| self.at(index)).collect()
    }
}

/// What was read for a value, kept for the reviver of `JSON.parse`: the text a primitive came from
/// and the value it produced, and the members of a container in the order they were read.
#[derive(Debug)]
pub(super) enum JsonNode {
    Primitive {
        source: JsString,
        value: JsValue,
    },
    Array(Vec<JsonNode>),
    /// By key, so that the reviver finds a member in constant time; a repeated key keeps the
    /// last member, as the object does.
    Object(FxHashMap<JsString, JsonNode>),
}

impl Drop for JsonNode {
    /// Takes a deep tree apart on the heap: the drop glue would recurse once per level.
    fn drop(&mut self) {
        let mut pending = Vec::new();
        self.take_children(&mut pending);
        while let Some(mut node) = pending.pop() {
            node.take_children(&mut pending);
        }
    }
}

impl JsonNode {
    fn take_children(&mut self, into: &mut Vec<JsonNode>) {
        match self {
            Self::Primitive { .. } => {}
            Self::Array(children) => into.append(children),
            Self::Object(members) => into.extend(members.drain().map(|(_, node)| node)),
        }
    }
}

/// A container whose members are still being read. `nodes` stays empty unless the reader keeps
/// the tree.
enum Container {
    Array {
        elements: Vec<JsValue>,
        nodes: Vec<JsonNode>,
    },
    Object {
        object: JsObject,
        /// The key of the member being read, once it has been read and its value has not.
        key: Option<JsString>,
        nodes: FxHashMap<JsString, JsonNode>,
    },
}

pub(super) struct Reader<'a> {
    input: Units<'a>,
    pos: usize,
    /// Whether a [`JsonNode`] tree is kept beside the values.
    keep_nodes: bool,
}

impl<'a> Reader<'a> {
    pub(super) fn new(text: JsStr<'a>) -> Self {
        Self {
            input: Units {
                inner: text.variant(),
            },
            pos: 0,
            keep_nodes: false,
        }
    }

    /// Reads the whole text and answers the value it describes.
    pub(super) fn read(&mut self, context: &mut Context) -> JsResult<JsValue> {
        self.read_nodes(context).map(|(value, _)| value)
    }

    /// Reads the whole text and answers the value it describes, with the tree of what each part
    /// of it was read from.
    pub(super) fn read_with_nodes(
        &mut self,
        context: &mut Context,
    ) -> JsResult<(JsValue, JsonNode)> {
        self.keep_nodes = true;
        let (value, node) = self.read_nodes(context)?;
        Ok((value, node.expect("the tree is kept when it is asked for")))
    }

    fn read_nodes(&mut self, context: &mut Context) -> JsResult<(JsValue, Option<JsonNode>)> {
        let mut stack: Vec<Container> = Vec::new();

        loop {
            self.skip_whitespace();

            let start = self.pos;
            let mut node = None;
            let mut value = match self.peek() {
                Some(unit) if unit == u16::from(b'{') => {
                    self.pos += 1;
                    let object = JsObject::with_object_proto(context.intrinsics());
                    if self.peek_after_whitespace() == Some(u16::from(b'}')) {
                        self.pos += 1;
                        node = self
                            .keep_nodes
                            .then(|| JsonNode::Object(FxHashMap::default()));
                        object.into()
                    } else {
                        let key = self.read_key()?;
                        stack.push(Container::Object {
                            object,
                            key: Some(key),
                            nodes: FxHashMap::default(),
                        });
                        continue;
                    }
                }
                Some(unit) if unit == u16::from(b'[') => {
                    self.pos += 1;
                    if self.peek_after_whitespace() == Some(u16::from(b']')) {
                        self.pos += 1;
                        node = self.keep_nodes.then(|| JsonNode::Array(Vec::new()));
                        Array::create_array_from_list(Vec::new(), context).into()
                    } else {
                        stack.push(Container::Array {
                            elements: Vec::new(),
                            nodes: Vec::new(),
                        });
                        continue;
                    }
                }
                Some(unit) if unit == u16::from(b'"') => self.read_string()?.into(),
                Some(unit) if unit == u16::from(b't') => {
                    self.read_literal(b"true")?;
                    true.into()
                }
                Some(unit) if unit == u16::from(b'f') => {
                    self.read_literal(b"false")?;
                    false.into()
                }
                Some(unit) if unit == u16::from(b'n') => {
                    self.read_literal(b"null")?;
                    JsValue::null()
                }
                Some(unit) if unit == u16::from(b'-') || is_digit(unit) => self.read_number()?,
                Some(_) => return Err(self.unexpected_token()),
                None => return Err(Self::end_of_input()),
            };
            // Whatever is not a container is a primitive, read from `start` to here.
            if self.keep_nodes && node.is_none() {
                node = Some(JsonNode::Primitive {
                    source: self.input.slice(start, self.pos).into(),
                    value: value.clone(),
                });
            }

            // The value is complete: give it to the container it belongs to, and close every
            // container it completes.
            loop {
                let Some(container) = stack.last_mut() else {
                    self.skip_whitespace();
                    if self.pos != self.input.len() {
                        return Err(self.at("Unexpected non-whitespace character after JSON"));
                    }
                    return Ok((value, node));
                };

                match container {
                    Container::Array { elements, nodes } => {
                        elements.push(value);
                        nodes.extend(node.take());
                    }
                    Container::Object { object, key, nodes } => {
                        let key = key.take().expect("a member's key is read before its value");
                        if let Some(node) = node.take() {
                            nodes.insert(key.clone(), node);
                        }
                        object.create_data_property(key, value, context)?;
                    }
                }

                let is_array = matches!(stack.last(), Some(Container::Array { .. }));
                let separator = if is_array {
                    "Expected ',' or ']' after array element in JSON"
                } else {
                    "Expected ',' or '}' after property value in JSON"
                };

                self.skip_whitespace();
                match self.peek() {
                    Some(unit) if unit == u16::from(b',') => {
                        self.pos += 1;
                        if let Some(Container::Object { key, .. }) = stack.last_mut() {
                            self.skip_whitespace();
                            *key = Some(self.read_key()?);
                        }
                        break;
                    }
                    Some(unit) if unit == u16::from(b']') && is_array => {
                        self.pos += 1;
                        let Some(Container::Array { elements, nodes }) = stack.pop() else {
                            unreachable!("an array closes only an array")
                        };
                        node = self.keep_nodes.then_some(JsonNode::Array(nodes));
                        value = Array::create_array_from_list(elements, context).into();
                    }
                    Some(unit) if unit == u16::from(b'}') && !is_array => {
                        self.pos += 1;
                        let Some(Container::Object { object, nodes, .. }) = stack.pop() else {
                            unreachable!("an object closes only an object")
                        };
                        node = self.keep_nodes.then_some(JsonNode::Object(nodes));
                        value = object.into();
                    }
                    _ => return Err(self.at(separator)),
                }
            }
        }
    }

    /// Reads a member's key and the colon after it.
    fn read_key(&mut self) -> JsResult<JsString> {
        self.skip_whitespace();

        if self.peek() != Some(u16::from(b'"')) {
            return Err(self.at("Expected double-quoted property name in JSON"));
        }
        let key = self.read_string()?;

        self.skip_whitespace();
        if self.peek() != Some(u16::from(b':')) {
            return Err(self.at("Expected ':' after property name in JSON"));
        }
        self.pos += 1;

        Ok(key)
    }

    fn read_string(&mut self) -> JsResult<JsString> {
        self.pos += 1;
        let start = self.pos;

        while let Some(unit) = self.peek() {
            match unit {
                unit if unit == u16::from(b'"') => {
                    let end = self.pos;
                    self.pos += 1;
                    return Ok(self.input.slice(start, end).into());
                }
                unit if unit == u16::from(b'\\') => break,
                unit if unit < 0x20 => {
                    return Err(self.at("Bad control character in string literal in JSON"));
                }
                _ => self.pos += 1,
            }
        }

        // The string has an escape in it, so its code units are collected as they are read.
        let mut units = self.input.units(start, self.pos);
        loop {
            let Some(unit) = self.peek() else {
                return Err(self.at("Unterminated string in JSON"));
            };
            match unit {
                unit if unit == u16::from(b'"') => {
                    self.pos += 1;
                    return Ok(JsString::from(&units[..]));
                }
                unit if unit == u16::from(b'\\') => {
                    self.pos += 1;
                    units.push(self.read_escape()?);
                }
                unit if unit < 0x20 => {
                    return Err(self.at("Bad control character in string literal in JSON"));
                }
                _ => {
                    units.push(unit);
                    self.pos += 1;
                }
            }
        }
    }

    /// Reads the escape whose backslash has just been consumed, and answers the code unit it
    /// stands for.
    fn read_escape(&mut self) -> JsResult<u16> {
        let Some(unit) = self.peek() else {
            return Err(self.at("Unterminated string in JSON"));
        };
        self.pos += 1;

        Ok(match unit {
            unit if unit == u16::from(b'"') => u16::from(b'"'),
            unit if unit == u16::from(b'\\') => u16::from(b'\\'),
            unit if unit == u16::from(b'/') => u16::from(b'/'),
            unit if unit == u16::from(b'b') => 0x08,
            unit if unit == u16::from(b'f') => 0x0C,
            unit if unit == u16::from(b'n') => u16::from(b'\n'),
            unit if unit == u16::from(b'r') => u16::from(b'\r'),
            unit if unit == u16::from(b't') => u16::from(b'\t'),
            unit if unit == u16::from(b'u') => self.read_hex4()?,
            _ => {
                return Err(self
                    .error_at(self.pos - 1, "Bad escaped character in JSON")
                    .into());
            }
        })
    }

    fn read_hex4(&mut self) -> JsResult<u16> {
        let mut unit = 0;

        for _ in 0..4 {
            let Some(digit) = self.peek().and_then(hex_value) else {
                return Err(self.at("Bad Unicode escape in JSON"));
            };
            unit = (unit << 4) | u16::from(digit);
            self.pos += 1;
        }

        Ok(unit)
    }

    fn read_number(&mut self) -> JsResult<JsValue> {
        let start = self.pos;

        if self.peek() == Some(u16::from(b'-')) {
            self.pos += 1;
            if !self.peek().is_some_and(is_digit) {
                return Err(self.at("No number after minus sign in JSON"));
            }
        }

        match self.peek() {
            Some(unit) if unit == u16::from(b'0') => {
                self.pos += 1;
                if self.peek().is_some_and(is_digit) {
                    return Err(self.at("Unexpected number in JSON"));
                }
            }
            Some(unit) if is_digit(unit) => self.skip_digits(),
            _ => return Err(self.unexpected_token()),
        }

        if self.peek() == Some(u16::from(b'.')) {
            self.pos += 1;
            if !self.peek().is_some_and(is_digit) {
                return Err(self.at("Unterminated fractional number in JSON"));
            }
            self.skip_digits();
        }

        if matches!(self.peek(), Some(unit) if unit == u16::from(b'e') || unit == u16::from(b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(unit) if unit == u16::from(b'+') || unit == u16::from(b'-'))
            {
                self.pos += 1;
            }
            if !self.peek().is_some_and(is_digit) {
                return Err(self.at("Exponent part is missing a number in JSON"));
            }
            self.skip_digits();
        }

        let digits: Vec<u8> = self
            .input
            .units(start, self.pos)
            .into_iter()
            .map(|unit| u8::try_from(unit).expect("the number grammar is ASCII"))
            .collect();
        let digits = std::str::from_utf8(&digits).expect("the number grammar is ASCII");

        // A text of digits that fits an `i32` is an integer value, as the number literal of a
        // script is, so that what is read back out (`to_json`, the JIT's integer paths) sees an
        // integer rather than a float that happens to be whole. `-0` stays a float.
        if !digits.contains(['.', 'e', 'E'])
            && digits != "-0"
            && let Ok(integer) = digits.parse::<i32>()
        {
            return Ok(JsValue::from(integer));
        }

        Ok(JsValue::from(
            digits
                .parse::<f64>()
                .expect("the number grammar is a valid float"),
        ))
    }

    fn read_literal(&mut self, literal: &[u8]) -> JsResult<()> {
        for expected in literal {
            match self.peek() {
                Some(unit) if unit == u16::from(*expected) => self.pos += 1,
                Some(_) => return Err(self.unexpected_token()),
                None => return Err(Self::end_of_input()),
            }
        }

        Ok(())
    }

    fn skip_digits(&mut self) {
        while self.peek().is_some_and(is_digit) {
            self.pos += 1;
        }
    }

    fn skip_whitespace(&mut self) {
        while self.peek().is_some_and(is_whitespace) {
            self.pos += 1;
        }
    }

    fn peek_after_whitespace(&mut self) -> Option<u16> {
        self.skip_whitespace();
        self.peek()
    }

    fn peek(&self) -> Option<u16> {
        (self.pos < self.input.len()).then(|| self.input.at(self.pos))
    }

    fn at(&self, message: &str) -> JsError {
        self.error_at(self.pos, message).into()
    }

    /// The error for a text that ends where a value was expected, which names no position.
    fn end_of_input() -> JsError {
        JsNativeError::syntax()
            .with_message("Unexpected end of JSON input")
            .into()
    }

    /// A syntax error whose message names `position` and the line and column it falls on.
    fn error_at(&self, position: usize, message: &str) -> JsNativeError {
        let (line, column) = self.line_column(position);
        JsNativeError::syntax().with_message(format!(
            "{message} at position {position} (line {line} column {column})"
        ))
    }

    /// The error for a code unit no value starts with.
    fn unexpected_token(&self) -> JsError {
        let Some(unit) = self.peek() else {
            return Self::end_of_input();
        };

        // Node answers a text that is one of the names JSON has no value for by quoting it alone.
        let tail = String::from_utf16_lossy(&self.input.units(self.pos, self.input.len()));
        if matches!(tail.as_str(), "undefined" | "NaN" | "Infinity") {
            return JsNativeError::syntax()
                .with_message(format!("\"{tail}\" is not valid JSON"))
                .into();
        }

        JsNativeError::syntax()
            .with_message(format!(
                "Unexpected token '{}', {} is not valid JSON",
                char::from_u32(u32::from(unit)).unwrap_or(char::REPLACEMENT_CHARACTER),
                self.excerpt()
            ))
            .into()
    }

    /// The text an `Unexpected token` message quotes: the whole text when it is short, and the
    /// code units around the token when it is not.
    fn excerpt(&self) -> String {
        let len = self.input.len();
        let text = |start: usize, end: usize| {
            format!(
                "{:?}",
                String::from_utf16_lossy(&self.input.units(start, end))
            )
        };

        if len <= QUOTE_WHOLE {
            text(0, len)
        } else if self.pos < QUOTE_BEFORE {
            format!("{}...", text(0, QUOTE_BEFORE))
        } else {
            format!("...{}", text(self.pos - QUOTE_BEFORE, self.pos + 1))
        }
    }

    fn line_column(&self, position: usize) -> (usize, usize) {
        let mut line = 1;
        let mut column = 1;

        for index in 0..position {
            if self.input.at(index) == u16::from(b'\n') {
                line += 1;
                column = 1;
            } else {
                column += 1;
            }
        }

        (line, column)
    }
}

fn is_digit(unit: u16) -> bool {
    (u16::from(b'0')..=u16::from(b'9')).contains(&unit)
}

fn is_whitespace(unit: u16) -> bool {
    unit == u16::from(b' ')
        || unit == u16::from(b'\t')
        || unit == u16::from(b'\n')
        || unit == u16::from(b'\r')
}

fn hex_value(unit: u16) -> Option<u8> {
    Some(match u8::try_from(unit).ok()? {
        digit @ b'0'..=b'9' => digit - b'0',
        digit @ b'a'..=b'f' => digit - b'a' + 10,
        digit @ b'A'..=b'F' => digit - b'A' + 10,
        _ => return None,
    })
}
