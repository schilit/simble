// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! AT command tokenizer and line model (ITU-T V.250 / 3GPP TS 27.007), used
//! by the Hands-Free Profile (`crate::classic::hfp`).
//!
//! [`tokenize_parameters`]/[`parse_parameters`] split a comma/parenthesis
//! separated parameter list into a tree of [`AtParameter`] values, honoring
//! double-quoted string constants (spaces inside quotes are preserved,
//! elsewhere ignored). `AtCommand` and [`AtResponse`] build on top of that
//! to parse a full line: a basic `ATxxx` command, an extended `AT+XXX=...`
//! set/read/test form, or an unsolicited/status `+XXX: ...` response.

use std::fmt;

/// Error returned when an AT command or response line cannot be tokenized or
/// parsed; wraps a human-readable description of the failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtParsingError(
    /// Human-readable description of the parse failure.
    pub(crate) String,
);

impl AtParsingError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for AtParsingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AT parsing error: {}", self.0)
    }
}

impl std::error::Error for AtParsingError {}

/// Splits `buffer` into raw tokens: bare runs of non-special characters,
/// `,`/`(`/`)` as their own single-byte tokens, and quoted strings (with the
/// surrounding quotes stripped). Spaces are dropped except inside quotes.
pub fn tokenize_parameters(buffer: &[u8]) -> Result<Vec<Vec<u8>>, AtParsingError> {
    let mut tokens: Vec<Vec<u8>> = Vec::new();
    let mut in_quotes = false;
    let mut token: Vec<u8> = Vec::new();

    for &b in buffer {
        if in_quotes {
            token.push(b);
            if b == b'"' {
                in_quotes = false;
                let inner = token[1..token.len() - 1].to_vec();
                tokens.push(inner);
                token = Vec::new();
            }
            continue;
        }
        match b {
            b' ' => {}
            b',' | b')' => {
                tokens.push(std::mem::take(&mut token));
                tokens.push(vec![b]);
            }
            b'(' => {
                if !token.is_empty() {
                    return Err(AtParsingError::new(
                        "open_paren following regular character",
                    ));
                }
                tokens.push(vec![b]);
            }
            b'"' => {
                if !token.is_empty() {
                    return Err(AtParsingError::new("quote following regular character"));
                }
                in_quotes = true;
                token.push(b);
            }
            _ => token.push(b),
        }
    }
    tokens.push(token);
    Ok(tokens.into_iter().filter(|t| !t.is_empty()).collect())
}

/// A parsed AT parameter: either a plain value or a parenthesized list of
/// parameters (used by test-form responses like `+CIND: ("call",(0-1))`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AtParameter {
    /// A single parameter value, kept as raw bytes.
    Value(Vec<u8>),
    /// A parenthesized sub-list of parameters, as in AT test-form responses.
    List(Vec<AtParameter>),
}

impl AtParameter {
    /// Returns the raw bytes of a [`AtParameter::Value`], or `None` for a list.
    pub(crate) fn as_value(&self) -> Option<&[u8]> {
        match self {
            AtParameter::Value(v) => Some(v),
            AtParameter::List(_) => None,
        }
    }

    /// Returns the value interpreted as UTF-8, or `None` for a list or invalid
    /// UTF-8.
    pub(crate) fn as_str(&self) -> Option<&str> {
        std::str::from_utf8(self.as_value()?).ok()
    }

    /// Returns the elements of a [`AtParameter::List`], or `None` for a value.
    pub fn as_list(&self) -> Option<&[AtParameter]> {
        match self {
            AtParameter::List(items) => Some(items),
            AtParameter::Value(_) => None,
        }
    }

    /// Parses the value as an integer; a present-but-empty value (as in
    /// `AT+CMER=3,,,1`'s omitted middle parameters) is not a valid integer
    /// and returns `None`, distinct from the parameter being absent.
    pub(crate) fn as_u32(&self) -> Option<u32> {
        self.as_str()?.parse().ok()
    }
}

/// Parses the comma/parenthesis-separated parameter list following an AT
/// command or response code (everything after `=`/`?`/`:`).
pub fn parse_parameters(buffer: &[u8]) -> Result<Vec<AtParameter>, AtParsingError> {
    let tokens = tokenize_parameters(buffer)?;
    let mut accumulator: Vec<Vec<AtParameter>> = vec![Vec::new()];
    let mut current = AtParameter::Value(Vec::new());

    for token in tokens {
        match token.as_slice() {
            b"," => {
                let value = std::mem::replace(&mut current, AtParameter::Value(Vec::new()));
                accumulator.last_mut().expect("non-empty").push(value);
            }
            b"(" => accumulator.push(Vec::new()),
            b")" => {
                if accumulator.len() < 2 {
                    return Err(AtParsingError::new(
                        "close_paren without matching open_paren",
                    ));
                }
                let value = std::mem::replace(&mut current, AtParameter::Value(Vec::new()));
                accumulator.last_mut().expect("non-empty").push(value);
                let list = accumulator.pop().expect("checked len >= 2");
                current = AtParameter::List(list);
            }
            _ => current = AtParameter::Value(token),
        }
    }

    accumulator.last_mut().expect("non-empty").push(current);
    if accumulator.len() > 1 {
        return Err(AtParsingError::new("missing close_paren"));
    }
    Ok(accumulator
        .into_iter()
        .next()
        .expect("non-empty")
        .into_iter()
        .collect())
}

/// Sub-form of an extended `AT+XXX` command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AtSubCode {
    /// `AT+XXX` or a basic command (`ATA`, `ATD...`): no `=`/`?` suffix.
    None,
    /// `AT+XXX=...`: set/execute form.
    Set,
    /// `AT+XXX=?`: test form (query supported values).
    Test,
    /// `AT+XXX?`: read form (query current value).
    Read,
}

/// A parsed AT command line, sent from the Hands-Free to the Audio Gateway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AtCommand {
    /// Command mnemonic without the `AT+` prefix (e.g. `BRSF`, `CIND`, or
    /// `A`/`D` for the basic `ATA`/`ATD` commands).
    pub(crate) code: String,
    /// Which extended form the line used (set, test, read, or none).
    pub(crate) sub_code: AtSubCode,
    /// Parsed parameter list following the command code.
    pub(crate) parameters: Vec<AtParameter>,
}

impl AtCommand {
    /// Parses one command line (without the trailing `\r`). `ATA` and `ATD`
    /// are special-cased (per the spec they take no `+` and, for `ATD`, an
    /// unparsed dial-string argument); every other basic/extended form must
    /// match `AT+<CODE>` followed by an optional `=?`/`=`/`?` and parameters.
    pub(crate) fn parse_from(buffer: &[u8]) -> Result<Self, AtParsingError> {
        if let Some(rest) = buffer.strip_prefix(b"AT+") {
            let code_len = rest.iter().take_while(|b| b.is_ascii_uppercase()).count();
            if code_len == 0 {
                return Err(AtParsingError::new("invalid command"));
            }
            let (code_bytes, after_code) = rest.split_at(code_len);
            let code = String::from_utf8_lossy(code_bytes).into_owned();
            let (sub_code, params_bytes) = if let Some(p) = after_code.strip_prefix(b"=?") {
                (AtSubCode::Test, p)
            } else if let Some(p) = after_code.strip_prefix(b"=") {
                (AtSubCode::Set, p)
            } else if let Some(p) = after_code.strip_prefix(b"?") {
                (AtSubCode::Read, p)
            } else {
                (AtSubCode::None, after_code)
            };
            let parameters = if params_bytes.is_empty() {
                Vec::new()
            } else {
                parse_parameters(params_bytes)?
            };
            return Ok(Self {
                code,
                sub_code,
                parameters,
            });
        }
        if buffer.starts_with(b"ATA") {
            return Ok(Self {
                code: "A".into(),
                sub_code: AtSubCode::None,
                parameters: Vec::new(),
            });
        }
        if buffer.starts_with(b"ATD") {
            return Ok(Self {
                code: "D".into(),
                sub_code: AtSubCode::None,
                parameters: vec![AtParameter::Value(buffer[3..].to_vec())],
            });
        }
        Err(AtParsingError::new("invalid command"))
    }
}

/// A parsed AT response line, sent from the Audio Gateway to the
/// Hands-Free: an unsolicited/status code (`OK`, `RING`, ...) or a
/// `+XXX: ...` result code with parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtResponse {
    /// Response/result code (e.g. `+BRSF`, `OK`, or `RING`).
    pub(crate) code: String,
    /// Parsed parameter list following a `+XXX:` result code.
    pub(crate) parameters: Vec<AtParameter>,
}

impl AtResponse {
    /// Parses one response line: an unsolicited/status code (`OK`, `RING`) or a
    /// `+XXX: ...` result code with a parsed parameter list.
    pub(crate) fn parse_from(buffer: &[u8]) -> Result<Self, AtParsingError> {
        let mut parts = buffer.splitn(2, |&b| b == b':');
        let code = parts.next().unwrap_or(b"");
        let params_bytes = parts.next().unwrap_or(b"");
        let code = String::from_utf8_lossy(code).into_owned();
        let parameters = if params_bytes.is_empty() {
            Vec::new()
        } else {
            parse_parameters(params_bytes)?
        };
        Ok(Self { code, parameters })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokenize_parameters() {
        assert_eq!(
            tokenize_parameters(b"1, 2, 3").unwrap(),
            vec![
                b"1".to_vec(),
                b",".to_vec(),
                b"2".to_vec(),
                b",".to_vec(),
                b"3".to_vec()
            ]
        );
        assert_eq!(
            tokenize_parameters(b"\"1, 2, 3\"").unwrap(),
            vec![b"1, 2, 3".to_vec()]
        );
        assert_eq!(
            tokenize_parameters(b"(1, \"2, 3\")").unwrap(),
            vec![
                b"(".to_vec(),
                b"1".to_vec(),
                b",".to_vec(),
                b"2, 3".to_vec(),
                b")".to_vec(),
            ]
        );
    }

    #[test]
    fn test_parse_parameters() {
        assert_eq!(
            parse_parameters(b"1, 2, 3").unwrap(),
            vec![
                AtParameter::Value(b"1".to_vec()),
                AtParameter::Value(b"2".to_vec()),
                AtParameter::Value(b"3".to_vec()),
            ]
        );
        assert_eq!(
            parse_parameters(b"1,, 3").unwrap(),
            vec![
                AtParameter::Value(b"1".to_vec()),
                AtParameter::Value(Vec::new()),
                AtParameter::Value(b"3".to_vec()),
            ]
        );
        assert_eq!(
            parse_parameters(b"\"1, 2, 3\"").unwrap(),
            vec![AtParameter::Value(b"1, 2, 3".to_vec())]
        );
        assert_eq!(
            parse_parameters(b"1, (2, (3))").unwrap(),
            vec![
                AtParameter::Value(b"1".to_vec()),
                AtParameter::List(vec![
                    AtParameter::Value(b"2".to_vec()),
                    AtParameter::List(vec![AtParameter::Value(b"3".to_vec())]),
                ]),
            ]
        );
        assert_eq!(
            parse_parameters(b"1, (2, \"3, 4\"), 5").unwrap(),
            vec![
                AtParameter::Value(b"1".to_vec()),
                AtParameter::List(vec![
                    AtParameter::Value(b"2".to_vec()),
                    AtParameter::Value(b"3, 4".to_vec()),
                ]),
                AtParameter::Value(b"5".to_vec()),
            ]
        );
    }

    #[test]
    fn test_parse_parameters_unbalanced_parens_rejected() {
        assert!(parse_parameters(b"1, (2, 3").is_err());
        assert!(parse_parameters(b"1, 2)").is_err());
    }

    #[test]
    fn test_at_command_basic_forms() {
        let cmd = AtCommand::parse_from(b"ATA").unwrap();
        assert_eq!(cmd.code, "A");
        assert_eq!(cmd.sub_code, AtSubCode::None);
        assert!(cmd.parameters.is_empty());

        let cmd = AtCommand::parse_from(b"ATD123456789").unwrap();
        assert_eq!(cmd.code, "D");
        assert_eq!(
            cmd.parameters,
            vec![AtParameter::Value(b"123456789".to_vec())]
        );
    }

    #[test]
    fn test_at_command_extended_forms() {
        let cmd = AtCommand::parse_from(b"AT+BRSF=31").unwrap();
        assert_eq!(cmd.code, "BRSF");
        assert_eq!(cmd.sub_code, AtSubCode::Set);
        assert_eq!(cmd.parameters, vec![AtParameter::Value(b"31".to_vec())]);

        let cmd = AtCommand::parse_from(b"AT+CIND=?").unwrap();
        assert_eq!(cmd.code, "CIND");
        assert_eq!(cmd.sub_code, AtSubCode::Test);

        let cmd = AtCommand::parse_from(b"AT+CIND?").unwrap();
        assert_eq!(cmd.code, "CIND");
        assert_eq!(cmd.sub_code, AtSubCode::Read);

        let cmd = AtCommand::parse_from(b"AT+CMER=3,,,1").unwrap();
        assert_eq!(cmd.code, "CMER");
        assert_eq!(
            cmd.parameters,
            vec![
                AtParameter::Value(b"3".to_vec()),
                AtParameter::Value(Vec::new()),
                AtParameter::Value(Vec::new()),
                AtParameter::Value(b"1".to_vec()),
            ]
        );
    }

    #[test]
    fn test_at_command_invalid_rejected() {
        assert!(AtCommand::parse_from(b"HELLO").is_err());
        assert!(AtCommand::parse_from(b"AT+").is_err());
    }

    #[test]
    fn test_at_response_parse() {
        let response = AtResponse::parse_from(b"+BRSF: 31").unwrap();
        assert_eq!(response.code, "+BRSF");
        assert_eq!(
            response.parameters,
            vec![AtParameter::Value(b"31".to_vec())]
        );

        let response = AtResponse::parse_from(b"OK").unwrap();
        assert_eq!(response.code, "OK");
        assert!(response.parameters.is_empty());
    }
}
