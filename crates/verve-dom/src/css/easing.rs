//! Easing functions (CSS Easing 1): how an animation's progress follows its time — the
//! keywords, `cubic-bezier()`, `steps()` and `linear()`.

use super::syntax::split_outside;

/// How an animation moves along. A function that does not parse is `Linear`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Easing {
    Linear,
    CubicBezier([f32; 4]),
    Steps {
        count: f32,
        at: StepAt,
    },
    /// `linear()`: the points it passes through, as `(output, input)`.
    Points(Vec<(f32, f32)>),
}

/// Where the jumps of `steps()` fall.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StepAt {
    Start,
    End,
    None,
    Both,
}

/// The keywords that stand for a curve.
const KEYWORDS: &[(&str, Easing)] = &[
    ("linear", Easing::Linear),
    ("ease", Easing::CubicBezier([0.25, 0.1, 0.25, 1.])),
    ("ease-in", Easing::CubicBezier([0.42, 0., 1., 1.])),
    ("ease-out", Easing::CubicBezier([0., 0., 0.58, 1.])),
    ("ease-in-out", Easing::CubicBezier([0.42, 0., 0.58, 1.])),
    (
        "step-start",
        Easing::Steps {
            count: 1.,
            at: StepAt::Start,
        },
    ),
    (
        "step-end",
        Easing::Steps {
            count: 1.,
            at: StepAt::End,
        },
    ),
];

impl Easing {
    /// Whether `text` is an easing function: a keyword or a call of one of the functions.
    pub(crate) fn is_easing(text: &str) -> bool {
        let lower = text.trim().to_ascii_lowercase();
        KEYWORDS.iter().any(|(name, _)| *name == lower)
            || ["cubic-bezier(", "steps(", "linear("]
                .iter()
                .any(|call| lower.starts_with(call))
    }

    /// The easing `text` says; a function that does not parse is `Linear`.
    pub(crate) fn parse(text: &str) -> Easing {
        let lower = text.trim().to_ascii_lowercase();
        if let Some((_, easing)) = KEYWORDS.iter().find(|(name, _)| *name == lower) {
            return easing.clone();
        }
        let Some((name, args)) = lower.split_once('(') else {
            return Easing::Linear;
        };
        let args = args.trim_end_matches(')');
        match name.trim() {
            "cubic-bezier" => match numbers(args).as_slice() {
                // The x of a point must be a time between 0 and 1: otherwise it is no easing.
                [a, b, c, d] if (0. ..=1.).contains(a) && (0. ..=1.).contains(c) => {
                    Easing::CubicBezier([*a, *b, *c, *d])
                }
                _ => Easing::Linear,
            },
            "steps" => steps(args),
            "linear" => Easing::Points(linear_points(args)),
            _ => Easing::Linear,
        }
    }

    /// The eased progress at `t`, both between 0 and 1.
    pub(crate) fn at(&self, t: f32) -> f32 {
        let t = t.clamp(0., 1.);
        match self {
            Easing::Linear => t,
            Easing::CubicBezier([x1, y1, x2, y2]) => bezier(*y1, *y2, solve(*x1, *x2, t)),
            Easing::Steps { count, at } => {
                let n = *count;
                let (jumped, total) = match at {
                    StepAt::Start => ((t * n).floor() + 1., n),
                    StepAt::End => ((t * n).floor(), n),
                    StepAt::None => ((t * n).floor(), (n - 1.).max(1.)),
                    StepAt::Both => ((t * n).floor() + 1., n + 1.),
                };
                (jumped / total).clamp(0., 1.)
            }
            Easing::Points(points) => points_at(points, t),
        }
    }
}

fn steps(args: &str) -> Easing {
    let mut parts = split_outside(args, ',')
        .into_iter()
        .map(|p| p.trim().to_owned());
    let count = parts
        .next()
        .and_then(|n| n.parse::<f32>().ok())
        .filter(|n| *n > 0.)
        .unwrap_or(1.);
    let at = match parts.next().as_deref() {
        Some("start" | "jump-start") => StepAt::Start,
        Some("jump-none") => StepAt::None,
        Some("jump-both") => StepAt::Both,
        _ => StepAt::End,
    };
    Easing::Steps { count, at }
}

fn numbers(text: &str) -> Vec<f32> {
    text.split(|c: char| c == ',' || c.is_whitespace())
        .filter_map(|n| n.parse::<f32>().ok())
        .collect()
}

/// The points of `linear()` (CSS Easing 2): each an output and up to two inputs, a second input
/// making a second point of the same output. The first point with no input is at 0 and the
/// last at 1, an input before one already seen is raised to it, and points with none are
/// spread evenly between the ones that have.
fn linear_points(args: &str) -> Vec<(f32, f32)> {
    let mut items: Vec<(f32, Option<f32>)> = Vec::new();
    for part in split_outside(args, ',') {
        let mut words = part.split_whitespace();
        let Some(output) = words.next().and_then(|n| n.parse::<f32>().ok()) else {
            continue;
        };
        let inputs: Vec<f32> = words
            .filter_map(|n| n.strip_suffix('%')?.parse::<f32>().ok().map(|p| p / 100.))
            .take(2)
            .collect();
        match inputs[..] {
            [] => items.push((output, None)),
            [at] => items.push((output, Some(at))),
            [from, to] => items.extend([(output, Some(from)), (output, Some(to))]),
            _ => {}
        }
    }
    if items.is_empty() {
        return vec![(0., 0.), (1., 1.)];
    }
    let last = items.len() - 1;
    items[0].1.get_or_insert(0.);
    let top = items.iter().filter_map(|i| i.1).fold(1., f32::max);
    items[last].1.get_or_insert(top);
    let mut seen = f32::NEG_INFINITY;
    for input in items.iter_mut().filter_map(|i| i.1.as_mut()) {
        *input = input.max(seen);
        seen = *input;
    }
    let mut at = 1;
    while at < items.len() {
        if items[at].1.is_none() {
            let end = (at..items.len())
                .find(|k| items[*k].1.is_some())
                .unwrap_or(last);
            let (from, to) = (items[at - 1].1.unwrap_or(0.), items[end].1.unwrap_or(1.));
            let span = (end - (at - 1)) as f32;
            for (k, item) in items[at..end].iter_mut().enumerate() {
                item.1 = Some(from + (to - from) * (k + 1) as f32 / span);
            }
            at = end;
        }
        at += 1;
    }
    items
        .into_iter()
        .map(|(output, input)| (output, input.unwrap_or(0.)))
        .collect()
}

/// The output of `linear()` at `t`: the first point's before it, the last's after, a line
/// between two.
fn points_at(points: &[(f32, f32)], t: f32) -> f32 {
    let Some(first) = points.first() else {
        return t;
    };
    if t <= first.1 {
        return first.0;
    }
    for pair in points.windows(2) {
        let ((from_output, from_input), (output, input)) = (pair[0], pair[1]);
        if t <= input {
            let span = input - from_input;
            return if span <= f32::EPSILON {
                output
            } else {
                from_output + (output - from_output) * (t - from_input) / span
            };
        }
    }
    points.last().map_or(t, |p| p.0)
}

fn bezier(p1: f32, p2: f32, t: f32) -> f32 {
    let u = 1. - t;
    3. * u * u * t * p1 + 3. * u * t * t * p2 + t * t * t
}

/// The `t` whose curve is at `x`, by bisection: enough for a frame's worth of easing.
fn solve(x1: f32, x2: f32, x: f32) -> f32 {
    let (mut low, mut high) = (0., 1.);
    for _ in 0..24 {
        let mid = (low + high) / 2.;
        if bezier(x1, x2, mid) < x {
            low = mid;
        } else {
            high = mid;
        }
    }
    (low + high) / 2.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_easing_reaches_both_ends() {
        for easing in [
            Easing::Linear,
            Easing::parse("ease"),
            Easing::parse("ease-in-out"),
            Easing::parse("cubic-bezier(0.4, 0, 0.2, 1)"),
            Easing::parse("steps(4, end)"),
            Easing::parse("linear(0, 0.5 30%, 1)"),
        ] {
            assert!(easing.at(0.) <= 0.001, "{easing:?}");
            assert!((easing.at(1.) - 1.).abs() < 0.001, "{easing:?}");
        }
    }

    #[test]
    fn steps_jump_where_they_say() {
        let end = Easing::parse("steps(4, end)");
        assert_eq!((end.at(0.2), end.at(0.3), end.at(0.99)), (0., 0.25, 0.75));
        let start = Easing::parse("steps(4, jump-start)");
        assert_eq!((start.at(0.), start.at(0.3)), (0.25, 0.5));
        assert!(Easing::is_easing("cubic-bezier(0,0,1,1)") && !Easing::is_easing("spin"));
    }

    #[test]
    fn linear_spreads_points_and_takes_two_inputs() {
        let near = |a: f32, b: f32| assert!((a - b).abs() < 1e-4, "{a} is not {b}");
        // The point with no input falls half way between its neighbours' inputs, not by index.
        near(Easing::parse("linear(0, 0.1 80%, 0.5, 1)").at(0.85), 0.3);
        // Two inputs make a plateau.
        near(Easing::parse("linear(0, .25 25% 75%, 1)").at(0.5), 0.25);
        // An input that goes back is raised to the one before it.
        near(Easing::parse("linear(0, 1 60%, 0.5 20%, 1)").at(0.7), 0.625);
        // A cubic-bezier with an x outside 0..1 is no easing.
        assert_eq!(
            Easing::parse("cubic-bezier(1.5, 0, 0.5, 1)"),
            Easing::Linear
        );
    }
}
