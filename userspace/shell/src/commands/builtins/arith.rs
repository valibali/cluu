//! `expr` and `let` builtins — arithmetic operations.

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use libcluu::ipc::{send_with_payload, TTY_WRITE_LABEL};
use libcluu::Result;

use super::registry::CommandContext;
use super::registry::{BuiltinCommand, BuiltinRegistry};

pub fn register(registry: &mut BuiltinRegistry) {
    registry.register(Box::new(ExprBuiltin));
    registry.register(Box::new(LetBuiltin));
}

// ---------------------------------------------------------------------------
// expr
// ---------------------------------------------------------------------------

pub(crate) struct ExprBuiltin;

impl BuiltinCommand for ExprBuiltin {
    fn name(&self) -> &'static str {
        "expr"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        let Some((lhs, op, rhs)) = parse_expr_tokens(args) else {
            send_with_payload(stdout, TTY_WRITE_LABEL, b"expr: invalid expression\n")?;
            return Ok(());
        };
        match op.as_str() {
            "+" => arithmetic_op(stdout, context, &[lhs, rhs], |a, b| a + b),
            "-" => arithmetic_op(stdout, context, &[lhs, rhs], |a, b| a - b),
            "*" => arithmetic_op(stdout, context, &[lhs, rhs], |a, b| a * b),
            "/" => div_op(stdout, context, &lhs, &rhs),
            _ => {
                send_with_payload(stdout, TTY_WRITE_LABEL, b"expr: unknown op\n")?;
                Ok(())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// let
// ---------------------------------------------------------------------------

pub(crate) struct LetBuiltin;

impl BuiltinCommand for LetBuiltin {
    fn name(&self) -> &'static str {
        "let"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        if args.is_empty() {
            send_with_payload(stdout, TTY_WRITE_LABEL, b"let: missing name\n")?;
            return Ok(());
        }

        let mut name = args[0].as_str();
        let mut expr_tokens: Vec<String> = Vec::new();

        if args.len() == 1 {
            if let Some((lhs, rhs)) = args[0].split_once('=') {
                if lhs.is_empty() || rhs.is_empty() {
                    send_with_payload(stdout, TTY_WRITE_LABEL, b"let: expected NAME=EXPR\n")?;
                    return Ok(());
                }
                name = lhs;
                expr_tokens.push(rhs.to_string());
            } else {
                send_with_payload(stdout, TTY_WRITE_LABEL, b"let: missing =\n")?;
                return Ok(());
            }
        } else {
            let Some(eq) = args.get(1) else {
                send_with_payload(stdout, TTY_WRITE_LABEL, b"let: missing =\n")?;
                return Ok(());
            };
            if eq.as_str() != "=" {
                send_with_payload(stdout, TTY_WRITE_LABEL, b"let: expected =\n")?;
                return Ok(());
            }
            expr_tokens.extend_from_slice(&args[2..]);
        }

        let expr_args = expr_tokens.as_slice();
        let Some((lhs, op, rhs)) = parse_expr_tokens(expr_args) else {
            send_with_payload(stdout, TTY_WRITE_LABEL, b"let: invalid expression\n")?;
            return Ok(());
        };
        let value = match op.as_str() {
            "+" => calc_value(context, &lhs, &rhs, |a, b| a + b),
            "-" => calc_value(context, &lhs, &rhs, |a, b| a - b),
            "*" => calc_value(context, &lhs, &rhs, |a, b| a * b),
            "/" => {
                let Some(a) = parse_value(context, &lhs) else {
                    send_with_payload(stdout, TTY_WRITE_LABEL, b"let: invalid lhs\n")?;
                    return Ok(());
                };
                let Some(b) = parse_value(context, &rhs) else {
                    send_with_payload(stdout, TTY_WRITE_LABEL, b"let: invalid rhs\n")?;
                    return Ok(());
                };
                if b == 0 {
                    send_with_payload(stdout, TTY_WRITE_LABEL, b"let: divide by zero\n")?;
                    return Ok(());
                }
                Some((a / b).to_string())
            }
            _ => None,
        };
        match value {
            Some(result) => {
                context.set(name, result);
            }
            None => {
                send_with_payload(stdout, TTY_WRITE_LABEL, b"let: invalid expression\n")?;
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Arithmetic helpers
// ---------------------------------------------------------------------------

fn arithmetic_op<F>(
    stdout: usize,
    context: &mut CommandContext,
    args: &[String],
    op: F,
) -> Result<()>
where
    F: FnOnce(i64, i64) -> i64,
{
    let Some(a) = parse_value(context, args.first().unwrap_or(&String::new())) else {
        send_with_payload(stdout, TTY_WRITE_LABEL, b"arith: invalid lhs\n")?;
        return Ok(());
    };
    let Some(b) = parse_value(context, args.get(1).unwrap_or(&String::new())) else {
        send_with_payload(stdout, TTY_WRITE_LABEL, b"arith: invalid rhs\n")?;
        return Ok(());
    };
    let result = op(a, b).to_string();
    send_with_payload(stdout, TTY_WRITE_LABEL, result.as_bytes())?;
    send_with_payload(stdout, TTY_WRITE_LABEL, b"\n")?;
    Ok(())
}

fn div_op(stdout: usize, context: &mut CommandContext, lhs: &str, rhs: &str) -> Result<()> {
    let Some(a) = parse_value(context, lhs) else {
        send_with_payload(stdout, TTY_WRITE_LABEL, b"div: invalid lhs\n")?;
        return Ok(());
    };
    let Some(b) = parse_value(context, rhs) else {
        send_with_payload(stdout, TTY_WRITE_LABEL, b"div: invalid rhs\n")?;
        return Ok(());
    };
    if b == 0 {
        send_with_payload(stdout, TTY_WRITE_LABEL, b"div: divide by zero\n")?;
        return Ok(());
    }
    let result = (a / b).to_string();
    send_with_payload(stdout, TTY_WRITE_LABEL, result.as_bytes())?;
    send_with_payload(stdout, TTY_WRITE_LABEL, b"\n")?;
    Ok(())
}

fn calc_value<F>(context: &CommandContext, lhs: &str, rhs: &str, op: F) -> Option<String>
where
    F: FnOnce(i64, i64) -> i64,
{
    let a = parse_value(context, lhs)?;
    let b = parse_value(context, rhs)?;
    Some(op(a, b).to_string())
}

fn parse_value(context: &CommandContext, token: &str) -> Option<i64> {
    if let Ok(value) = token.parse::<i64>() {
        return Some(value);
    }
    context.get(token).and_then(|val| val.parse::<i64>().ok())
}

fn parse_expr_tokens(args: &[String]) -> Option<(String, String, String)> {
    if args.len() >= 3 {
        return Some((args[0].clone(), args[1].clone(), args[2].clone()));
    }
    if let Some(token) = args.first() {
        if let Some((lhs, op, rhs)) = split_expr_token(token) {
            return Some((lhs, op, rhs));
        }
    }
    None
}

fn split_expr_token(token: &str) -> Option<(String, String, String)> {
    let mut idx = None;
    for (pos, ch) in token.char_indices() {
        if matches!(ch, '+' | '-' | '*' | '/') {
            idx = Some((pos, ch));
            break;
        }
    }
    let (pos, ch) = idx?;
    let lhs = token.get(0..pos)?.trim();
    let rhs = token.get(pos + ch.len_utf8()..)?.trim();
    if lhs.is_empty() || rhs.is_empty() {
        return None;
    }
    Some((lhs.to_string(), ch.to_string(), rhs.to_string()))
}
