//! Pest-based parser for the cluu shell language.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use pest::iterators::Pair;
use pest::Parser;
use pest_derive::Parser;

use crate::ast::{
    Assign, CmdElem, Command, DqPart, Pipeline, Program, Redir, RedirOp, Stmt, Word, WordPart,
};

/// Parser generated from the cluu.pest grammar.
#[derive(Parser)]
#[grammar = "cluu.pest"]
pub struct CluuParser;

/// Lightweight parse error for no_std consumers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    pub line: usize,
    pub col: usize,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at {}:{}", self.message, self.line, self.col)
    }
}

impl ParseError {
    fn from_pest(err: pest::error::Error<Rule>) -> Self {
        let (line, col) = match err.line_col {
            pest::error::LineColLocation::Pos((line, col)) => (line, col),
            pest::error::LineColLocation::Span((line, col), _) => (line, col),
        };
        Self {
            message: err.to_string(),
            line,
            col,
        }
    }
}

/// Parse an input string into a Program AST.
pub fn parse_program(input: &str) -> Result<Program, ParseError> {
    let mut pairs = CluuParser::parse(Rule::program, input).map_err(ParseError::from_pest)?;
    let pair = pairs.next().ok_or(ParseError {
        message: "missing program".to_string(),
        line: 0,
        col: 0,
    })?;
    Ok(build_program(pair))
}

fn build_program(pair: Pair<Rule>) -> Program {
    let mut stmts = Vec::new();
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::stmt_list => {
                stmts = build_stmt_list(inner);
            }
            Rule::pipeline => {
                stmts.push(Stmt::Pipeline(build_pipeline(inner)));
            }
            Rule::stmt => {
                stmts.push(build_stmt(inner));
            }
            _ => {}
        }
    }
    Program { stmts }
}

fn build_stmt_list(pair: Pair<Rule>) -> Vec<Stmt> {
    let mut stmts = Vec::new();
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::stmt => stmts.push(build_stmt(inner)),
            Rule::pipeline => stmts.push(Stmt::Pipeline(build_pipeline(inner))),
            _ => {}
        }
    }
    stmts
}

fn build_stmt(pair: Pair<Rule>) -> Stmt {
    let mut inner = pair.into_inner();
    let pipeline = inner.next().map(build_pipeline).unwrap_or(Pipeline {
        commands: Vec::new(),
        bg: false,
    });
    Stmt::Pipeline(pipeline)
}

fn build_pipeline(pair: Pair<Rule>) -> Pipeline {
    let mut commands = Vec::new();
    let mut bg = false;
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::command => commands.push(build_command(inner)),
            Rule::bg_amp => bg = true,
            _ => {}
        }
    }
    Pipeline { commands, bg }
}

fn build_command(pair: Pair<Rule>) -> Command {
    let mut assigns = Vec::new();
    let mut elems = Vec::new();
    let mut redirs = Vec::new();

    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::prefix_assign => assigns.push(build_assign(inner)),
            Rule::cmd_elem => elems.push(build_cmd_elem(inner)),
            Rule::redir => redirs.push(build_redir(inner)),
            Rule::cmd_item => {
                for item in inner.into_inner() {
                    match item.as_rule() {
                        Rule::redir => redirs.push(build_redir(item)),
                        Rule::cmd_elem => elems.push(build_cmd_elem(item)),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    Command {
        assigns,
        elems,
        redirs,
    }
}

fn build_assign(pair: Pair<Rule>) -> Assign {
    let mut inner = pair.into_inner();
    let name = inner
        .next()
        .map(|p| p.as_str().to_string())
        .unwrap_or_default();
    let value = inner
        .next()
        .map(build_word)
        .unwrap_or(Word { parts: Vec::new() });
    Assign { name, value }
}

fn build_cmd_elem(pair: Pair<Rule>) -> CmdElem {
    let mut inner = pair.into_inner();
    match inner.next() {
        Some(p) if p.as_rule() == Rule::word => CmdElem::Word(build_word(p)),
        Some(p) if p.as_rule() == Rule::subshell => CmdElem::Subshell(build_subshell(p)),
        _ => CmdElem::Word(Word { parts: Vec::new() }),
    }
}

fn build_subshell(pair: Pair<Rule>) -> Program {
    for inner in pair.into_inner() {
        if inner.as_rule() == Rule::subprogram {
            return build_program(inner);
        }
    }
    Program { stmts: Vec::new() }
}

fn build_redir(pair: Pair<Rule>) -> Redir {
    let mut inner = pair.into_inner();
    let op = inner
        .next()
        .map(build_redir_op)
        .unwrap_or(RedirOp::OutTrunc);
    let target = inner
        .next()
        .map(build_word)
        .unwrap_or(Word { parts: Vec::new() });
    Redir { op, target }
}

fn build_redir_op(pair: Pair<Rule>) -> RedirOp {
    match pair.as_str() {
        "<" => RedirOp::In,
        ">>" => RedirOp::OutAppend,
        "2>" => RedirOp::ErrTrunc,
        ">" => RedirOp::OutTrunc,
        _ => RedirOp::OutTrunc,
    }
}

fn build_word(pair: Pair<Rule>) -> Word {
    let mut parts = Vec::new();
    for inner in pair.into_inner() {
        if inner.as_rule() == Rule::word_part {
            for part in inner.into_inner() {
                parts.push(build_word_part(part));
            }
        }
    }
    Word { parts }
}

fn build_word_part(pair: Pair<Rule>) -> WordPart {
    match pair.as_rule() {
        Rule::bare => WordPart::Bare(parse_bare(pair)),
        Rule::single_quoted => WordPart::SingleQuoted(strip_quotes(pair.as_str())),
        Rule::double_quoted => WordPart::DoubleQuoted(parse_double_quoted(pair)),
        Rule::var => WordPart::Var(parse_ident_from(pair)),
        Rule::cmdsub => WordPart::CmdSub(parse_cmdsub(pair)),
        _ => WordPart::Bare(String::new()),
    }
}

fn parse_bare(pair: Pair<Rule>) -> String {
    let mut out = String::new();
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::bare_char => out.push_str(inner.as_str()),
            Rule::bare_escape => {
                let escaped = inner.as_str();
                if let Some(ch) = escaped.chars().nth(1) {
                    out.push(ch);
                }
            }
            _ => {}
        }
    }
    out
}

fn parse_double_quoted(pair: Pair<Rule>) -> Vec<DqPart> {
    let mut parts = Vec::new();
    for inner in pair.into_inner() {
        if inner.as_rule() == Rule::dq_part {
            for part in inner.into_inner() {
                match part.as_rule() {
                    Rule::dq_text => parts.push(DqPart::Text(part.as_str().to_string())),
                    Rule::dq_var => parts.push(DqPart::Var(parse_ident_from(part))),
                    Rule::dq_cmdsub => parts.push(DqPart::CmdSub(parse_cmdsub(part))),
                    Rule::dq_escaped => parts.push(DqPart::Escaped(decode_escape(part.as_str()))),
                    _ => {}
                }
            }
        }
    }
    parts
}

fn parse_ident_from(pair: Pair<Rule>) -> String {
    let mut inner = pair.into_inner();
    inner
        .find(|p| p.as_rule() == Rule::ident)
        .map(|p| p.as_str().to_string())
        .unwrap_or_default()
}

fn parse_cmdsub(pair: Pair<Rule>) -> Program {
    for inner in pair.into_inner() {
        if inner.as_rule() == Rule::subprogram {
            return build_program(inner);
        }
    }
    Program { stmts: Vec::new() }
}

fn decode_escape(raw: &str) -> String {
    let mut chars = raw.chars();
    let _ = chars.next();
    match chars.next() {
        Some('n') => "\n".to_string(),
        Some('t') => "\t".to_string(),
        Some('"') => "\"".to_string(),
        Some('\\') => "\\".to_string(),
        Some('$') => "$".to_string(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

fn strip_quotes(raw: &str) -> String {
    if raw.len() >= 2 {
        raw.get(1..raw.len() - 1).unwrap_or("").to_string()
    } else {
        String::new()
    }
}
