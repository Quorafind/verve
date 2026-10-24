//! Static CommonJS export discovery. No source rewriting or JavaScript lexer is executed.
//!
//! The conservative token walk recognizes the assignments and reexports emitted by common
//! bundlers, but never infers named exports by executing a module or enumerating its result.

use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    Word(String),
    String(String),
    Punct(char),
}

#[derive(Default, Debug)]
pub(crate) struct Exports {
    pub names: BTreeSet<String>,
    pub reexports: BTreeSet<String>,
}

pub(crate) fn analyze(source: &str) -> Exports {
    let tokens = tokenize(source);
    let mut out = Exports::default();
    for at in 0..tokens.len() {
        let Some(end) = exports_object(&tokens, at) else {
            continue;
        };
        if punct(&tokens, end, '.') {
            if let Some(Token::Word(name)) = tokens.get(end + 1)
                && punct(&tokens, end + 2, '=')
                && !punct(&tokens, end + 3, '=')
            {
                out.names.insert(name.clone());
            }
        } else if punct(&tokens, end, '[') {
            if let Some(Token::String(name)) = tokens.get(end + 1)
                && punct(&tokens, end + 2, ']')
                && punct(&tokens, end + 3, '=')
            {
                out.names.insert(name.clone());
            }
        } else if punct(&tokens, end, '=') {
            if let Some(name) = required(&tokens, end + 1) {
                out.reexports.insert(name.to_owned());
            }
            if punct(&tokens, end + 1, '{') {
                let mut index = end + 2;
                let mut depth = 0usize;
                let mut key = true;
                while let Some(token) = tokens.get(index) {
                    match token {
                        Token::Punct('}') if depth == 0 => break,
                        Token::Punct('{') | Token::Punct('[') | Token::Punct('(') => depth += 1,
                        Token::Punct('}') | Token::Punct(']') | Token::Punct(')') => {
                            depth = depth.saturating_sub(1)
                        }
                        Token::Punct(',') if depth == 0 => key = true,
                        Token::Word(name) | Token::String(name) if depth == 0 && key => {
                            if punct(&tokens, index + 1, ':')
                                || punct(&tokens, index + 1, ',')
                                || punct(&tokens, index + 1, '}')
                            {
                                out.names.insert(name.clone());
                            }
                            key = false;
                        }
                        _ => {}
                    }
                    index += 1;
                }
            }
        }
    }
    for at in 0..tokens.len() {
        if word(&tokens, at, "Object")
            && punct(&tokens, at + 1, '.')
            && word(&tokens, at + 2, "defineProperty")
            && punct(&tokens, at + 3, '(')
            && let Some(end) = exports_object(&tokens, at + 4)
            && punct(&tokens, end, ',')
            && let Some(Token::String(name)) = tokens.get(end + 1)
        {
            out.names.insert(name.clone());
        }
        if let Some(Token::Word(helper)) = tokens.get(at)
            && (helper == "__exportStar" || helper == "__export")
            && punct(&tokens, at + 1, '(')
            && let Some(name) = required(&tokens, at + 2)
        {
            out.reexports.insert(name.to_owned());
        }
    }
    // Node always supplies the entire CommonJS exports value as default, even with __esModule.
    out.names.remove("default");
    out
}

fn exports_object(tokens: &[Token], at: usize) -> Option<usize> {
    if at > 0 && punct(tokens, at - 1, '.') {
        return None;
    }
    if word(tokens, at, "exports") {
        return Some(at + 1);
    }
    (word(tokens, at, "module") && punct(tokens, at + 1, '.') && word(tokens, at + 2, "exports"))
        .then_some(at + 3)
}

fn required(tokens: &[Token], at: usize) -> Option<&str> {
    if word(tokens, at, "require")
        && punct(tokens, at + 1, '(')
        && let Some(Token::String(name)) = tokens.get(at + 2)
        && punct(tokens, at + 3, ')')
    {
        return Some(name);
    }
    None
}

fn word(tokens: &[Token], at: usize, name: &str) -> bool {
    matches!(tokens.get(at), Some(Token::Word(s)) if s == name)
}
fn punct(tokens: &[Token], at: usize, value: char) -> bool {
    tokens.get(at) == Some(&Token::Punct(value))
}

fn tokenize(source: &str) -> Vec<Token> {
    let chars: Vec<_> = source.chars().collect();
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(&c) = chars.get(at) {
        if c.is_whitespace() {
            at += 1;
            continue;
        }
        if c == '/' && chars.get(at + 1) == Some(&'/') {
            at += 2;
            while chars.get(at).is_some_and(|c| *c != '\n') {
                at += 1;
            }
            continue;
        }
        if c == '/' && chars.get(at + 1) == Some(&'*') {
            at += 2;
            while at + 1 < chars.len() && !(chars[at] == '*' && chars[at + 1] == '/') {
                at += 1;
            }
            at += 2;
            continue;
        }
        if c == '\'' || c == '"' || c == '`' {
            let quote = c;
            at += 1;
            let mut text = String::new();
            while let Some(&c) = chars.get(at) {
                at += 1;
                if c == quote {
                    break;
                }
                if c == '\\' {
                    if let Some(&escaped) = chars.get(at) {
                        at += 1;
                        text.push(match escaped {
                            'n' => '\n',
                            'r' => '\r',
                            't' => '\t',
                            other => other,
                        });
                    }
                } else {
                    text.push(c);
                }
            }
            // Templates are opaque: apparent exports in their literal text are not code.
            if quote != '`' {
                out.push(Token::String(text));
            } else {
                out.push(Token::Punct('`'));
            }
            continue;
        }
        // A slash in expression-start position starts a regexp literal, not executable tokens.
        if c == '/'
            && (out.is_empty()
                || matches!(
                    out.last(),
                    Some(Token::Punct('=' | '(' | ',' | ':' | '[' | '!' | ';' | '{'))
                )
                || matches!(out.last(), Some(Token::Word(w)) if w == "return" || w == "throw"))
        {
            at += 1;
            let mut class = false;
            while let Some(&c) = chars.get(at) {
                at += 1;
                if c == '\\' {
                    at += 1;
                    continue;
                }
                if c == '[' {
                    class = true;
                }
                if c == ']' {
                    class = false;
                }
                if c == '/' && !class {
                    break;
                }
            }
            while chars.get(at).is_some_and(|c| c.is_alphabetic()) {
                at += 1;
            }
            out.push(Token::Punct('/'));
            continue;
        }
        if c.is_alphanumeric() || c == '_' || c == '$' {
            let start = at;
            at += 1;
            while chars
                .get(at)
                .is_some_and(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
            {
                at += 1;
            }
            out.push(Token::Word(chars[start..at].iter().collect()));
        } else {
            out.push(Token::Punct(c));
            at += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assignments_reexports_and_opaque_literals() {
        let found = analyze(
            r#"
            // exports.comment = 1
            const text = "exports.string = 1";
            const re = /exports.regex = 1/;
            exports.one = 1; module.exports.two = 2; exports['three'] = 3;
            Object.defineProperty(exports, '__esModule', {value:true});
            module.exports = {four: 4, five};
            __exportStar(require('./other.cjs'), exports);
        "#,
        );
        assert_eq!(
            found.names.into_iter().collect::<Vec<_>>(),
            ["__esModule", "five", "four", "one", "three", "two"]
        );
        assert_eq!(
            found.reexports.into_iter().collect::<Vec<_>>(),
            ["./other.cjs"]
        );
    }
}
