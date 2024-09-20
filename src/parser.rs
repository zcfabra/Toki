use crate::ast::{
    AstBinExpr, AstBlock, AstConditional, AstExpr, AstLiteral, AstNode,
    AstStmt, TypeAnnotation,
};
use crate::lexer::{LexErr, Result as LexResult};
use crate::token::{SpannedToken, Token};
use core::iter::Peekable;

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Precedence {
    Lowest,
    AddSub,
    MulDiv,
    Equality,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseErr {
    LexErr(LexErr),
    InvalidExpressionStart(usize, usize),

    UnexpectedEnd,
    UnexpectedIndent(usize, usize, usize),
    UnexpectedStmt(usize, usize),

    ExpectedTypeAnnotation(usize, usize),
    ExpectedNewline(usize, usize),
    ExpectedSemi(usize, usize),
    ExpectedColon(usize, usize),
    ExpectedFnName(usize, usize),

    // TODO: Make this an &str once or &Token once lifetime is removed
    ExpectedToken(usize, usize, String),
}

#[derive(Debug, Clone, Copy)]
struct ParseContext {
    can_parse_annotation: bool,
    is_in_paren_block: bool,
}

impl ParseContext {
    fn new() -> Self {
        ParseContext {
            can_parse_annotation: true,
            is_in_paren_block: false,
        }
    }
    fn entering_parens(mut self: Self) -> Self {
        self.is_in_paren_block = true;
        self
    }
    fn exiting_parens(mut self: Self) -> Self {
        self.is_in_paren_block = true;
        self
    }
    fn with_annotation_parsing(mut self: Self) -> Self {
        self.can_parse_annotation = true;
        self
    }
    fn without_annotation_parsing(mut self: Self) -> Self {
        self.can_parse_annotation = false;
        self
    }
}

type TokenIter<'src> = LexResult<SpannedToken<'src>>;

type Result<T> = std::result::Result<T, ParseErr>;

pub fn get_next_token<'src, I>(
    tokens: &mut Peekable<I>,
) -> Result<SpannedToken<'src>>
where
    I: Iterator<Item = TokenIter<'src>>,
{
    match tokens.next() {
        Some(Ok(tok)) => Ok(tok),
        Some(Err(e)) => Err(ParseErr::LexErr(e)),
        None => Err(ParseErr::UnexpectedEnd),
    }
}

pub fn parse<'src, I>(tokens: I) -> Result<AstBlock<'src>>
where
    I: Iterator<Item = TokenIter<'src>>,
{
    // Entry point of the parser
    let peekable_tokens = &mut tokens.peekable();
    return Ok(parse_block(peekable_tokens)?);
}

pub fn get_block_indent<'src, I>(tokens: &mut Peekable<I>) -> usize
where
    I: Iterator<Item = TokenIter<'src>>,
{
    if let Some(Ok((_, Token::Spaces(n_spaces)))) = tokens.peek() {
        let indent = n_spaces / 4;
        tokens.next();
        indent
    } else {
        0
    }
}
pub fn parse_block<'src, I>(tokens: &mut Peekable<I>) -> Result<AstBlock<'src>>
where
    I: Iterator<Item = TokenIter<'src>>,
{
    let mut stmts = Vec::new();
    let mut has_no_semi_expr = false;

    // Need to handle no spaces to deal w/ the very first statement in the program
    let indent = get_block_indent(tokens);

    loop {
        match tokens.peek() {
            None => break,
            Some(Ok((_, Token::Newline))) => {
                tokens.next();
            }
            Some(Ok((_, Token::Spaces(n_spaces)))) => {
                let found_indent = n_spaces / 4;
                if found_indent < indent {
                    tokens.next();
                    if !matches!(tokens.peek(), Some(Ok((_, Token::Newline))))
                    {
                        break;
                    }
                } else if found_indent > indent {
                    let (ix, tok) = get_next_token(tokens)?;
                    return Err(ParseErr::UnexpectedIndent(
                        ix,
                        tok.src_len(),
                        indent,
                    ));
                } else {
                    tokens.next();
                }
            }
            Some(Ok((ix, tok))) => {
                if has_no_semi_expr {
                    return Err(ParseErr::UnexpectedStmt(*ix, tok.src_len()));
                }

                let stmt = parse_stmt(tokens)?;

                if matches!(
                    stmt,
                    AstStmt::Expr {
                        has_semi: false,
                        ..
                    }
                ) {
                    has_no_semi_expr = true;
                }

                stmts.push(stmt);
            }
            Some(Err(_)) => todo!(),
        };
    }

    let block = AstBlock {
        indent,
        stmts,
        has_semi: !has_no_semi_expr,
    };
    Ok(block)
}
pub fn parse_stmt<'src, I>(tokens: &mut Peekable<I>) -> Result<AstStmt<'src>>
where
    I: Iterator<Item = TokenIter<'src>>,
{
    // If a statement starts w/ an identifier, it could be
    // 1. An assignment `a = 10;`
    // 2. A mutation statement `a += 10;`
    // 3. The beginning of an expr `a + 10`
    // 4. A call statement some_fn();
    // 5. A call expression some_fn()

    let context = ParseContext::new();
    if matches!(tokens.peek(), Some(Ok((_, Token::Return)))) {
        tokens.next();

        let expr = parse_expr(tokens, Precedence::Lowest, context)?;

        eat(tokens, Token::Semicolon)?;

        return Ok(AstStmt::Return(expr));
    }

    if matches!(tokens.peek(), Some(Ok((_, Token::Def)))) {
        return Ok(parse_fn_def(tokens)?);
    }

    let primary_expr = parse_primary_expr(tokens, context)?;

    if let Some(Ok((_, Token::Eq))) = tokens.peek() {
        tokens.next();
        let to_assign = parse_expr(tokens, Precedence::Lowest, context)?;

        eat(tokens, Token::Semicolon)?;

        return Ok(AstStmt::Assignment {
            target: primary_expr,
            assigned: to_assign,
        });
    }

    let expr =
        parse_expr_with(primary_expr, tokens, Precedence::Lowest, context)?;

    let has_semi_next =
        matches!(tokens.peek(), Some(Ok((_, Token::Semicolon))));
    if has_semi_next {
        eat(tokens, Token::Semicolon)?;
    }

    let has_semi = expr_has_semi(&expr, has_semi_next);
    Ok(AstStmt::Expr { expr, has_semi })
}

fn parse_fn_args<'src, I>(
    tokens: &mut Peekable<I>,
) -> Result<Vec<AstLiteral<'src>>>
where
    I: Iterator<Item = TokenIter<'src>>,
{
    let mut args = Vec::new();

    loop {
        if !matches!(tokens.peek(), Some(Ok((_, Token::Ident(_))))) {
            break;
        }

        let (_, tok) = get_next_token(tokens)?;

        args.push(AstLiteral::TypedIdent {
            name: tok,
            type_annotation: parse_annotation(tokens)?,
        });

        if matches!(tokens.peek(), Some(Ok((_, Token::Comma)))) {
            tokens.next();
        }
    }
    eat(tokens, Token::RParen)?;
    Ok(args)
}

pub fn parse_fn_def<'src, I>(tokens: &mut Peekable<I>) -> Result<AstStmt<'src>>
where
    I: Iterator<Item = TokenIter<'src>>,
{
    tokens.next();
    let (ix, fn_name) = get_next_token(tokens)?;

    if !matches!(fn_name, Token::Ident(_)) {
        return Err(ParseErr::ExpectedFnName(ix, fn_name.src_len()));
    }
    let name = AstLiteral::Ident(fn_name);

    eat(tokens, Token::LParen)?;

    let args = parse_fn_args(tokens)?;

    eat(tokens, Token::Arrow)?;
    let return_type = parse_type_decl(tokens)?;

    eat(tokens, Token::Colon)?;
    eat(tokens, Token::Newline)?;

    if !matches!(tokens.peek(), Some(Ok((_, Token::Spaces(spaces)))) if *spaces > 0)
    {
        let (ix, tok) = get_next_token(tokens)?;
        return Err(ParseErr::UnexpectedIndent(ix, tok.src_len(), 1));
    }

    let body = parse_block(tokens)?;

    return Ok(AstStmt::FnDef {
        name,
        args,
        body,
        return_type,
    });
}

fn parse_type_decl<'src, I>(
    tokens: &mut Peekable<I>,
) -> Result<TypeAnnotation<'src>>
where
    I: Iterator<Item = TokenIter<'src>>,
{
    let is_mut = if matches!(tokens.peek(), Some(Ok((_, Token::Mut)))) {
        tokens.next();
        true
    } else {
        false
    };

    let (ix, tok) = match tokens.next() {
        Some(Ok(spanned)) => spanned,
        Some(Err(e)) => return Err(ParseErr::LexErr(e)),
        None => return Err(ParseErr::UnexpectedEnd),
    };

    Ok(match tok {
        Token::Ident(id) => {
            let type_ = TypeAnnotation::Dynamic(id);
            if is_mut {
                TypeAnnotation::Mut(Box::new(type_))
            } else {
                type_
            }
        }
        _ => return Err(ParseErr::ExpectedTypeAnnotation(ix, tok.src_len())),
    })
}

pub fn expr_has_semi(expr: &AstExpr<'_>, has_semi_next: bool) -> bool {
    match expr {
        AstExpr::BinExpr(_) => has_semi_next,
        AstExpr::LitExpr(_) => has_semi_next,
        AstExpr::BlockExpr(AstBlock { has_semi, .. }) => *has_semi,
        AstExpr::ConditionalExpr(AstConditional {
            if_block,
            else_block,
            ..
        }) => {
            if let Some(eb) = else_block {
                expr_has_semi(&eb, has_semi_next)
            } else {
                if_block.has_semi
            }
        }
    }
}

fn parse_primary_expr<'src, I>(
    tokens: &mut Peekable<I>,
    context: ParseContext,
) -> Result<AstExpr<'src>>
where
    I: Iterator<Item = TokenIter<'src>>,
{
    let (ix, tok) = get_next_token(tokens)?;

    let expr = match tok {
        Token::LParen => {
            if matches!(tokens.peek(), Some(Ok((_, Token::Newline)))) {
                return parse_expr(tokens, Precedence::Lowest, context);
            }
            return parse_expr(tokens, Precedence::Lowest, context);
        }
        Token::If => return Ok(parse_conditional(tokens)?.into()),
        id @ Token::Ident(_) => {
            if matches!(tokens.peek(), Some(Ok((_, Token::Colon))))
                && context.can_parse_annotation
            {
                AstLiteral::TypedIdent {
                    name: id,
                    type_annotation: parse_annotation(tokens)?,
                }
            } else {
                AstLiteral::Ident(id)
            }
        }
        il @ Token::IntLiteral(_) => AstLiteral::Int(il),
        sl @ Token::StrLiteral(_) => AstLiteral::Str(sl),
        // fl @ Token::FloatLiteral(_) => AstLiteral::Str(sl).into(),
        ref x => {
            println!("Encountered Invalid Expression Start: {}", x);
            return Err(ParseErr::InvalidExpressionStart(ix, tok.src_len()));
        }
    };

    Ok(expr.into())
}

fn parse_conditional<'src, I>(
    tokens: &mut Peekable<I>,
) -> Result<AstConditional<'src>>
where
    I: Iterator<Item = TokenIter<'src>>,
{
    let condition = Box::new(parse_expr(
        tokens,
        Precedence::Lowest,
        ParseContext::new().without_annotation_parsing(),
    )?);

    let (ix, tok) = get_next_token(tokens)?;

    if !matches!(tok, Token::Colon) {
        return Err(ParseErr::ExpectedColon(ix, tok.src_len()));
    }

    if let Some(Ok((_, Token::Newline))) = tokens.peek() {
        tokens.next();
    }
    let if_block = parse_block(tokens)?;

    let else_block = if matches!(tokens.peek(), Some(Ok((_, Token::Else)))) {
        // Consume 'else'
        tokens.next();

        let (ix, tok) = get_next_token(tokens)?;
        let expr = match tok {
            Token::If => AstExpr::ConditionalExpr(parse_conditional(tokens)?),
            Token::Colon => {
                if !matches!(tokens.next(), Some(Ok((_, Token::Newline)))) {
                    return Err(ParseErr::ExpectedNewline(ix, tok.src_len()));
                }
                AstExpr::BlockExpr(parse_block(tokens)?)
            }
            _ => return Err(ParseErr::ExpectedColon(ix, tok.src_len())),
        };

        Some(Box::new(expr))
    } else {
        None
    };

    Ok(AstConditional {
        condition,
        if_block,
        else_block,
    })
}

fn parse_expr<'src, I>(
    tokens: &mut Peekable<I>,
    precedence: Precedence,
    context: ParseContext,
) -> Result<AstExpr<'src>>
where
    I: Iterator<Item = TokenIter<'src>>,
{
    let lhs = parse_primary_expr(tokens, context)?;
    Ok(parse_expr_with(lhs, tokens, precedence, context)?)
}

fn parse_expr_with<'src, I>(
    parsed_expr: AstExpr<'src>,
    tokens: &mut Peekable<I>,
    precedence: Precedence,
    context: ParseContext,
) -> Result<AstExpr<'src>>
where
    I: Iterator<Item = TokenIter<'src>>,
{
    let mut lhs = parsed_expr;

    loop {
        if let Some(Ok((_, tok))) = tokens.peek() {
            if matches!(tok, Token::Semicolon) {
                break;
            }
            if matches!(tok, Token::RParen) {
                tokens.next();
                break;
            }

            let op = match tok.as_operator() {
                None => break,
                Some(op) => op,
            };

            let encountered_precedence = op.precedence();
            if encountered_precedence < precedence {
                break;
            }

            tokens.next();

            let rhs = parse_expr(tokens, encountered_precedence, context)?;
            lhs = (lhs, op, rhs).into();
        } else {
            break;
        }
    }

    Ok(lhs)
}

fn parse_annotation<'src, I>(
    tokens: &mut Peekable<I>,
) -> Result<TypeAnnotation<'src>>
where
    I: Iterator<Item = TokenIter<'src>>,
{
    // Consume the ':'
    eat(tokens, Token::Colon)?;

    Ok(parse_type_decl(tokens)?)
}

fn eat<'src, I>(tokens: &mut Peekable<I>, expected_type: Token) -> Result<()>
where
    I: Iterator<Item = TokenIter<'src>>,
{
    let (ix, tok) = get_next_token(tokens)?;
    if tok == expected_type {
        return Ok(());
    }
    Err(ParseErr::ExpectedToken(
        ix,
        tok.src_len(),
        expected_type.to_string(),
    ))
}
