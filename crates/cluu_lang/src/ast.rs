//! AST definitions for the cluu shell language.

use alloc::string::String;
use alloc::vec::Vec;

/// Root program node containing a list of statements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    pub stmts: Vec<Stmt>,
}

/// A top-level statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stmt {
    Pipeline(Pipeline),
}

/// A pipeline is a list of commands separated by pipes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pipeline {
    pub commands: Vec<Command>,
    /// True when the pipeline is run in the background (trailing `&`).
    pub bg: bool,
}

/// A command with optional prefix assignments, elements, and redirections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub assigns: Vec<Assign>,
    pub elems: Vec<CmdElem>,
    pub redirs: Vec<Redir>,
}

/// A prefix assignment (only allowed at command start).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assign {
    pub name: String,
    pub value: Word,
}

/// Elements inside a command body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CmdElem {
    Word(Word),
    Subshell(Program),
}

/// A word is composed of multiple parts (e.g., bare chunks, variables).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word {
    pub parts: Vec<WordPart>,
}

/// Individual pieces inside a word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WordPart {
    Bare(String),
    SingleQuoted(String),
    DoubleQuoted(Vec<DqPart>),
    Var(String),
    CmdSub(Program),
}

/// Double-quoted parts (text, variable, command substitution, escapes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DqPart {
    Text(String),
    Var(String),
    CmdSub(Program),
    Escaped(String),
}

/// Redirection operation with target word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redir {
    pub op: RedirOp,
    pub target: Word,
}

/// Supported redirection operators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedirOp {
    In,
    OutTrunc,
    OutAppend,
    ErrTrunc,
}
