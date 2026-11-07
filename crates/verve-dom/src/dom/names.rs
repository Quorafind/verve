//! The names of CSS properties and the text of inline styles.

/// `color: red; padding: 4px !important` as pairs, and the names declared `!important`.
pub fn parse_style(text: &str) -> (Vec<(String, String)>, Vec<String>) {
    let mut important = Vec::new();
    let pairs = crate::css::declarations(text)
        .into_iter()
        .map(|d| {
            if d.important {
                important.push(d.name.clone());
            }
            (d.name, d.value)
        })
        .collect();
    (pairs, important)
}

/// `fontSize` as `font-size`, `cssFloat` as `float`, `webkitTransform` as `-webkit-transform`; a
/// custom property is left alone.
pub fn css_name(camel: &str) -> String {
    if camel.starts_with("--") {
        return camel.to_owned();
    }
    if camel == "cssFloat" {
        return "float".to_owned();
    }
    let mut out = String::with_capacity(camel.len() + 4);
    for c in camel.chars() {
        if c.is_ascii_uppercase() {
            out.push('-');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    for prefix in ["webkit-", "moz-", "ms-"] {
        if out.starts_with(prefix) {
            return format!("-{out}");
        }
    }
    out
}

/// `font-size` as `fontSize`.
pub fn camel_name(css: &str) -> String {
    if css.starts_with("--") {
        return css.to_owned();
    }
    if css == "float" {
        return "cssFloat".to_owned();
    }
    let css = css.strip_prefix('-').unwrap_or(css);
    let mut out = String::with_capacity(css.len());
    let mut upper = false;
    for c in css.chars() {
        if c == '-' {
            upper = true;
        } else if upper {
            out.push(c.to_ascii_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}
