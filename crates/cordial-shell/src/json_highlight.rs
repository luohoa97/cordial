//! Colouring for the FastFlags editor, as a function over text.
//!
//! **Why this is hand-written rather than GtkSourceView.** SourceView is the
//! obvious answer and it is one `pkg-config` away from being impossible:
//! `gtksourceview-5` is on neither this host nor the container the packages are
//! built in, so adopting it adds a runtime dependency to the deb, the rpm, the
//! Arch package, the AppImage and the Flatpak manifest -- five packaging files
//! and a new failure mode on every distribution -- to colour one text box.
//! A FastFlags document is a flat JSON object of a few hundred lines at the
//! outside, which a scanner handles in microseconds.
//!
//! The scanner is deliberately separated from the widget so it can be tested
//! without a display. What GTK gets from it is a list of ranges and what each
//! one is; choosing the colours and applying the tags is the settings window's
//! business.
//!
//! Offsets are in **characters, not bytes**, because that is what
//! `TextBuffer::iter_at_offset` takes. Getting that wrong is invisible until
//! somebody pastes a flag value containing a non-ASCII character, at which
//! point every colour after it slides.

/// What a run of characters is, for colouring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token {
    /// A string in key position — the flag name. Worth its own colour because
    /// it is the thing anybody scanning this document is looking for.
    Key,
    Str,
    Number,
    /// `true`, `false`, `null`. Bare, not quoted: Roblox's own documents spell
    /// booleans as the strings `"True"` and `"False"`, so a bare `true` here is
    /// usually somebody writing JSON rather than a FastFlag, and colouring it
    /// differently from a string is a hint that it will not do what they meant.
    Keyword,
    Punct,
}

/// Every coloured run in `text`, in order, as `(start, end, kind)` character
/// offsets.
///
/// Never fails. A half-typed document is the normal case in a live editor, so
/// an unterminated string runs to the end of the text and anything the scanner
/// does not recognise is simply left uncoloured rather than aborting the pass —
/// colouring is not validation, and the status line does that job.
pub fn spans(text: &str) -> Vec<(usize, usize, Token)> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        match c {
            '"' => {
                let start = i;
                i += 1;
                while i < chars.len() {
                    // A backslash escapes the next character, so a `\"` does
                    // not end the string. Without this a value like
                    // "say \"hi\"" ends early and the rest of the document
                    // colours as though it were inside a string.
                    if chars[i] == '\\' {
                        i += 2;
                        continue;
                    }
                    if chars[i] == '"' {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                let end = i.min(chars.len());
                // Key or value is decided by what follows, not by nesting
                // depth: the next non-space character being a colon is what
                // makes a string a key, and that is true at any depth.
                let mut j = end;
                while j < chars.len() && chars[j].is_whitespace() {
                    j += 1;
                }
                let kind = if chars.get(j) == Some(&':') { Token::Key } else { Token::Str };
                out.push((start, end, kind));
            }
            '{' | '}' | '[' | ']' | ',' | ':' => {
                out.push((i, i + 1, Token::Punct));
                i += 1;
            }
            '-' | '0'..='9' => {
                let start = i;
                i += 1;
                while i < chars.len() && matches!(chars[i], '0'..='9' | '.' | 'e' | 'E' | '+' | '-')
                {
                    i += 1;
                }
                out.push((start, i, Token::Number));
            }
            'a'..='z' | 'A'..='Z' => {
                let start = i;
                while i < chars.len() && chars[i].is_ascii_alphabetic() {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                if matches!(word.as_str(), "true" | "false" | "null") {
                    out.push((start, i, Token::Keyword));
                }
            }
            _ => i += 1,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<(&str, Token)> {
        let chars: Vec<char> = text.chars().collect();
        spans(text)
            .into_iter()
            .map(|(a, b, k)| (&text[char_byte(&chars, a)..char_byte(&chars, b)], k))
            .collect()
    }

    fn char_byte(chars: &[char], offset: usize) -> usize {
        chars[..offset].iter().map(|c| c.len_utf8()).sum()
    }

    #[test]
    fn a_key_and_its_value_colour_differently() {
        let got = kinds(r#"{"FFlagFoo": "True"}"#);
        assert_eq!(
            got,
            vec![
                ("{", Token::Punct),
                ("\"FFlagFoo\"", Token::Key),
                (":", Token::Punct),
                ("\"True\"", Token::Str),
                ("}", Token::Punct),
            ]
        );
    }

    /// **The offsets are characters, and this is the test that says so.**
    /// `TextBuffer::iter_at_offset` counts characters; a scanner counting
    /// bytes looks perfect until a document contains one non-ASCII character,
    /// after which every colour in the rest of the file is displaced. That is
    /// the kind of bug nobody reproduces on purpose.
    #[test]
    fn offsets_are_characters_not_bytes() {
        let text = r#"{"é": 1}"#;
        let spans = spans(text);
        let (start, end, kind) = spans[1];
        assert_eq!(kind, Token::Key);
        // Three characters -- quote, e-acute, quote -- but four bytes.
        assert_eq!(end - start, 3, "counted bytes instead of characters: {spans:?}");
        assert_eq!(text.chars().count(), 8);
    }

    #[test]
    fn an_escaped_quote_does_not_end_the_string() {
        let got = kinds(r#"{"a": "say \"hi\""}"#);
        assert_eq!(got[3], (r#""say \"hi\"""#, Token::Str));
        assert_eq!(got.len(), 5, "the string swallowed the rest: {got:?}");
    }

    /// A live editor spends most of its time holding invalid JSON, so the
    /// scanner must not need valid input. It colours what it can and stops.
    #[test]
    fn a_half_typed_document_still_colours() {
        let got = kinds(r#"{"FFlagFoo": "Tru"#);
        assert_eq!(got[1], ("\"FFlagFoo\"", Token::Key));
        assert_eq!(got[3], ("\"Tru", Token::Str), "an unterminated string runs to the end");
    }

    #[test]
    fn numbers_and_keywords_are_their_own_kinds() {
        let got = kinds(r#"{"a": -1.5e3, "b": true, "c": null}"#);
        assert!(got.contains(&("-1.5e3", Token::Number)), "{got:?}");
        assert!(got.contains(&("true", Token::Keyword)), "{got:?}");
        assert!(got.contains(&("null", Token::Keyword)), "{got:?}");
    }

    /// A bare word that is not a JSON keyword gets no colour at all, rather
    /// than being guessed at. `True` with a capital is Roblox's spelling for a
    /// *string* value, and if somebody has written it unquoted it is a mistake
    /// -- leaving it plain is a quieter version of the same hint the status
    /// line gives out loud.
    #[test]
    fn an_unquoted_roblox_style_boolean_is_not_a_keyword() {
        let got = kinds(r#"{"a": True}"#);
        assert!(!got.iter().any(|(t, _)| *t == "True"), "{got:?}");
    }

    #[test]
    fn an_empty_document_is_no_work() {
        assert!(spans("").is_empty());
        assert_eq!(spans("{}").len(), 2);
    }
}
