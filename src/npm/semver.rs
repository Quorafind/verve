//! Versions and ranges as npm reads them: node-semver's grammar and its rules.
//!
//! Rust's `semver` crate follows Cargo, which is not npm: `1.2` there means `^1.2`, and a range
//! never lets a prerelease in unless a comparator names the same `major.minor.patch`. npm's
//! ranges mean something else in several places — `1.2` is `1.2.x`, `^0.0.x` stops at `0.1.0`,
//! `1.2.3 - 2` reaches every `2.x`, `||` joins sets — and a package's `package.json` is written
//! against npm's meaning, so the resolver has to read it the same way. This is node-semver 7's
//! non-loose parse, desugared into comparator sets the way its `Range` class does.

use std::cmp::Ordering;
use std::fmt;

/// One dot-separated part of a prerelease: numeric parts sort below alphanumeric ones.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Ident {
    Num(u64),
    Alpha(String),
}

impl Ord for Ident {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Ident::Num(a), Ident::Num(b)) => a.cmp(b),
            (Ident::Num(_), Ident::Alpha(_)) => Ordering::Less,
            (Ident::Alpha(_), Ident::Num(_)) => Ordering::Greater,
            (Ident::Alpha(a), Ident::Alpha(b)) => a.cmp(b),
        }
    }
}

impl PartialOrd for Ident {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A full version. Build metadata is kept for printing but never compared, as in node-semver.
#[derive(Clone, Debug, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre: Vec<Ident>,
    pub build: Vec<String>,
}

impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => self.pre.cmp(&other.pre),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        for (i, part) in self.pre.iter().enumerate() {
            f.write_str(if i == 0 { "-" } else { "." })?;
            match part {
                Ident::Num(n) => write!(f, "{n}")?,
                Ident::Alpha(s) => f.write_str(s)?,
            }
        }
        if !self.build.is_empty() {
            write!(f, "+{}", self.build.join("."))?;
        }
        Ok(())
    }
}

impl Version {
    fn new(major: u64, minor: u64, patch: u64, pre: Vec<Ident>) -> Self {
        Version {
            major,
            minor,
            patch,
            pre,
            build: Vec::new(),
        }
    }

    /// `x.y.z-0`: the lowest version of `x.y.z`, so `<x.y.z-0` stops below its prereleases.
    fn floor(major: u64, minor: u64, patch: u64) -> Self {
        Version::new(major, minor, patch, vec![Ident::Num(0)])
    }

    /// Parses a full version, allowing the leading `v` or `=` and surrounding space npm allows.
    pub fn parse(text: &str) -> Option<Version> {
        let text = text.trim();
        let text = text.strip_prefix('=').unwrap_or(text).trim_start();
        let text = text.strip_prefix('v').unwrap_or(text);
        match parse_partial(text)? {
            (Part::Num(major), Part::Num(minor), Part::Num(patch), pre, build) => Some(Version {
                major,
                minor,
                patch,
                pre,
                build,
            }),
            _ => None,
        }
    }

    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Num(u64),
    /// `x`, `X`, `*` or missing.
    Any,
}

type Partial = (Part, Part, Part, Vec<Ident>, Vec<String>);

fn number(text: &str) -> Option<u64> {
    // node-semver refuses leading zeros in the strict grammar, and numbers past 2^53.
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if text.len() > 1 && text.starts_with('0') {
        return None;
    }
    let n: u64 = text.parse().ok()?;
    (n <= 9_007_199_254_740_991).then_some(n)
}

fn identifiers(text: &str, numeric_rules: bool) -> Option<Vec<String>> {
    text.split('.')
        .map(|part| {
            let ok = !part.is_empty()
                && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                && !(numeric_rules
                    && part.len() > 1
                    && part.starts_with('0')
                    && part.bytes().all(|b| b.is_ascii_digit()));
            ok.then(|| part.to_owned())
        })
        .collect()
}

/// `1`, `1.2`, `1.x.*`, `1.2.3-pre+build`: a version with the parts a range may leave out.
fn parse_partial(token: &str) -> Option<Partial> {
    let (core, build) = match token.split_once('+') {
        Some((core, build)) => (core, identifiers(build, false)?),
        None => (token, Vec::new()),
    };
    let (numbers, pre) = match core.split_once('-') {
        Some((numbers, pre)) => (numbers, Some(pre)),
        None => (core, None),
    };
    let mut parts = numbers.split('.');
    let mut next = |required: bool| -> Option<Part> {
        match parts.next() {
            None if required => None,
            None => Some(Part::Any),
            Some("x" | "X" | "*") => Some(Part::Any),
            Some(n) => number(n).map(Part::Num),
        }
    };
    let major = next(true)?;
    let minor = next(false)?;
    let patch = next(false)?;
    if parts.next().is_some() {
        return None;
    }
    let pre = match pre {
        None => Vec::new(),
        Some(pre) => identifiers(pre, true)?
            .into_iter()
            .map(|part| match number(&part) {
                Some(n) if part.bytes().all(|b| b.is_ascii_digit()) => Ident::Num(n),
                _ => Ident::Alpha(part),
            })
            .collect(),
    };
    // A prerelease only means something on a full version (`1.2.3-x`, not `1.2-x`).
    if !pre.is_empty() && patch == Part::Any {
        return None;
    }
    Some((major, minor, patch, pre, build))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Comparator {
    /// Every version (`*`), though a prerelease still needs a comparator that names its tuple.
    Any,
    /// Nothing at all (`<0.0.0-0`, what `>*` means).
    None,
    Cmp(Op, Version),
}

impl Comparator {
    fn test(&self, version: &Version) -> bool {
        match self {
            Comparator::Any => true,
            Comparator::None => false,
            Comparator::Cmp(op, bound) => {
                let ord = version.cmp(bound);
                match op {
                    Op::Lt => ord == Ordering::Less,
                    Op::Le => ord != Ordering::Greater,
                    Op::Gt => ord == Ordering::Greater,
                    Op::Ge => ord != Ordering::Less,
                    Op::Eq => ord == Ordering::Equal,
                }
            }
        }
    }
}

/// A range: sets of comparators joined by `||`, each set satisfied when all of its are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Range {
    sets: Vec<Vec<Comparator>>,
}

impl Range {
    /// Parses a range the way `semver.validRange` does; `None` is what npm calls an invalid
    /// range — npm then reads the spec as a dist-tag.
    pub fn parse(text: &str) -> Option<Range> {
        let sets = text
            .split("||")
            .map(parse_set)
            .collect::<Option<Vec<_>>>()?;
        Some(Range { sets })
    }

    /// Whether `version` is in the range, by node-semver's prerelease rule: a prerelease is in
    /// only when some comparator of the set that admits it names the same `major.minor.patch`
    /// with a prerelease of its own — so `^1.2.3` admits no `1.3.0-beta`, and `^1.3.0-beta.1`
    /// admits `1.3.0-beta.2` but not `1.4.0-beta`.
    pub fn satisfies(&self, version: &Version) -> bool {
        self.sets.iter().any(|set| {
            set.iter().all(|c| c.test(version))
                && (!version.is_prerelease()
                    || set.iter().any(|c| match c {
                        Comparator::Cmp(_, bound) => {
                            bound.is_prerelease()
                                && (bound.major, bound.minor, bound.patch)
                                    == (version.major, version.minor, version.patch)
                        }
                        _ => false,
                    }))
        })
    }

    /// Whether this is `*` (or empty): npm then takes the `latest` tag even when it names a
    /// prerelease.
    pub fn is_any(&self) -> bool {
        self.sets.len() == 1 && self.sets[0] == [Comparator::Any]
    }

    /// The highest of `versions` the range admits.
    #[cfg(test)]
    pub fn max_satisfying<'a>(
        &self,
        versions: impl IntoIterator<Item = &'a Version>,
    ) -> Option<&'a Version> {
        versions.into_iter().filter(|v| self.satisfies(v)).max()
    }
}

/// Squeezes the space node-semver allows between an operator and its version (`>= 1.2`), and
/// reads `~>` as `~`, so a set splits on whitespace into one token per comparator.
fn tokens(set: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut pending_op = String::new();
    for word in set.split_whitespace() {
        let word = word.replace("~>", "~");
        if word
            .bytes()
            .all(|b| matches!(b, b'<' | b'>' | b'=' | b'~' | b'^'))
        {
            pending_op.push_str(&word);
            continue;
        }
        out.push(format!("{}{}", std::mem::take(&mut pending_op), word));
    }
    if !pending_op.is_empty() {
        out.push(pending_op);
    }
    out
}

fn parse_set(set: &str) -> Option<Vec<Comparator>> {
    let set = set.trim();
    // Hyphen ranges need the spaces: `1.2.3 - 2.3.4`.
    let words: Vec<&str> = set.split_whitespace().collect();
    if words.len() == 3 && words[1] == "-" {
        return hyphen(words[0], words[2]);
    }
    let mut out = Vec::new();
    for token in tokens(set) {
        out.extend(comparator(&token)?);
    }
    if out.is_empty() {
        out.push(Comparator::Any);
    }
    Some(out)
}

fn strip_v(text: &str) -> &str {
    let text = text.strip_prefix('=').unwrap_or(text);
    text.strip_prefix('v').unwrap_or(text)
}

fn hyphen(from: &str, to: &str) -> Option<Vec<Comparator>> {
    let (fm, fi, fp, fpre, _) = parse_partial(strip_v(from))?;
    let (tm, ti, tp, tpre, _) = parse_partial(strip_v(to))?;
    let mut out = Vec::new();
    match (fm, fi, fp) {
        (Part::Any, _, _) => {}
        (Part::Num(m), Part::Any, _) => {
            out.push(Comparator::Cmp(Op::Ge, Version::new(m, 0, 0, vec![])))
        }
        (Part::Num(m), Part::Num(i), Part::Any) => {
            out.push(Comparator::Cmp(Op::Ge, Version::new(m, i, 0, vec![])))
        }
        (Part::Num(m), Part::Num(i), Part::Num(p)) => {
            out.push(Comparator::Cmp(Op::Ge, Version::new(m, i, p, fpre)))
        }
    }
    match (tm, ti, tp) {
        (Part::Any, _, _) => {}
        (Part::Num(m), Part::Any, _) => {
            out.push(Comparator::Cmp(Op::Lt, Version::floor(m + 1, 0, 0)))
        }
        (Part::Num(m), Part::Num(i), Part::Any) => {
            out.push(Comparator::Cmp(Op::Lt, Version::floor(m, i + 1, 0)))
        }
        (Part::Num(m), Part::Num(i), Part::Num(p)) => {
            out.push(Comparator::Cmp(Op::Le, Version::new(m, i, p, tpre)))
        }
    }
    if out.is_empty() {
        out.push(Comparator::Any);
    }
    Some(out)
}

fn comparator(token: &str) -> Option<Vec<Comparator>> {
    let (op, rest) = ["<=", ">=", "<", ">", "=", "~", "^"]
        .iter()
        .find_map(|op| token.strip_prefix(op).map(|rest| (*op, rest)))
        .unwrap_or(("", token));
    let rest = rest.strip_prefix('v').unwrap_or(rest);
    let (major, minor, patch, pre, _) = parse_partial(rest)?;
    let ge = |v: Version| Comparator::Cmp(Op::Ge, v);
    let lt = |v: Version| Comparator::Cmp(Op::Lt, v);
    use Part::{Any, Num};
    Some(match op {
        "~" => match (major, minor, patch) {
            (Any, _, _) => vec![Comparator::Any],
            (Num(m), Any, _) => vec![
                ge(Version::new(m, 0, 0, vec![])),
                lt(Version::floor(m + 1, 0, 0)),
            ],
            (Num(m), Num(i), Any) => vec![
                ge(Version::new(m, i, 0, vec![])),
                lt(Version::floor(m, i + 1, 0)),
            ],
            (Num(m), Num(i), Num(p)) => vec![
                ge(Version::new(m, i, p, pre)),
                lt(Version::floor(m, i + 1, 0)),
            ],
        },
        "^" => match (major, minor, patch) {
            (Any, _, _) => vec![Comparator::Any],
            (Num(m), Any, _) => vec![
                ge(Version::new(m, 0, 0, vec![])),
                lt(Version::floor(m + 1, 0, 0)),
            ],
            (Num(0), Num(i), Any) => vec![
                ge(Version::new(0, i, 0, vec![])),
                lt(Version::floor(0, i + 1, 0)),
            ],
            (Num(m), Num(i), Any) => vec![
                ge(Version::new(m, i, 0, vec![])),
                lt(Version::floor(m + 1, 0, 0)),
            ],
            (Num(0), Num(0), Num(p)) => vec![
                ge(Version::new(0, 0, p, pre)),
                lt(Version::floor(0, 0, p + 1)),
            ],
            (Num(0), Num(i), Num(p)) => vec![
                ge(Version::new(0, i, p, pre)),
                lt(Version::floor(0, i + 1, 0)),
            ],
            (Num(m), Num(i), Num(p)) => vec![
                ge(Version::new(m, i, p, pre)),
                lt(Version::floor(m + 1, 0, 0)),
            ],
        },
        "" | "=" => match (major, minor, patch) {
            (Any, _, _) => vec![Comparator::Any],
            (Num(m), Any, _) => vec![
                ge(Version::new(m, 0, 0, vec![])),
                lt(Version::floor(m + 1, 0, 0)),
            ],
            (Num(m), Num(i), Any) => vec![
                ge(Version::new(m, i, 0, vec![])),
                lt(Version::floor(m, i + 1, 0)),
            ],
            (Num(m), Num(i), Num(p)) => vec![Comparator::Cmp(Op::Eq, Version::new(m, i, p, pre))],
        },
        ">" => match (major, minor, patch) {
            (Any, _, _) => vec![Comparator::None],
            (Num(m), Any, _) => vec![ge(Version::new(m + 1, 0, 0, vec![]))],
            (Num(m), Num(i), Any) => vec![ge(Version::new(m, i + 1, 0, vec![]))],
            (Num(m), Num(i), Num(p)) => vec![Comparator::Cmp(Op::Gt, Version::new(m, i, p, pre))],
        },
        ">=" => match (major, minor, patch) {
            (Any, _, _) => vec![Comparator::Any],
            (Num(m), Any, _) => vec![ge(Version::new(m, 0, 0, vec![]))],
            (Num(m), Num(i), Any) => vec![ge(Version::new(m, i, 0, vec![]))],
            (Num(m), Num(i), Num(p)) => vec![ge(Version::new(m, i, p, pre))],
        },
        "<" => match (major, minor, patch) {
            (Any, _, _) => vec![Comparator::None],
            (Num(m), Any, _) => vec![lt(Version::floor(m, 0, 0))],
            (Num(m), Num(i), Any) => vec![lt(Version::floor(m, i, 0))],
            (Num(m), Num(i), Num(p)) => vec![lt(Version::new(m, i, p, pre))],
        },
        "<=" => match (major, minor, patch) {
            (Any, _, _) => vec![Comparator::Any],
            (Num(m), Any, _) => vec![lt(Version::floor(m + 1, 0, 0))],
            (Num(m), Num(i), Any) => vec![lt(Version::floor(m, i + 1, 0))],
            (Num(m), Num(i), Num(p)) => vec![Comparator::Cmp(Op::Le, Version::new(m, i, p, pre))],
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sat(range: &str, version: &str) -> bool {
        Range::parse(range)
            .unwrap_or_else(|| panic!("range {range}"))
            .satisfies(&Version::parse(version).unwrap_or_else(|| panic!("version {version}")))
    }

    #[test]
    fn versions_order_as_semver_says() {
        let order = [
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
            "1.0.1",
            "1.10.0",
        ];
        for pair in order.windows(2) {
            assert!(
                Version::parse(pair[0]).unwrap() < Version::parse(pair[1]).unwrap(),
                "{pair:?}"
            );
        }
        assert_eq!(
            Version::parse("v1.2.3+build.5").unwrap().to_string(),
            "1.2.3+build.5"
        );
        assert_eq!(Version::parse("1.2.3+a"), Version::parse("1.2.3+b"));
        assert!(Version::parse("1.2").is_none());
        assert!(Version::parse("01.2.3").is_none());
    }

    #[test]
    fn ranges_mean_what_node_semver_means() {
        let yes = [
            ("^1.2.3", "1.9.9"),
            ("^0.2.3", "0.2.9"),
            ("^0.0.3", "0.0.3"),
            ("^0.0", "0.0.9"),
            ("^0.x", "0.9.0"),
            ("~1.2", "1.2.9"),
            ("~1", "1.9.0"),
            ("~>1.2.3", "1.2.4"),
            ("1.2", "1.2.7"),
            ("1.x", "1.8.0"),
            ("*", "3.0.0"),
            ("", "3.0.0"),
            ("x", "0.0.1"),
            (">= 1.2.3 < 2", "1.5.0"),
            (">1.2", "1.3.0"),
            ("<=1.2", "1.2.99"),
            ("1.2.3 - 2.3", "2.3.9"),
            ("1.2 - 2.3.4", "1.2.0"),
            ("1.2.3 - 2", "2.99.0"),
            ("1.0.0 || >=2.5.0 <3", "2.6.0"),
            ("=1.2.3", "1.2.3"),
            ("v1.2.3", "1.2.3"),
            ("^1.2.3-beta.2", "1.2.3-beta.4"),
            ("^1.2.3-beta.2", "1.2.4"),
            (">1.2.3-alpha.3", "1.2.3-alpha.7"),
            ("1.2.3-pre", "1.2.3-pre"),
        ];
        for (range, version) in yes {
            assert!(sat(range, version), "{range} should admit {version}");
        }
        let no = [
            ("^1.2.3", "2.0.0"),
            ("^1.2.3", "2.0.0-0"),
            ("^0.2.3", "0.3.0"),
            ("^0.0.3", "0.0.4"),
            ("~1.2", "1.3.0"),
            ("1.2", "1.3.0"),
            ("<1.2", "1.2.0-beta"),
            (">1.2", "1.2.9"),
            ("1.2.3 - 2.3", "2.4.0"),
            ("*", "1.0.0-beta"),
            ("^1.2.3", "1.3.0-beta"),
            ("^1.2.3-beta.2", "1.2.4-beta.1"),
            (">1.2.3-alpha.3", "3.4.5-alpha.9"),
            (">*", "1.0.0"),
            ("1.0.0 || >=2.5.0 <3", "2.0.0"),
        ];
        for (range, version) in no {
            assert!(!sat(range, version), "{range} should not admit {version}");
        }
        for invalid in ["latest", "next", "^1.2.3.4", "1.2.3-", ">=a"] {
            assert!(
                Range::parse(invalid).is_none(),
                "{invalid} is a tag, not a range"
            );
        }
    }

    #[test]
    fn the_highest_admitted_version_wins() {
        let versions: Vec<Version> = ["1.0.0", "1.4.0", "1.5.0-rc.1", "2.0.0"]
            .iter()
            .map(|v| Version::parse(v).unwrap())
            .collect();
        let max = |r: &str| {
            Range::parse(r)
                .unwrap()
                .max_satisfying(&versions)
                .map(|v| v.to_string())
        };
        assert_eq!(max("^1").as_deref(), Some("1.4.0"));
        assert_eq!(max("^1.5.0-rc.0").as_deref(), Some("1.5.0-rc.1"));
        assert_eq!(max("^3"), None);
    }
}
