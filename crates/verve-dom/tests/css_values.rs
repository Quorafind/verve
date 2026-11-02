//! Values: what each colour notation comes to in sRGB, what `getComputedStyle` writes back, and
//! how `calc()` folds into pixels or a `calc()` the drawer resolves.

mod common;

use common::{on_page, style, styled};
use verve_dom::css::{color, color_with};

/// Asserts two colours are the same, channel by channel.
fn near(actual: [f32; 4], expected: [f32; 4]) {
    for (a, e) in actual.iter().zip(expected) {
        assert!((a - e).abs() < 0.01, "{actual:?} is not {expected:?}");
    }
}

#[test]
fn every_color_notation_comes_to_srgb() {
    // The notations there were, and the ones Tailwind v4 writes.
    near(color("#1e90ff").unwrap(), [0.118, 0.565, 1., 1.]);
    near(color("#0f08").unwrap(), [0., 1., 0., 0.533]);
    near(color("rgb(30 144 255)").unwrap(), [0.118, 0.565, 1., 1.]);
    near(color("rgba(0, 0, 0, 0.5)").unwrap(), [0., 0., 0., 0.5]);
    near(color("hsl(210 100% 56%)").unwrap(), [0.12, 0.56, 1., 1.]);
    near(color("hsl(120 100% 25%)").unwrap(), [0., 0.5, 0., 1.]);
    near(color("rebeccapurple").unwrap(), [0.4, 0.2, 0.6, 1.]);
    near(color("transparent").unwrap(), [0., 0., 0., 0.]);
    near(color("hwb(120 0% 0%)").unwrap(), [0., 1., 0., 1.]);
    near(color("hwb(0 50% 50%)").unwrap(), [0.5, 0.5, 0.5, 1.]);
    near(
        color("oklch(0.62796 0.25768 29.234)").unwrap(),
        [1., 0., 0., 1.],
    );
    near(
        color("oklch(62.8% 0.2577 29.23)").unwrap(),
        [1., 0., 0., 1.],
    );
    near(
        color("oklab(0.6279 0.2249 0.1258)").unwrap(),
        [1., 0., 0., 1.],
    );
    // `lab()` and `lch()` are D50: red is not the same numbers as in D65.
    near(color("lab(54.29 80.8 69.89)").unwrap(), [1., 0., 0., 1.]);
    near(color("lch(54.29 106.83 40.85)").unwrap(), [1., 0., 0., 1.]);
    near(color("color(srgb 0 1 0)").unwrap(), [0., 1., 0., 1.]);
    near(color("oklch(1 0 0)").unwrap(), [1., 1., 1., 1.]);
    near(color("oklch(0 0 0)").unwrap(), [0., 0., 0., 1.]);
    // A grey has no hue and equal channels in every space.
    let grey = color("oklch(0.7 0 0)").unwrap();
    assert!((grey[0] - grey[1]).abs() < 0.01 && (grey[1] - grey[2]).abs() < 0.01);
    // Out-of-gamut channels are clipped, as a display does.
    let clipped = color("oklch(0.7 0.4 150)").unwrap();
    assert!(
        clipped.iter().all(|c| (0. ..=1.).contains(c)),
        "{clipped:?}"
    );
    assert_eq!(color("not-a-color"), None);
    assert_eq!(color("rgb(1 2)"), None);
}

#[test]
fn rgb_takes_percentages_for_its_channels() {
    // A percentage is of 255, not a number to be read as zero.
    near(color("rgb(50% 0% 0%)").unwrap(), [0.5, 0., 0., 1.]);
    near(color("rgb(100%, 50%, 0%)").unwrap(), [1., 0.5, 0., 1.]);
    near(color("rgb(0% 0% 100% / 25%)").unwrap(), [0., 0., 1., 0.25]);
}

#[test]
fn hsl_and_hwb_take_bare_numbers_and_chroma_takes_percentages() {
    // `hsl(120 100 50)` is green: the numbers are percentages without the sign.
    near(color("hsl(120 100 50)").unwrap(), [0., 1., 0., 1.]);
    near(color("hsl(120, 100, 25)").unwrap(), [0., 0.5, 0., 1.]);
    near(color("hwb(120 0 0)").unwrap(), [0., 1., 0., 1.]);
    near(color("hwb(0 50 50)").unwrap(), [0.5, 0.5, 0.5, 1.]);
    // 100% of a chroma or an axis is 0.4 in oklab and oklch, 150 in lch and 125 in lab.
    near(
        color("oklch(70% 50% 30)").unwrap(),
        color("oklch(0.7 0.2 30)").unwrap(),
    );
    near(
        color("oklab(70% 50% -50%)").unwrap(),
        color("oklab(0.7 0.2 -0.2)").unwrap(),
    );
    near(
        color("lch(50% 40% 30)").unwrap(),
        color("lch(50 60 30)").unwrap(),
    );
    near(
        color("lab(50% 40% -40%)").unwrap(),
        color("lab(50 50 -50)").unwrap(),
    );
}

#[test]
fn hwb_is_a_legacy_form_and_computes_to_rgb() {
    on_page(|| {
        let mut page = styled(
            "#target { color: hwb(120 0% 0%); background-color: hwb(0 0% 0% / 0.5) }",
            r#"<div id="target">t</div>"#,
        );
        assert_eq!(style(&mut page, "target", "color"), "rgb(0, 255, 0)");
        assert_eq!(
            style(&mut page, "target", "background-color"),
            "rgba(255, 0, 0, 0.5)"
        );
    });
}

#[test]
fn currentcolor_is_the_color_the_caller_gives() {
    near(color("currentcolor").unwrap(), [0., 0., 0., 1.]);
    near(
        color_with("currentcolor", [0.5, 0.25, 0.75, 1.]).unwrap(),
        [0.5, 0.25, 0.75, 1.],
    );
    near(
        color_with("color-mix(in srgb, currentcolor, white)", [0., 0., 0., 1.]).unwrap(),
        [0.5, 0.5, 0.5, 1.],
    );
}

#[test]
fn color_mixes_weigh_their_operands() {
    near(
        color("color-mix(in srgb, red, blue)").unwrap(),
        [0.5, 0., 0.5, 1.],
    );
    near(
        color("color-mix(in srgb, red 25%, blue)").unwrap(),
        [0.25, 0., 0.75, 1.],
    );
    near(
        color("color-mix(in srgb, blue, 25% red)").unwrap(),
        [0.25, 0., 0.75, 1.],
    );
    near(
        color("color-mix(in srgb, red 40%, blue 60%)").unwrap(),
        [0.4, 0., 0.6, 1.],
    );
    // Weights that add up to less than 100% leave the mix that much more transparent.
    near(
        color("color-mix(in srgb, red 30%, blue 30%)").unwrap(),
        [0.5, 0., 0.5, 0.6],
    );
    // In oklab the mix is not the midpoint of the channels but of what they look like: half
    // of white is a mid grey, a little dark.
    let grey = color("color-mix(in oklab, white, black)").unwrap();
    assert!(grey[0] > 0.35 && grey[0] < 0.45, "{grey:?}");
    assert!((grey[0] - grey[1]).abs() < 0.01 && (grey[1] - grey[2]).abs() < 0.01);
    // A hue mixes the short way round: red and blue meet at magenta, not through green.
    let magenta = color("color-mix(in hsl, red, blue)").unwrap();
    assert!(
        magenta[0] > 0.9 && magenta[2] > 0.9 && magenta[1] < 0.1,
        "{magenta:?}"
    );
}

#[test]
fn a_mix_with_transparent_keeps_the_color_and_loses_alpha() {
    // Tailwind v4's `bg-red-500/50`: the other half is transparent, which is no color at all.
    // Mixed premultiplied the color stays what it was, in every space — darkening it
    // would be mixing it with black.
    let red = color("oklch(63.7% 0.237 25.331)").unwrap();
    for space in ["oklab", "oklch", "srgb", "hsl"] {
        let mixed = color(&format!(
            "color-mix(in {space}, oklch(63.7% 0.237 25.331) 50%, transparent)"
        ))
        .unwrap();
        near(mixed, [red[0], red[1], red[2], 0.5]);
        let reversed = color(&format!(
            "color-mix(in {space}, transparent, oklch(63.7% 0.237 25.331) 50%)"
        ))
        .unwrap();
        near(reversed, [red[0], red[1], red[2], 0.5]);
    }
}

#[test]
fn a_mix_reads_operands_that_hold_commas() {
    near(
        color("color-mix(in srgb, rgb(255, 0, 0) 30%, rgb(0, 0, 255))").unwrap(),
        [0.3, 0., 0.7, 1.],
    );
    near(
        color("color-mix(in srgb, rgba(255, 0, 0, 0.5), hsl(240, 100%, 50%) 50%)").unwrap(),
        [0.333, 0., 0.667, 0.75],
    );
}

#[test]
fn relative_colors_name_the_channels_of_the_color_they_start_from() {
    // Naming every channel gives the same color again.
    near(
        color("oklch(from #1e90ff l c h)").unwrap(),
        color("#1e90ff").unwrap(),
    );
    near(
        color("rgb(from red r g b / 50%)").unwrap(),
        [1., 0., 0., 0.5],
    );
    near(color("hsl(from red h s l)").unwrap(), [1., 0., 0., 1.]);
    // Half the lightness of red keeps its hue.
    let dim = color("oklch(from red 0.5 c h)").unwrap();
    assert!(dim[0] > 0.7 && dim[0] < 0.95, "{dim:?}");
    // The color it starts from may hold parentheses of its own.
    near(
        color("rgb(from color-mix(in srgb, red, blue) r g b)").unwrap(),
        [0.5, 0., 0.5, 1.],
    );
}

#[test]
fn a_computed_color_is_rgb_unless_it_keeps_its_space() {
    on_page(|| {
        let mut page = styled(
            "#target {
               color: #112233;
               border-top-color: hsl(120 100% 25%);
               outline-color: rebeccapurple;
               background-color: oklch(63.7% 0.237 25.331);
               caret-color: rgba(0, 0, 0, 0.5);
               text-decoration-color: currentcolor;
               fill: transparent;
             }",
            r#"<div id="target">t</div>"#,
        );
        assert_eq!(style(&mut page, "target", "color"), "rgb(17, 34, 51)");
        assert_eq!(
            style(&mut page, "target", "border-top-color"),
            "rgb(0, 128, 0)"
        );
        assert_eq!(
            style(&mut page, "target", "outline-color"),
            "rgb(102, 51, 153)"
        );
        // A color of the newer spaces keeps the text it was written with.
        assert_eq!(
            style(&mut page, "target", "background-color"),
            "oklch(63.7% 0.237 25.331)"
        );
        assert_eq!(
            style(&mut page, "target", "caret-color"),
            "rgba(0, 0, 0, 0.5)"
        );
        assert_eq!(style(&mut page, "target", "fill"), "rgba(0, 0, 0, 0)");
        // `currentcolor` in another color property is the element's own color.
        assert_eq!(
            style(&mut page, "target", "text-decoration-color"),
            "rgb(17, 34, 51)"
        );
        assert_eq!(page.console(), "");
    });
}

#[test]
fn a_variable_holding_a_color_reads_as_one() {
    on_page(|| {
        let mut page = styled(
            ":root { --brand: oklch(63.7% 0.237 25.331); --legacy: #008000 }
             #target { color: var(--legacy); background-color: var(--brand) }",
            r#"<div id="target">t</div>"#,
        );
        assert_eq!(style(&mut page, "target", "color"), "rgb(0, 128, 0)");
        assert_eq!(
            style(&mut page, "target", "background-color"),
            "oklch(63.7% 0.237 25.331)"
        );
        // The custom property itself keeps what it was given, as a browser's does.
        assert_eq!(
            style(&mut page, "target", "--brand"),
            "oklch(63.7% 0.237 25.331)"
        );
    });
}

#[test]
fn calc_folds_absolute_lengths_into_pixels() {
    on_page(|| {
        let mut page = styled(
            "#target {
               --step: 6px;
               padding-left: calc(2 * var(--step));
               margin-top: calc(1rem + 2px);
               height: min(10px, 2em);
               border-top-width: max(1px, 2px);
               letter-spacing: calc(0.5px * 3);
               z-index: calc(2 + 3);
               font-size: calc(1em + 2px);
               line-height: calc(1.5 * 2);
               margin: 0 calc(1px + 2px);
             }",
            r#"<div id="target">t</div>"#,
        );
        assert_eq!(style(&mut page, "target", "padding-left"), "12px");
        assert_eq!(style(&mut page, "target", "border-top-width"), "2px");
        assert_eq!(style(&mut page, "target", "letter-spacing"), "1.5px");
        assert_eq!(style(&mut page, "target", "z-index"), "5");
        // A font size is computed first: `1em` is the parent's 16.
        assert_eq!(style(&mut page, "target", "font-size"), "18px");
        assert_eq!(style(&mut page, "target", "line-height"), "3");
        // A function in a shorthand is folded in the longhand it lands in.
        assert_eq!(style(&mut page, "target", "margin-right"), "3px");
        assert_eq!(page.console(), "");
    });
}

#[test]
fn em_and_rem_are_measured_from_the_font_sizes_they_belong_to() {
    on_page(|| {
        let mut page = styled(
            "html { font-size: 20px }
             #target { font-size: 10px; margin-top: calc(1rem + 2px); height: calc(2em + 1rem) }
             #big { font-size: 1.5rem }
             #wide { font-size: calc(1rem + 100%) }",
            r#"<div id="target">t</div><div id="big"></div><div id="wide"></div>"#,
        );
        // `rem` is the root element's font size, `em` the element's own.
        assert_eq!(style(&mut page, "target", "margin-top"), "22px");
        assert_eq!(style(&mut page, "target", "height"), "40px");
        assert_eq!(style(&mut page, "big", "font-size"), "30px");
        // A percentage in a font size is of the parent's.
        assert_eq!(style(&mut page, "wide", "font-size"), "40px");
        // Changing the root's size changes what its descendants' `rem` come to.
        page.eval("document.documentElement.style.fontSize = '10px'");
        assert_eq!(style(&mut page, "target", "margin-top"), "12px");
    });
}

#[test]
fn a_calc_with_a_percentage_keeps_it_for_the_drawer() {
    on_page(|| {
        let mut page = styled(
            "#target {
               width: calc(100% - 12px);
               height: calc((100% - 20px) / 2);
               max-width: calc(50% + 10px);
               top: calc(25%);
               left: min(50%, 10px);
             }",
            r#"<div id="target">t</div>"#,
        );
        assert_eq!(style(&mut page, "target", "width"), "calc(100% - 12px)");
        assert_eq!(style(&mut page, "target", "height"), "calc(50% - 10px)");
        assert_eq!(style(&mut page, "target", "max-width"), "calc(50% + 10px)");
        assert_eq!(style(&mut page, "target", "top"), "25%");
        // `min()` of a percentage and a length needs the layout, so it keeps its text.
        assert_eq!(style(&mut page, "target", "left"), "min(50%, 10px)");
        assert_eq!(page.console(), "");
    });
}

#[test]
fn a_slash_is_not_division_outside_a_math_function() {
    on_page(|| {
        // `col-span-full` and `aspect-video`: two numbers and a slash are a line range and a
        // ratio, and stay what they were written as.
        let mut page = styled(
            "#target {
               grid-column: 1 / -1;
               grid-row: 2 / span 2;
               aspect-ratio: 16 / 9;
               font: 12px/1.5 serif;
             }",
            r#"<div id="target">t</div>"#,
        );
        assert_eq!(style(&mut page, "target", "grid-column"), "1 / -1");
        assert_eq!(style(&mut page, "target", "grid-row"), "2 / span 2");
        assert_eq!(style(&mut page, "target", "aspect-ratio"), "16 / 9");
        assert_eq!(style(&mut page, "target", "line-height"), "1.5");
        assert_eq!(page.console(), "");
    });
}

#[test]
fn clamp_prefers_its_minimum_when_the_bounds_cross() {
    on_page(|| {
        // CSS Values 4: `clamp(MIN, VAL, MAX)` is `max(MIN, min(VAL, MAX))`, so MIN wins.
        let mut page = styled(
            "#target {
               width: clamp(100px, 5px, 20px);
               height: clamp(10px, 50px, 20px);
               top: clamp(10px, 15px, 20px);
               left: clamp(10px, 5px, 20px);
             }",
            r#"<div id="target">t</div>"#,
        );
        assert_eq!(style(&mut page, "target", "width"), "100px");
        assert_eq!(style(&mut page, "target", "height"), "20px");
        assert_eq!(style(&mut page, "target", "top"), "15px");
        assert_eq!(style(&mut page, "target", "left"), "10px");
    });
}

#[test]
fn durations_keep_their_unit_and_only_a_number_divides() {
    on_page(|| {
        let mut page = styled(
            "#target {
               transition-duration: calc(0.3s);
               transition-delay: calc(1s + 500ms);
               animation-duration: calc(300ms);
               width: calc(10px / 2px);
               height: calc(10px / 2);
               top: calc(10px / 0);
             }",
            r#"<div id="target">t</div>"#,
        );
        // Chrome: `0.3s` and `1.5s` — a time is written in seconds, as a duration is.
        assert_eq!(style(&mut page, "target", "transition-duration"), "0.3s");
        assert_eq!(style(&mut page, "target", "transition-delay"), "1.5s");
        assert_eq!(style(&mut page, "target", "animation-duration"), "0.3s");
        // Chrome drops a length over a length as invalid, so it is kept as written here; a
        // division by zero is infinity, which is the largest length.
        assert_eq!(style(&mut page, "target", "width"), "calc(10px / 2px)");
        assert_eq!(style(&mut page, "target", "height"), "5px");
        assert_eq!(style(&mut page, "target", "top"), "33554400px");
    });
}

#[test]
fn lengths_are_read_by_one_table_of_units() {
    use verve_dom::css::length;
    assert_eq!(length("1.5em", 10.), Some(15.));
    assert_eq!(length("2rem", 10.), Some(32.));
    assert_eq!(length("12pt", 10.), Some(16.));
    assert_eq!(length("0", 10.), Some(0.));
    assert_eq!(length("-3px", 10.), Some(-3.));
    assert_eq!(length("calc(1px + 2px)", 10.), Some(3.));
    // A percentage is of something, and arithmetic outside a function is not a length.
    assert_eq!(length("50%", 10.), None);
    assert_eq!(length("1 / -1", 10.), None);
    assert_eq!(length("16 / 9", 10.), None);
    assert_eq!(length("45deg", 10.), None);
}

#[test]
fn the_drawer_resolves_a_length_against_its_basis() {
    let env = verve_dom::Environment {
        width: 800.,
        height: 600.,
        ..verve_dom::Environment::default()
    };
    let calc = |text: &str| verve_dom::css::length_calc(text, 16., 16., &env);
    let half = calc("calc(50% - 10px)").unwrap();
    assert_eq!((half.px, half.percent), (-10., 50.));
    assert_eq!(calc("10vw").unwrap().px, 80.);
    assert_eq!(calc("5vh").unwrap().px, 30.);
    assert_eq!(calc("2em").unwrap().px, 32.);
    assert_eq!(calc("calc(1rem + 2px)").unwrap().px, 18.);
    assert_eq!(calc("50%").unwrap().percent, 50.);
    // A container unit is the layout's to know; so is what is not a length.
    assert_eq!(calc("10cqw"), None);
    assert_eq!(calc("auto"), None);
}

#[test]
fn infinity_is_the_largest_length_and_makes_a_pill() {
    on_page(|| {
        // Tailwind v4's `rounded-full` is `calc(infinity * 1px)`: a radius so large the corners
        // meet. Chrome writes it `3.35544e+07px`; the value is the same.
        let mut page = styled(
            "#target {
               border-radius: calc(infinity * 1px);
               top: calc(-infinity * 1px);
               left: calc(NaN * 1px);
               width: calc(pi * 1px);
               height: calc(e * 10px);
               z-index: calc(1 + infinity);
             }",
            r#"<div id="target">t</div>"#,
        );
        assert_eq!(
            style(&mut page, "target", "border-top-left-radius"),
            "33554400px"
        );
        assert_eq!(style(&mut page, "target", "top"), "-33554400px");
        assert_eq!(style(&mut page, "target", "left"), "0px");
        assert_eq!(style(&mut page, "target", "width"), "3.142px");
        assert_eq!(style(&mut page, "target", "height"), "27.183px");
    });
}

#[test]
fn angles_are_read_with_the_same_units() {
    near(color("hsl(0.5turn 100% 50%)").unwrap(), [0., 1., 1., 1.]);
    near(color("hsl(200grad 100% 50%)").unwrap(), [0., 1., 1., 1.]);
    near(color("hwb(3.14159rad 0% 0%)").unwrap(), [0., 1., 1., 1.]);
}
