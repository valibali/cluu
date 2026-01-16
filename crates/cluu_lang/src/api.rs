//! Public API helpers for cluu_lang.

use alloc::format;
use alloc::string::String;
use core::fmt::Write;

use crate::ast::{CmdElem, Command, DqPart, Program, Redir, RedirOp, Stmt, Word, WordPart};
pub use crate::parser::parse_program;

/// Format an AST with a simple indentation scheme for debugging.
pub fn format_ast(program: &Program) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Program");
    for stmt in &program.stmts {
        format_stmt(&mut out, stmt, 1);
    }
    out
}

fn format_stmt(out: &mut String, stmt: &Stmt, indent: usize) {
    match stmt {
        Stmt::Pipeline(pipeline) => {
            indent_line(out, indent, "Pipeline");
            for command in &pipeline.commands {
                format_command(out, command, indent + 1);
            }
        }
    }
}

fn format_command(out: &mut String, command: &Command, indent: usize) {
    indent_line(out, indent, "Command");
    for assign in &command.assigns {
        indent_line(out, indent + 1, &format!("Assign {}", assign.name));
        format_word(out, &assign.value, indent + 2);
    }
    for elem in &command.elems {
        match elem {
            CmdElem::Word(word) => {
                indent_line(out, indent + 1, "WordElem");
                format_word(out, word, indent + 2);
            }
            CmdElem::Subshell(program) => {
                indent_line(out, indent + 1, "Subshell");
                format_program(out, program, indent + 2);
            }
        }
    }
    for redir in &command.redirs {
        format_redir(out, redir, indent + 1);
    }
}

fn format_program(out: &mut String, program: &Program, indent: usize) {
    indent_line(out, indent, "Program");
    for stmt in &program.stmts {
        format_stmt(out, stmt, indent + 1);
    }
}

fn format_word(out: &mut String, word: &Word, indent: usize) {
    indent_line(out, indent, "Word");
    for part in &word.parts {
        match part {
            WordPart::Bare(text) => indent_line(out, indent + 1, &format!("Bare {}", text)),
            WordPart::SingleQuoted(text) => {
                indent_line(out, indent + 1, &format!("SingleQuoted {}", text));
            }
            WordPart::DoubleQuoted(parts) => {
                indent_line(out, indent + 1, "DoubleQuoted");
                for dq in parts {
                    format_dq_part(out, dq, indent + 2);
                }
            }
            WordPart::Var(name) => indent_line(out, indent + 1, &format!("Var {}", name)),
            WordPart::CmdSub(program) => {
                indent_line(out, indent + 1, "CmdSub");
                format_program(out, program, indent + 2);
            }
        }
    }
}

fn format_dq_part(out: &mut String, part: &DqPart, indent: usize) {
    match part {
        DqPart::Text(text) => indent_line(out, indent, &format!("Text {}", text)),
        DqPart::Var(name) => indent_line(out, indent, &format!("Var {}", name)),
        DqPart::CmdSub(program) => {
            indent_line(out, indent, "CmdSub");
            format_program(out, program, indent + 1);
        }
        DqPart::Escaped(text) => indent_line(out, indent, &format!("Escaped {}", text)),
    }
}

fn format_redir(out: &mut String, redir: &Redir, indent: usize) {
    let op = match redir.op {
        RedirOp::In => "<",
        RedirOp::OutTrunc => ">",
        RedirOp::OutAppend => ">>",
        RedirOp::ErrTrunc => "2>",
    };
    indent_line(out, indent, &format!("Redir {}", op));
    format_word(out, &redir.target, indent + 1);
}

fn indent_line(out: &mut String, indent: usize, text: &str) {
    let _ = writeln!(out, "{}{}", "  ".repeat(indent), text);
}
