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

/// How deep arrays and objects may nest: deeper text (from a broken or
/// hostile server) is refused rather than overflowing the stack.
const MAX_DEPTH: usize = 512;

pub fn parse(text: &str) -> Option<Value> {
    let mut parser = Parser { bytes: text.as_bytes(), at: 0, depth: 0 };
    let value = parser.value()?;
    parser.space();
    (parser.at == parser.bytes.len()).then_some(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    at: usize,
    depth: usize,
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
        if self.depth >= MAX_DEPTH {
            return None;
        }
        self.depth += 1;
        let value = self.value_here();
        self.depth -= 1;
        value
    }

    fn value_here(&mut self) -> Option<Value> {
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
                                if (0xdc00..0xe000).contains(&low) {
                                    code = 0x10000 + ((code - 0xd800) << 10) + (low - 0xdc00);
                                } else {
                                    // Half a pair: a replacement, then the
                                    // next escape as itself.
                                    out.push('\u{fffd}');
                                    code = low;
                                }
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

    fn string(json: &str) -> Option<String> {
        parse(json)?.str().map(str::to_owned)
    }

    #[test]
    fn every_escape_reads_as_its_character() {
        assert_eq!(string(r#""a\nb\tc\rd\be\ff\/g\\h\"i""#).as_deref(), Some("a\nb\tc\rd\u{8}e\u{c}f/g\\h\"i"));
        assert_eq!(string(r#""\u0041\u00e9\u4e2d""#).as_deref(), Some("Aé中"));
        // Upper or lower case hex, and an emoji as its surrogate pair.
        assert_eq!(string(r#""\u00C9\ud83d\ude00""#).as_deref(), Some("É😀"));
        // Half a pair alone can't be a character.
        assert_eq!(string(r#""\ud83d""#).as_deref(), Some("\u{fffd}"));
        assert_eq!(string(r#""x\ude00y""#).as_deref(), Some("x\u{fffd}y"));
    }

    #[test]
    fn half_a_surrogate_pair_doesnt_eat_the_next_escape() {
        assert_eq!(string(r#""\ud83d\u0041""#).as_deref(), Some("\u{fffd}A"));
    }

    #[test]
    fn text_in_any_script_comes_through_unescaped() {
        let value = parse(r#"{"名前": "テミノ", "emoji": "👩‍💻 ok", "mixed": "naïve café"}"#).unwrap();
        assert_eq!(value.str_at("名前"), Some("テミノ"));
        assert_eq!(value.str_at("emoji"), Some("👩‍💻 ok"));
        assert_eq!(value.str_at("mixed"), Some("naïve café"));
    }

    #[test]
    fn nested_objects_and_arrays_keep_their_order() {
        let text = " {\n \"type\" : \"assistant\" ,\"message\":{\"content\":[ {\"type\":\"text\",\"text\":\"hi\"}, [], {} ],\"usage\":{\"input_tokens\":12}},\"ok\":true,\"no\":false,\"none\":null } \n";
        let value = parse(text).unwrap();
        assert_eq!(value.str_at("type"), Some("assistant"));
        let message = value.get("message").unwrap();
        let content = message.get("content").unwrap().array();
        assert_eq!(content.len(), 3);
        assert_eq!(content[0].str_at("text"), Some("hi"));
        assert_eq!(content[1], Value::Array(Vec::new()));
        assert_eq!(content[2], Value::Object(Vec::new()));
        assert_eq!(message.get("usage").unwrap().num_at("input_tokens"), Some(12.0));
        assert!(value.get("ok").unwrap().is_true());
        assert!(!value.get("no").unwrap().is_true());
        assert_eq!(value.get("none"), Some(&Value::Null));
        let Value::Object(fields) = &value else { panic!("not an object") };
        let keys: Vec<&str> = fields.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["type", "message", "ok", "no", "none"]);
        // Asking the wrong kind of value for something gives nothing, not a panic.
        assert_eq!(value.str_at("ok"), None);
        assert_eq!(value.num_at("type"), None);
        assert!(value.str_at("type").is_some() && value.array().is_empty());
        assert_eq!(Value::Null.get("x"), None);
        // The first of two same keys wins.
        assert_eq!(parse(r#"{"a":1,"a":2}"#).unwrap().num_at("a"), Some(1.0));
        // Deep, but not absurdly so, is fine.
        let deep = format!("{}1{}", "[".repeat(500), "]".repeat(500));
        assert!(parse(&deep).is_some());
    }

    #[test]
    fn numbers_read_in_every_json_form() {
        let num = |text: &str| parse(text).and_then(|v| v.num());
        assert_eq!(num("0"), Some(0.0));
        assert_eq!(num("-12"), Some(-12.0));
        assert_eq!(num("3.25"), Some(3.25));
        assert_eq!(num("1.5e3"), Some(1500.0));
        assert_eq!(num("2E-2"), Some(0.02));
        assert_eq!(num("1e+2"), Some(100.0));
        assert_eq!(num(" 7 "), Some(7.0));
        assert_eq!(num("12345678901234567890"), Some(1.2345678901234567e19));
        for bad in ["-", "1e", "1.2.3", "--1", "1-", "e5", ".", "0x10", "NaN", "Infinity", "1 2"] {
            assert_eq!(parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn malformed_json_is_refused_without_panicking() {
        let bad = [
            "",
            " ",
            "{",
            "}",
            "[",
            "]",
            ",",
            ":",
            "\"",
            "\"abc",
            "\"\\",
            "\"\\u12",
            "\"\\u12zz\"",
            "\"\\ud83d\\u",
            "{\"a\" 1}",
            "{\"a\":}",
            "{\"a\":1,}",
            "{\"a\":1 \"b\":2}",
            "{1:2}",
            "{a:1}",
            "[1,]",
            "[1 2]",
            "[,1]",
            "tru",
            "nul",
            "falsey",
            "True",
            "{\"a\":1}}",
            "[]]",
            "\"\\é\"",
            "\u{0}",
        ];
        for text in bad {
            assert_eq!(parse(text), None, "{text:?}");
        }
        // Nor does any cut-short piece of a good record.
        let good = r#"{"a":[1,-2.5e1,{"b":"x\"y\u00e9😀"}],"c":null,"d":true}"#;
        assert!(parse(good).is_some());
        for end in (0..good.len()).filter(|&end| good.is_char_boundary(end)) {
            assert_eq!(parse(&good[..end]), None, "{:?}", &good[..end]);
        }
    }

    #[test]
    fn very_deep_nesting_is_refused_instead_of_crashing() {
        let deep = "[".repeat(1_000_000);
        assert_eq!(parse(&deep), None);
    }
}
