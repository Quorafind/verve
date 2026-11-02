//! The text-level readings every part of the CSS engine shares: where a list is split, where
//! a block closes, what a comment hides.

/// `text` cut at each `separator` that is not inside parentheses, brackets or quotes; a last
/// piece of only whitespace is dropped.
pub(super) fn split_outside(text: &str, separator: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut current = String::new();
    let mut escaped = false;
    for c in text.chars() {
        if escaped {
            current.push(c);
            escaped = false;
            continue;
        }
        match (quote, c) {
            (_, '\\') => {
                escaped = true;
                current.push(c);
                continue;
            }
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '(' | '[') => depth += 1,
            (None, ')' | ']') => depth -= 1,
            (None, c) if c == separator && depth <= 0 => {
                out.push(std::mem::take(&mut current));
                continue;
            }
            _ => {}
        }
        current.push(c);
    }
    if !current.trim().is_empty() {
        out.push(current);
    }
    out
}

/// `text` without its `/* … */` comments.
pub(super) fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        rest = match rest[start + 2..].find("*/") {
            Some(end) => &rest[start + 2 + end + 2..],
            None => "",
        };
    }
    out.push_str(rest);
    out
}

/// Where the `}` closing the `{` at `open` is, braces inside quotes left alone.
pub(super) fn matching_brace(text: &str, open: usize) -> Option<usize> {
    let mut depth = 0;
    let mut quote: Option<char> = None;
    for (offset, c) in text[open..].char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '{') => depth += 1,
            (None, '}') => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + offset);
                }
            }
            _ => {}
        }
    }
    None
}

/// A function as a value is written it: its name and what is inside its parentheses.
pub(super) fn function_parts(text: &str) -> Option<(&str, &str)> {
    let open = text.find('(')?;
    let args = text[open + 1..].strip_suffix(')')?;
    Some((text[..open].trim(), args.trim()))
}

/// A number as CSS writes it: `0.5`, not `0.500000`, and `3`, not `3.0`.
pub(super) fn decimal(value: f32) -> String {
    let rounded = (value * 1000.).round() / 1000.;
    if rounded.fract() == 0. {
        format!("{}", rounded as i64)
    } else {
        format!("{rounded}")
    }
}
