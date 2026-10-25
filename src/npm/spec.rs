//! What a dependency's value in `package.json` asks for, read as npm-package-arg reads it.

use super::semver::{Range, Version};

/// A registry request: a range, an exact version or a dist-tag of a package by its real name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Wanted {
    Range(Range),
    Tag(String),
}

/// One dependency spec.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Spec {
    /// From the registry; `name` is the package fetched, which differs from the folder it is
    /// installed under for an alias (`"foo": "npm:bar@^1"`).
    Registry { name: String, wanted: Wanted },
    /// A tarball fetched from a URL.
    Tarball { url: String },
}

impl Spec {
    /// Whether `version` of `name` is what this spec asks for. A tag or a tarball is answered
    /// by whatever was installed for it: npm does not re-resolve those against an installed tree.
    pub fn accepts(&self, name: &str, version: &str) -> bool {
        match self {
            Spec::Registry {
                name: wanted_name,
                wanted,
            } => {
                wanted_name == name
                    && match wanted {
                        Wanted::Range(range) => {
                            Version::parse(version).is_some_and(|v| range.satisfies(&v))
                        }
                        Wanted::Tag(_) => true,
                    }
            }
            Spec::Tarball { .. } => true,
        }
    }
}

/// Why a spec cannot be installed: the message that follows the package's name.
pub type SpecError = String;

/// Whether `name` is a name npm would publish: lowercase-free checks are left to the registry,
/// but nothing that could escape `node_modules` gets past here.
pub fn valid_name(name: &str) -> bool {
    let (scope, base) = match name.strip_prefix('@') {
        Some(rest) => match rest.split_once('/') {
            Some((scope, base)) => (Some(scope), base),
            None => return false,
        },
        None => (None, name),
    };
    let part_ok = |part: &str| {
        !part.is_empty()
            && !part.starts_with('.')
            && !part.starts_with('_')
            && part.len() <= 214
            && part.bytes().all(|b| {
                b.is_ascii_alphanumeric()
                    || matches!(
                        b,
                        b'-' | b'.' | b'_' | b'~' | b'!' | b'*' | b'\'' | b'(' | b')'
                    )
            })
    };
    scope.is_none_or(part_ok) && part_ok(base)
}

/// Reads `spec`, the value of dependency `name`.
pub fn parse(name: &str, spec: &str) -> Result<Spec, SpecError> {
    if !valid_name(name) {
        return Err("is not a valid package name".to_owned());
    }
    let spec = spec.trim();
    if let Some(aliased) = spec.strip_prefix("npm:") {
        // `npm:name@range`, the `@` after a scope's being part of the name.
        let at = aliased
            .char_indices()
            .skip(1)
            .find(|&(_, c)| c == '@')
            .map(|(i, _)| i);
        let (real, wanted) = match at {
            Some(i) => (&aliased[..i], &aliased[i + 1..]),
            None => (aliased, ""),
        };
        if !valid_name(real) {
            return Err(format!(
                "aliases {real:?}, which is not a valid package name"
            ));
        }
        if wanted.starts_with("npm:") {
            return Err("is an alias of an alias".to_owned());
        }
        return Ok(Spec::Registry {
            name: real.to_owned(),
            wanted: wanted_of(wanted)?,
        });
    }
    if spec.starts_with("http://") || spec.starts_with("https://") {
        if spec.ends_with(".git") || spec.contains(".git#") {
            return Err(unsupported_git(spec));
        }
        return Ok(Spec::Tarball {
            url: spec.to_owned(),
        });
    }
    for scheme in [
        "git+",
        "git://",
        "github:",
        "gitlab:",
        "bitbucket:",
        "gist:",
    ] {
        if spec.starts_with(scheme) {
            return Err(unsupported_git(spec));
        }
    }
    if spec.starts_with("file:") || spec.starts_with("link:") || is_path(spec) {
        return Err(format!(
            "{spec:?} is a local path; only registry and tarball URL dependencies are installed \
             (a local folder is a link, which this installer does not make)"
        ));
    }
    if spec.starts_with("workspace:") {
        return Err(format!(
            "{spec:?} is a workspace protocol, which npm itself does not read"
        ));
    }
    // `user/repo` and `user/repo#ref`: GitHub shorthand.
    if !spec.starts_with('@')
        && spec.contains('/')
        && !spec.contains(' ')
        && Range::parse(spec).is_none()
    {
        return Err(unsupported_git(spec));
    }
    Ok(Spec::Registry {
        name: name.to_owned(),
        wanted: wanted_of(spec)?,
    })
}

fn is_path(spec: &str) -> bool {
    spec.starts_with("./")
        || spec.starts_with("../")
        || spec.starts_with('/')
        || spec.starts_with("~/")
        || spec.starts_with(".\\")
        || spec.starts_with("..\\")
        || (spec.len() > 2
            && spec.as_bytes()[1] == b':'
            && spec.as_bytes()[0].is_ascii_alphabetic())
}

fn unsupported_git(spec: &str) -> SpecError {
    format!(
        "{spec:?} is a git dependency; installing from git needs a git checkout and the \
         package's prepare script, and this installer runs neither — publish it or use a \
         tarball URL"
    )
}

/// A range when node-semver reads one, else a dist-tag (npm's rule). An empty spec is the
/// range `*`, as `semver.validRange('')` makes it.
fn wanted_of(text: &str) -> Result<Wanted, SpecError> {
    let text = text.trim();
    if let Some(range) = Range::parse(text) {
        return Ok(Wanted::Range(range));
    }
    let tag_ok = text
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'));
    if tag_ok {
        Ok(Wanted::Tag(text.to_owned()))
    } else {
        Err(format!(
            "{text:?} is neither a version range nor a dist-tag"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs_read_as_npm_reads_them() {
        assert!(matches!(
            parse("a", "^1.2.0"),
            Ok(Spec::Registry { ref name, wanted: Wanted::Range(_) }) if name == "a"
        ));
        assert_eq!(
            parse("a", "next"),
            Ok(Spec::Registry {
                name: "a".into(),
                wanted: Wanted::Tag("next".into())
            })
        );
        assert_eq!(
            parse("a", ""),
            Ok(Spec::Registry {
                name: "a".into(),
                wanted: Wanted::Range(Range::parse("*").unwrap())
            })
        );
        assert!(matches!(
            parse("b", "npm:@scope/real@~2"),
            Ok(Spec::Registry { ref name, wanted: Wanted::Range(_) }) if name == "@scope/real"
        ));
        assert_eq!(
            parse("b", "npm:real"),
            Ok(Spec::Registry {
                name: "real".into(),
                wanted: Wanted::Range(Range::parse("").unwrap())
            })
        );
        assert_eq!(
            parse("t", "https://example.test/t-1.0.0.tgz"),
            Ok(Spec::Tarball {
                url: "https://example.test/t-1.0.0.tgz".into()
            })
        );
        for git in ["git+https://x/y.git", "github:u/r", "u/r#main", "git://x/y"] {
            assert!(
                parse("g", git).unwrap_err().contains("git dependency"),
                "{git}"
            );
        }
        for path in ["file:../x", "./x", "link:../y"] {
            assert!(
                parse("p", path).unwrap_err().contains("local path"),
                "{path}"
            );
        }
        assert!(parse("../evil", "1.0.0").is_err());
        assert!(parse("@scope", "1.0.0").is_err());
        assert!(parse("a", "not a range").is_err());
    }
}
