//! Pure-std POSIX shell word splitting (`split`), replacing the external
//! `shlex` dependency with zero `unsafe` code.

/// Splits `input` into words using POSIX shell syntax, returning `None` on
/// unclosed quotes or a trailing unescaped backslash.
#[must_use]
pub fn split(input: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            ' ' | '\t' | '\n' => {}
            '#' => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            '\\' => {
                let mut word = String::new();
                match chars.next() {
                    None => return None,
                    Some('\n') => {}
                    Some(escaped) => word.push(escaped),
                }
                read_word(&mut chars, &mut word)?;
                words.push(word);
            }
            '\'' => {
                let mut word = String::new();
                read_single_quoted(&mut chars, &mut word)?;
                read_word(&mut chars, &mut word)?;
                words.push(word);
            }
            '"' => {
                let mut word = String::new();
                read_double_quoted(&mut chars, &mut word)?;
                read_word(&mut chars, &mut word)?;
                words.push(word);
            }
            other => {
                let mut word = String::new();
                word.push(other);
                read_word(&mut chars, &mut word)?;
                words.push(word);
            }
        }
    }

    Some(words)
}

fn read_word<I>(chars: &mut std::iter::Peekable<I>, word: &mut String) -> Option<()>
where
    I: Iterator<Item = char>,
{
    while let Some(ch) = chars.next() {
        match ch {
            ' ' | '\t' | '\n' => break,
            '\'' => read_single_quoted(chars, word)?,
            '"' => read_double_quoted(chars, word)?,
            '\\' => match chars.next() {
                None => return None,
                Some('\n') => {}
                Some(escaped) => word.push(escaped),
            },
            other => word.push(other),
        }
    }
    Some(())
}

fn read_single_quoted<I>(chars: &mut I, word: &mut String) -> Option<()>
where
    I: Iterator<Item = char>,
{
    for ch in chars {
        if ch == '\'' {
            return Some(());
        }
        word.push(ch);
    }
    None
}

fn read_double_quoted<I>(chars: &mut I, word: &mut String) -> Option<()>
where
    I: Iterator<Item = char>,
{
    while let Some(ch) = chars.next() {
        match ch {
            '"' => return Some(()),
            '\\' => match chars.next() {
                None => return None,
                Some('\n') => {}
                Some(escaped @ ('$' | '`' | '"' | '\\')) => word.push(escaped),
                Some(other) => {
                    word.push('\\');
                    word.push(other);
                }
            },
            other => word.push(other),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::split;

    #[test]
    fn splits_posix_words_quotes_comments_and_escapes() {
        assert_eq!(split(""), Some(Vec::<String>::new()));
        assert_eq!(split("   \t\n  "), Some(Vec::<String>::new()));
        assert_eq!(split("# comment only"), Some(Vec::<String>::new()));
        assert_eq!(
            split("git commit -m 'hello world' # trailing comment\nstatus"),
            Some(vec![
                "git".to_string(),
                "commit".to_string(),
                "-m".to_string(),
                "hello world".to_string(),
                "status".to_string(),
            ])
        );
        assert_eq!(
            split("foo#bar '' \"\" a'b'\"c\""),
            Some(vec![
                "foo#bar".to_string(),
                String::new(),
                String::new(),
                "abc".to_string(),
            ])
        );
        assert_eq!(
            split(r#""a\"b\\c\$d\`e\nf""#),
            Some(vec![r#"a"b\c$d`e\nf"#.to_string()])
        );
        assert_eq!(
            split("line\\\njoined \\\n next"),
            Some(vec![
                "linejoined".to_string(),
                String::new(),
                "next".to_string()
            ])
        );
    }

    #[test]
    fn rejects_unclosed_quotes_and_trailing_backslash() {
        assert_eq!(split("'unclosed"), None);
        assert_eq!(split("\"unclosed"), None);
        assert_eq!(split("trailing\\"), None);
        assert_eq!(split("\"trailing\\"), None);
    }

    #[test]
    fn preserves_cr_unicode_nul_and_escaped_whitespace() {
        assert_eq!(split("\r"), Some(vec!["\r".to_string()]));
        assert_eq!(split("a\rb"), Some(vec!["a\rb".to_string()]));
        assert_eq!(split("é\0🦀"), Some(vec!["é\0🦀".to_string()]));
        assert_eq!(
            split(r"\  \	"),
            Some(vec![" ".to_string(), "\t".to_string()])
        );
        assert_eq!(split("\\\n"), Some(vec![String::new()]));
    }
}
