use crate::parser::tests::check_script_parser;
use boa_ast::{
    Expression, Span, Statement,
    expression::{Call, Identifier, access::SimplePropertyAccess},
};
use boa_interner::Interner;
use boa_macros::utf16;

macro_rules! check_call_property_identifier {
    ($property:literal) => {{
        let interner = &mut Interner::default();
        let input = format!("a().{}", $property);
        #[allow(clippy::cast_possible_truncation)]
        let input_end = input.len() as u32 + 1;
        check_script_parser(
            input.as_str(),
            vec![
                Statement::Expression(Expression::PropertyAccess(
                    SimplePropertyAccess::new(
                        Call::new(
                            Identifier::new(
                                interner.get_or_intern_static("a", utf16!("a")),
                                Span::new((1, 1), (1, 2)),
                            )
                            .into(),
                            Box::default(),
                            Span::new((1, 2), (1, 4)),
                        )
                        .into(),
                        Identifier::new(
                            interner.get_or_intern_static($property, utf16!($property)),
                            Span::new((1, 5), (1, input_end)),
                        ),
                    )
                    .into(),
                ))
                .into(),
            ],
            interner,
        );
    }};
}

#[test]
fn check_call_properties() {
    check_call_property_identifier!("prop");
    check_call_property_identifier!("true");
    check_call_property_identifier!("false");
    check_call_property_identifier!("null");
    check_call_property_identifier!("let");
}

macro_rules! check_member_property_identifier {
    ($property:literal) => {{
        let interner = &mut Interner::default();
        let input = format!("a.{}", $property);
        #[allow(clippy::cast_possible_truncation)]
        let input_end = input.len() as u32 + 1;
        check_script_parser(
            input.as_str(),
            vec![
                Statement::Expression(Expression::PropertyAccess(
                    SimplePropertyAccess::new(
                        Identifier::new(
                            interner.get_or_intern_static("a", utf16!("a")),
                            Span::new((1, 1), (1, 2)),
                        )
                        .into(),
                        Identifier::new(
                            interner.get_or_intern_static($property, utf16!($property)),
                            Span::new((1, 3), (1, input_end)),
                        ),
                    )
                    .into(),
                ))
                .into(),
            ],
            interner,
        );
    }};
}

#[test]
fn check_member_properties() {
    check_member_property_identifier!("prop");
    check_member_property_identifier!("true");
    check_member_property_identifier!("false");
    check_member_property_identifier!("null");
    check_member_property_identifier!("let");
}

/// `super(...)` is a whole `CallExpression`, so member accesses, calls, optional chains and
/// tagged templates go on from it (`return super(x).then(f)` in bundled code).
#[test]
fn super_call_is_a_call_expression() {
    use crate::{Parser, Source};
    use boa_ast::scope::Scope;

    for tail in [
        ".then(r => r)",
        ".x",
        "[k]",
        "()",
        "?.x",
        "?.[k]()",
        "`t`",
        ".a.b(c)[d]`e${f}`",
    ] {
        let js = format!(
            "class A extends B {{ constructor(k) {{ return super(k){tail}; }} }}\n\
             class C extends B {{ constructor(k) {{ super(k){tail}; }} }}"
        );
        let result = Parser::new(Source::from_bytes(js.as_str()))
            .parse_script(&Scope::new_global(), &mut Interner::default());
        assert!(result.is_ok(), "`{js}` failed to parse: {result:?}");
    }
}

/// `super(...)` still only goes in a derived class constructor, nested in a chain or not.
#[test]
fn super_call_chain_outside_a_constructor_is_an_error() {
    crate::parser::tests::check_invalid_script("class A extends B { m() { super().x; } }");
    crate::parser::tests::check_invalid_script("function f() { return super().then(g); }");
}
