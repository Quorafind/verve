//! Left hand side expression parsing.
//!
//! More information:
//!  - [MDN documentation][mdn]
//!  - [ECMAScript specification][spec]
//!
//! [mdn]: https://developer.mozilla.org/en-US/docs/Web/JavaScript/Guide/Expressions_and_Operators#Left-hand-side_expressions
//! [spec]: https://tc39.es/ecma262/#sec-left-hand-side-expressions

#[cfg(test)]
mod tests;

mod arguments;
mod call;
mod member;
mod optional;
mod template;

use crate::{
    Error,
    lexer::{InputElement, TokenKind},
    parser::{
        AllowAwait, AllowYield, Cursor, OrAbrupt, ParseResult, TokenParser,
        expression::{
            AssignmentExpression, FormalParameterListOrExpression,
            left_hand_side::{
                arguments::Arguments,
                call::{CallExpression, CallExpressionTail},
                member::MemberExpression,
                optional::OptionalExpression,
            },
            primary::arguments_to_formal_parameters,
        },
    },
    source::ReadChar,
};
use boa_ast::{
    Keyword, Position, Punctuator, Span, Spanned,
    expression::{Call, Identifier, ImportCall, ImportPhase, SuperCall},
};
use boa_interner::{Interner, Sym};

/// Parses a left hand side expression.
///
/// More information:
///  - [MDN documentation][mdn]
///  - [ECMAScript specification][spec]
///
/// [mdn]: https://developer.mozilla.org/en-US/docs/Web/JavaScript/Guide/Expressions_and_Operators#Left-hand-side_expressions
/// [spec]: https://tc39.es/ecma262/#prod-LeftHandSideExpression
#[derive(Debug, Clone, Copy)]
pub(in crate::parser) struct LeftHandSideExpression {
    allow_yield: AllowYield,
    allow_await: AllowAwait,
}

impl LeftHandSideExpression {
    /// Creates a new `LeftHandSideExpression` parser.
    pub(in crate::parser) fn new<Y, A>(allow_yield: Y, allow_await: A) -> Self
    where
        Y: Into<AllowYield>,
        A: Into<AllowAwait>,
    {
        Self {
            allow_yield: allow_yield.into(),
            allow_await: allow_await.into(),
        }
    }
}

impl<R> TokenParser<R> for LeftHandSideExpression
where
    R: ReadChar,
{
    type Output = FormalParameterListOrExpression;

    fn parse(self, cursor: &mut Cursor<R>, interner: &mut Interner) -> ParseResult<Self::Output> {
        /// Checks if we need to parse a keyword call expression `keyword()`.
        ///
        /// It first checks if the next token is `keyword`, and if it is, it checks if the second next
        /// token is the open parenthesis (`(`) punctuator.
        ///
        /// This is needed because the `if let` chain is very complex, and putting it inline in the
        /// initialization of `lhs` would make it very hard to return an expression over all
        /// possible branches of the `if let`s. Instead, we extract the check into its own function,
        /// then use it inside the condition of a simple `if ... else` expression.
        fn is_keyword_call<R: ReadChar>(
            keyword: Keyword,
            cursor: &mut Cursor<R>,
            interner: &mut Interner,
        ) -> ParseResult<Option<Position>> {
            if let Some(next) = cursor.peek(0, interner)?
                && let TokenKind::Keyword((kw, escaped)) = next.kind()
            {
                let keyword_token_start = next.span().start();
                if kw == &keyword {
                    if *escaped {
                        return Err(Error::general(
                            format!(
                                "keyword `{}` cannot contain escaped characters",
                                kw.as_str().0
                            ),
                            keyword_token_start,
                        ));
                    }
                    if let Some(next) = cursor.peek(1, interner)?
                        && next.kind() == &TokenKind::Punctuator(Punctuator::OpenParen)
                    {
                        return Ok(Some(keyword_token_start));
                    }
                }
            }
            Ok(None)
        }

        /// Checks if the next tokens form an `import.defer(` or `import.source(` pattern.
        /// Returns `Some((position, phase))` if matched, `None` otherwise.
        fn is_import_phase_call<R: ReadChar>(
            cursor: &mut Cursor<R>,
            interner: &mut Interner,
        ) -> ParseResult<Option<(Position, ImportPhase)>> {
            if let Some(next) = cursor.peek(0, interner)?
                && let TokenKind::Keyword((Keyword::Import, escaped)) = next.kind()
            {
                let keyword_token_start = next.span().start();
                if *escaped {
                    return Err(Error::general(
                        "keyword `import` cannot contain escaped characters",
                        keyword_token_start,
                    ));
                }
                if let Some(dot) = cursor.peek(1, interner)?
                    && dot.kind() == &TokenKind::Punctuator(Punctuator::Dot)
                    && let Some(ident_tok) = cursor.peek(2, interner)?
                    && let TokenKind::IdentifierName((sym, _)) = ident_tok.kind()
                {
                    let phase = match *sym {
                        Sym::DEFER => Some(ImportPhase::Defer),
                        Sym::SOURCE => Some(ImportPhase::Source),
                        _ => None,
                    };
                    if let Some(phase) = phase
                        && let Some(paren) = cursor.peek(3, interner)?
                        && paren.kind() == &TokenKind::Punctuator(Punctuator::OpenParen)
                    {
                        return Ok(Some((keyword_token_start, phase)));
                    }
                }
            }
            Ok(None)
        }

        cursor.set_goal(InputElement::TemplateTail);

        let token = cursor.peek(0, interner).or_abrupt()?;
        let async_span = token.span();
        let is_async = token.kind() == &TokenKind::Keyword((Keyword::Async, false));
        let is_async_cover = if is_async {
            let skip_n = if cursor.peek_is_line_terminator(0, interner).or_abrupt()? {
                2
            } else {
                1
            };
            !cursor
                .peek_is_line_terminator(skip_n, interner)?
                .unwrap_or(true)
                && cursor.peek(1, interner)?.is_some_and(|token| {
                    token.kind() == &TokenKind::Punctuator(Punctuator::OpenParen)
                })
        } else {
            false
        };

        if is_async_cover {
            cursor.advance(interner);
            let (args, args_span, trailing_comma) =
                Arguments::new(self.allow_yield, self.allow_await).parse_cover(cursor, interner)?;

            // Parse the cover with the normal argument grammar and lexer goals. Only `=>`
            // decides whether it is a call or an async arrow head; do not scan ahead over it.
            if cursor
                .peek(0, interner)?
                .is_some_and(|token| token.kind() == &TokenKind::Punctuator(Punctuator::Arrow))
                && !cursor.peek_is_line_terminator(0, interner).or_abrupt()?
            {
                return Ok(FormalParameterListOrExpression::AsyncArrowHead {
                    parameters: arguments_to_formal_parameters(
                        &args,
                        trailing_comma,
                        cursor.strict(),
                        args_span,
                    )?,
                    span_start: args_span.start(),
                });
            }

            let mut call = CallExpressionTail::new(
                self.allow_yield,
                self.allow_await,
                Call::new(
                    Identifier::new(Sym::ASYNC, async_span).into(),
                    args,
                    args_span,
                )
                .into(),
            )
            .parse(cursor, interner)?;
            if cursor
                .peek(0, interner)?
                .is_some_and(|token| token.kind() == &TokenKind::Punctuator(Punctuator::Optional))
            {
                call = OptionalExpression::new(self.allow_yield, self.allow_await, call)
                    .parse(cursor, interner)?
                    .into();
            }
            return Ok(call.into());
        }

        let mut lhs: FormalParameterListOrExpression =
            if let Some(start) = is_keyword_call(Keyword::Super, cursor, interner)? {
                cursor.advance(interner);
                let (args, args_span) =
                    Arguments::new(self.allow_yield, self.allow_await).parse(cursor, interner)?;
                // `super(...)` is a whole CallExpression: `super().then(f)`, `super()[k]`,
                // `super()()` and a tagged template go on from it like any other call.
                CallExpressionTail::new(
                    self.allow_yield,
                    self.allow_await,
                    SuperCall::new(args, Span::new(start, args_span.end())).into(),
                )
                .parse(cursor, interner)?
                .into()
            } else if let Some(start) = is_keyword_call(Keyword::Import, cursor, interner)? {
                // Plain `import(...)` call
                cursor.advance(interner);
                // `(`
                cursor.advance(interner);

                let specifier = AssignmentExpression::new(true, self.allow_yield, self.allow_await)
                    .parse(cursor, interner)?;

                let options =
                    if cursor
                        .next_if(TokenKind::Punctuator(Punctuator::Comma), interner)?
                        .is_some()
                    {
                        if cursor.peek(0, interner)?.is_some_and(|t| {
                            t.kind() == &TokenKind::Punctuator(Punctuator::CloseParen)
                        }) {
                            None
                        } else {
                            let opts =
                                AssignmentExpression::new(true, self.allow_yield, self.allow_await)
                                    .parse(cursor, interner)?;
                            if cursor.peek(0, interner)?.is_some_and(|t| {
                                t.kind() == &TokenKind::Punctuator(Punctuator::Comma)
                            }) {
                                cursor.advance(interner);
                            }
                            Some(opts)
                        }
                    } else {
                        None
                    };

                let end = cursor
                    .expect(
                        TokenKind::Punctuator(Punctuator::CloseParen),
                        "import call",
                        interner,
                    )?
                    .span()
                    .end();

                CallExpressionTail::new(
                    self.allow_yield,
                    self.allow_await,
                    ImportCall::new(
                        specifier,
                        options,
                        ImportPhase::Evaluation,
                        Span::new(start, end),
                    )
                    .into(),
                )
                .parse(cursor, interner)?
                .into()
            } else if let Some((start, phase)) = is_import_phase_call(cursor, interner)? {
                // `import.defer(...)` or `import.source(...)` call
                // Consume `import`
                cursor.advance(interner);
                // Consume `.`
                cursor.advance(interner);
                // Consume `defer` or `source`
                cursor.advance(interner);
                // Consume `(`
                cursor.advance(interner);

                let specifier = AssignmentExpression::new(true, self.allow_yield, self.allow_await)
                    .parse(cursor, interner)?;

                let options =
                    if cursor
                        .next_if(TokenKind::Punctuator(Punctuator::Comma), interner)?
                        .is_some()
                    {
                        if cursor.peek(0, interner)?.is_some_and(|t| {
                            t.kind() == &TokenKind::Punctuator(Punctuator::CloseParen)
                        }) {
                            None
                        } else {
                            let opts =
                                AssignmentExpression::new(true, self.allow_yield, self.allow_await)
                                    .parse(cursor, interner)?;
                            if cursor.peek(0, interner)?.is_some_and(|t| {
                                t.kind() == &TokenKind::Punctuator(Punctuator::Comma)
                            }) {
                                cursor.advance(interner);
                            }
                            Some(opts)
                        }
                    } else {
                        None
                    };

                let end = cursor
                    .expect(
                        TokenKind::Punctuator(Punctuator::CloseParen),
                        "import call",
                        interner,
                    )?
                    .span()
                    .end();

                CallExpressionTail::new(
                    self.allow_yield,
                    self.allow_await,
                    ImportCall::new(specifier, options, phase, Span::new(start, end)).into(),
                )
                .parse(cursor, interner)?
                .into()
            } else {
                let member = MemberExpression::new(self.allow_yield, self.allow_await)
                    .parse(cursor, interner)?;
                if let Some(tok) = cursor.peek(0, interner)?
                    && tok.kind() == &TokenKind::Punctuator(Punctuator::OpenParen)
                {
                    CallExpression::new(
                        self.allow_yield,
                        self.allow_await,
                        member.try_into_expression()?,
                    )
                    .parse(cursor, interner)?
                    .into()
                } else {
                    member
                }
            };

        if let Some(tok) = cursor.peek(0, interner)?
            && tok.kind() == &TokenKind::Punctuator(Punctuator::Optional)
        {
            lhs = OptionalExpression::new(
                self.allow_yield,
                self.allow_await,
                lhs.try_into_expression()?,
            )
            .parse(cursor, interner)?
            .into();
        }

        Ok(lhs)
    }
}
