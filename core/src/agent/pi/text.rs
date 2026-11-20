use serde::de::IgnoredAny;

/// Scan the model's JSON inside Pi's decoded text deltas. The final response is parsed separately.
#[derive(Default)]
pub(super) struct AnswerText {
    input: String,
    offset: usize,
    state: State,
    boundary: Boundary,
}

#[derive(Default)]
enum State {
    #[default]
    Start,
    Fence,
    Object,
    Key,
    SkipValue,
    TextStart,
    Text,
    Done,
}

impl AnswerText {
    pub(super) fn finished() -> Self {
        Self {
            state: State::Done,
            ..Self::default()
        }
    }

    pub(super) fn push(&mut self, delta: &str) -> String {
        if matches!(self.state, State::Done) {
            return String::new();
        }
        self.input.push_str(delta);
        let mut text = String::new();
        while self.offset < self.input.len() {
            let remaining = &self.input[self.offset..];
            match self.state {
                State::Start | State::Object => {
                    let trimmed = remaining.trim_start();
                    self.offset += remaining.len() - trimmed.len();
                    if trimmed.is_empty() {
                        break;
                    } else if trimmed.starts_with('{') {
                        self.offset += 1;
                        self.state = State::Key;
                    } else if matches!(self.state, State::Start) && trimmed.starts_with("```") {
                        self.offset += 3;
                        self.state = State::Fence;
                    } else if matches!(self.state, State::Start) && "```".starts_with(trimmed) {
                        break;
                    } else {
                        self.state = State::Done;
                    }
                }
                State::Fence => {
                    if remaining.starts_with("json") {
                        self.offset += 4;
                    } else if "json".starts_with(remaining) {
                        break;
                    }
                    self.state = State::Object;
                }
                State::Key => {
                    let Some(end) = self.boundary.find(remaining, b":}") else {
                        break;
                    };
                    self.state = match serde_json::from_str::<String>(&remaining[..end]) {
                        Ok(key) if remaining.as_bytes()[end] == b':' => {
                            if key == "text" {
                                State::TextStart
                            } else {
                                State::SkipValue
                            }
                        }
                        _ => State::Done,
                    };
                    self.offset += end + 1;
                }
                State::SkipValue => {
                    let Some(end) = self.boundary.find(remaining, b",}") else {
                        break;
                    };
                    self.state = if remaining.as_bytes()[end] == b','
                        && serde_json::from_str::<IgnoredAny>(&remaining[..end]).is_ok()
                    {
                        State::Key
                    } else {
                        State::Done
                    };
                    self.offset += end + 1;
                }
                State::TextStart => {
                    let trimmed = remaining.trim_start_matches([' ', '\n', '\r', '\t']);
                    self.offset += remaining.len() - trimmed.len();
                    if trimmed.is_empty() {
                        break;
                    } else if trimmed.starts_with('"') {
                        self.offset += 1;
                        self.state = State::Text;
                    } else {
                        self.state = State::Done;
                    }
                }
                State::Text => {
                    let end = remaining
                        .find(|character| matches!(character, '"' | '\\' | '\0'..='\x1f'))
                        .unwrap_or(remaining.len());
                    text.push_str(&remaining[..end]);
                    self.offset += end;
                    let remaining = &remaining[end..];
                    if remaining.is_empty() {
                        break;
                    } else if !remaining.starts_with('\\') {
                        self.state = State::Done;
                    } else if let Some(length) = escape_length(remaining) {
                        let mut quoted = [b'"'; 14];
                        quoted[1..=length].copy_from_slice(&remaining.as_bytes()[..length]);
                        match serde_json::from_slice::<String>(&quoted[..length + 2]) {
                            Ok(decoded) => {
                                text.push_str(&decoded);
                                self.offset += length;
                            }
                            Err(_) => self.state = State::Done,
                        }
                    } else {
                        break;
                    }
                }
                State::Done => break,
            }
        }
        if matches!(self.state, State::Done) {
            *self = Self::finished();
        } else if matches!(self.state, State::Text) {
            self.input.drain(..self.offset);
            self.offset = 0;
        }
        text
    }
}

fn escape_length(input: &str) -> Option<usize> {
    let bytes = input.as_bytes();
    let length = match bytes.get(1)? {
        b'u' => {
            if bytes.len() < 6 {
                return None;
            }
            let code = input
                .get(2..6)
                .and_then(|hex| u16::from_str_radix(hex, 16).ok());
            if code.is_some_and(|code| (0xd800..=0xdbff).contains(&code)) {
                12
            } else {
                6
            }
        }
        _ => 2,
    };
    (bytes.len() >= length).then_some(length)
}

/// Find a delimiter outside quoted strings and nested values, resuming at the next byte.
#[derive(Default)]
struct Boundary {
    offset: usize,
    depth: usize,
    quoted: bool,
    escaped: bool,
}

impl Boundary {
    fn find(&mut self, input: &str, delimiters: &[u8]) -> Option<usize> {
        for (offset, &byte) in input.as_bytes()[self.offset..].iter().enumerate() {
            if self.quoted {
                if self.escaped {
                    self.escaped = false;
                } else if byte == b'\\' {
                    self.escaped = true;
                } else if byte == b'"' {
                    self.quoted = false;
                }
            } else if self.depth == 0 && delimiters.contains(&byte) {
                let end = self.offset + offset;
                *self = Self::default();
                return Some(end);
            } else {
                match byte {
                    b'"' => self.quoted = true,
                    b'{' | b'[' => self.depth += 1,
                    b'}' | b']' => self.depth = self.depth.saturating_sub(1),
                    _ => {}
                }
            }
        }
        self.offset = input.len();
        None
    }
}

#[cfg(test)]
mod tests {
    use super::AnswerText;

    #[test]
    fn streams_top_level_text_with_every_character_fragmented() {
        let input = r#" ```json
{"recommendations":[{"text":"nested,}:\" secret"}], "gaps": ["other"], "te\u0078t":"中文😀 \uD83D\uDE80\\\n\t\u00e9\"\/fin"}
```"#;
        let mut parser = AnswerText::default();
        let output: String = input
            .chars()
            .map(|character| parser.push(character.encode_utf8(&mut [0; 4])))
            .collect();
        assert_eq!(output, "中文😀\u{2028}🚀\\\n\té\"/fin");
    }

    #[test]
    fn rejects_non_answers_and_incomplete_escapes() {
        for input in [
            r#"unstructured {"text":"private"}"#,
            r#"[{"text":"private"}]"#,
            r#"{"gaps":"private", "text":{"text":"private"}}"#,
            r#"{"gaps" invalid: [], "text":"private"}"#,
            r#"{"gaps":[} unrelated, "text":"private"}"#,
        ] {
            assert_eq!(AnswerText::default().push(input), "", "{input}");
        }
        let mut parser = AnswerText::default();
        assert_eq!(parser.push(r#"{"text":"visible\uD800"#), "visible");
        assert_eq!(parser.push(r#"\u00e9 PRIVATE"#), "");
        assert_eq!(parser.push(r#"","text":"PRIVATE"}"#), "");
    }
}
