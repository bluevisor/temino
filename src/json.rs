//! Just enough JSON to read the files coding agents leave about: their
//! session records and transcripts.

#[derive(Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn array(&self) -> &[Value] {
        match self {
            Value::Array(items) => items,
            _ => &[],
        }
    }

    pub fn is_true(&self) -> bool {
        *self == Value::Bool(true)
    }

    pub fn num(&self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(*n),
            _ => None,
        }
    }

    /// The number at `key`, if there is one.
    pub fn num_at(&self, key: &str) -> Option<f64> {
        self.get(key).and_then(Value::num)
    }

    /// The string at `key`, if there is one.
    pub fn str_at(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(Value::str)
    }
}

pub fn parse(text: &str) -> Option<Value> {
    let mut parser = Parser { bytes: text.as_bytes(), at: 0 };
    let value = parser.value()?;
    parser.space();
    (parser.at == parser.bytes.len()).then_some(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn space(&mut self) {
        while self.bytes.get(self.at).is_some_and(|b| b.is_ascii_whitespace()) {
            self.at += 1;
        }
    }

    fn eat(&mut self, word: &str) -> bool {
        let found = self.bytes[self.at..].starts_with(word.as_bytes());
        if found {
            self.at += word.len();
        }
        found
    }

    fn value(&mut self) -> Option<Value> {
        self.space();
        match *self.bytes.get(self.at)? {
            b'{' => {
                self.at += 1;
                let mut fields = Vec::new();
                self.space();
                if self.eat("}") {
                    return Some(Value::Object(fields));
                }
                loop {
                    self.space();
                    let key = self.string()?;
                    self.space();
                    if !self.eat(":") {
                        return None;
                    }
                    fields.push((key, self.value()?));
                    self.space();
                    if self.eat("}") {
                        return Some(Value::Object(fields));
                    }
                    if !self.eat(",") {
                        return None;
                    }
                }
            }
            b'[' => {
                self.at += 1;
                let mut items = Vec::new();
                self.space();
                if self.eat("]") {
                    return Some(Value::Array(items));
                }
                loop {
                    items.push(self.value()?);
                    self.space();
                    if self.eat("]") {
                        return Some(Value::Array(items));
                    }
                    if !self.eat(",") {
                        return None;
                    }
                }
            }
            b'"' => self.string().map(Value::String),
            b't' if self.eat("true") => Some(Value::Bool(true)),
            b'f' if self.eat("false") => Some(Value::Bool(false)),
            b'n' if self.eat("null") => Some(Value::Null),
            _ => {
                let start = self.at;
                while self.bytes.get(self.at).is_some_and(|b| b"+-.eE0123456789".contains(b)) {
                    self.at += 1;
                }
                std::str::from_utf8(&self.bytes[start..self.at]).ok()?.parse().ok().map(Value::Number)
            }
        }
    }

    fn string(&mut self) -> Option<String> {
        if !self.eat("\"") {
            return None;
        }
        let mut out = String::new();
        loop {
            // Copy the run up to the next quote or escape in one go.
            let start = self.at;
            while self.bytes.get(self.at).is_some_and(|&b| b != b'"' && b != b'\\') {
                self.at += 1;
            }
            out.push_str(std::str::from_utf8(&self.bytes[start..self.at]).ok()?);
            match *self.bytes.get(self.at)? {
                b'"' => {
                    self.at += 1;
                    return Some(out);
                }
                _ => {
                    self.at += 1;
                    let escape = *self.bytes.get(self.at)?;
                    self.at += 1;
                    match escape {
                        b'n' => out.push('\n'),
                        b't' => out.push('\t'),
                        b'r' => out.push('\r'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'u' => {
                            let mut code = self.hex4()?;
                            if (0xd800..0xdc00).contains(&code) && self.eat("\\u") {
                                let low = self.hex4()?;
                                code = 0x10000 + ((code - 0xd800) << 10) + (low.wrapping_sub(0xdc00) & 0x3ff);
                            }
                            out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                        }
                        other => out.push(other as char),
                    }
                }
            }
        }
    }

    fn hex4(&mut self) -> Option<u32> {
        let digits = std::str::from_utf8(self.bytes.get(self.at..self.at + 4)?).ok()?;
        self.at += 4;
        u32::from_str_radix(digits, 16).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_nested_values_and_escapes() {
        let value = parse(r#"{"a":[1,-2.5e1,true,null],"b":{"c":"x\"y\né😀"}}"#).unwrap();
        assert_eq!(value.get("a").unwrap().array().len(), 4);
        assert_eq!(value.get("b").unwrap().str_at("c"), Some("x\"y\né😀"));
        assert!(parse("{\"a\":").is_none());
        assert!(parse("[1] x").is_none());
    }
}
