use cluu_lang::{format_ast, parse_program};

fn main() {
    let samples = [
        "echo hello world",
        "FOO=bar echo \"$FOO\"",
        "echo one\\ two three",
        "echo \"$(echo nested; echo ok)\"",
        "(echo a; echo b) | cat",
        "echo hi > out.txt",
        "echo 'literal $HOME'",
        "echo $HOME",
        "echo hi >> log.txt",
        "cat < input.txt",
        "echo fail 2> err.txt",
    ];

    for sample in samples {
        println!("=== input ===\n{}", sample);
        match parse_program(sample) {
            Ok(ast) => {
                println!("=== ast ===\n{}", format_ast(&ast));
            }
            Err(err) => {
                println!("parse error: {}", err);
            }
        }
    }
}
