//! Host-side unit tests for libcluu::cli argument parser.
//! Run with: cargo test -p libcluu --features host-test --test cli_test

#![cfg(feature = "host-test")]

use libcluu::cli::{parse, render_help, CliError, Spec};

// ---------------------------------------------------------------------------
// Test 1: single short flag
// ---------------------------------------------------------------------------
#[test]
fn parse_single_short_flag() {
    let spec = Spec::new().flag('a', "all", "include hidden");
    let argv = ["prog", "-a"]
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    let parsed = parse(&spec, &argv).unwrap();
    assert!(parsed.is_set("all"));
    assert!(parsed.positional.is_empty());
}

// ---------------------------------------------------------------------------
// Test 2: clustered short flags (-rfv → 3 flags)
// ---------------------------------------------------------------------------
#[test]
fn parse_clustered_short_flags() {
    let spec = Spec::new()
        .flag('r', "recursive", "")
        .flag('f', "force", "")
        .flag('v', "verbose", "");
    let argv = ["rm", "-rfv", "file"]
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    let p = parse(&spec, &argv).unwrap();
    assert!(p.is_set("recursive"));
    assert!(p.is_set("force"));
    assert!(p.is_set("verbose"));
    assert_eq!(p.positional, vec!["file".to_string()]);
}

// ---------------------------------------------------------------------------
// Test 3: required arg — separate token (-n 5)
// ---------------------------------------------------------------------------
#[test]
fn parse_required_arg_separate() {
    let spec = Spec::new().required('n', "lines", "");
    let argv = ["head", "-n", "5", "file"]
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    let p = parse(&spec, &argv).unwrap();
    assert_eq!(p.value("lines"), Some("5"));
    assert_eq!(p.positional, vec!["file".to_string()]);
}

// ---------------------------------------------------------------------------
// Test 4: required arg — attached (-n5)
// ---------------------------------------------------------------------------
#[test]
fn parse_required_arg_attached() {
    let spec = Spec::new().required('n', "lines", "");
    let argv = ["head", "-n5", "file"]
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    let p = parse(&spec, &argv).unwrap();
    assert_eq!(p.value("lines"), Some("5"));
}

// ---------------------------------------------------------------------------
// Test 5: long required — equals (--lines=5)
// ---------------------------------------------------------------------------
#[test]
fn parse_long_required_eq() {
    let spec = Spec::new().required('n', "lines", "");
    let argv = ["head", "--lines=5", "file"]
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    let p = parse(&spec, &argv).unwrap();
    assert_eq!(p.value("lines"), Some("5"));
}

// ---------------------------------------------------------------------------
// Test 6: long required — space (--lines 5)
// ---------------------------------------------------------------------------
#[test]
fn parse_long_required_space() {
    let spec = Spec::new().required('n', "lines", "");
    let argv = ["head", "--lines", "5", "file"]
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    let p = parse(&spec, &argv).unwrap();
    assert_eq!(p.value("lines"), Some("5"));
}

// ---------------------------------------------------------------------------
// Test 7: -- terminates option parsing
// ---------------------------------------------------------------------------
#[test]
fn parse_double_dash_terminates_options() {
    let spec = Spec::new().flag('v', "verbose", "");
    let argv = ["x", "-v", "--", "-not-a-flag", "file"]
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    let p = parse(&spec, &argv).unwrap();
    assert!(p.is_set("verbose"));
    assert_eq!(
        p.positional,
        vec!["-not-a-flag".to_string(), "file".to_string()]
    );
}

// ---------------------------------------------------------------------------
// Test 8: unknown option → CliError::UnknownOption
// ---------------------------------------------------------------------------
#[test]
fn parse_unknown_option_errors() {
    let spec = Spec::new().flag('v', "verbose", "");
    let argv = ["x", "-z"].iter().map(|s| s.to_string()).collect::<Vec<_>>();
    match parse(&spec, &argv) {
        Err(CliError::UnknownOption(_)) => {}
        other => panic!("expected UnknownOption, got {:?}", other),
    }
}

// ---------------------------------------------------------------------------
// Test 9: --help → CliError::HelpRequested
// ---------------------------------------------------------------------------
#[test]
fn parse_help_requested() {
    let spec = Spec::new().flag('v', "verbose", "");
    let argv = ["x", "--help"]
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    match parse(&spec, &argv) {
        Err(CliError::HelpRequested) => {}
        other => panic!("expected HelpRequested, got {:?}", other),
    }
}

// ---------------------------------------------------------------------------
// Test 10: --version → CliError::VersionRequested
// ---------------------------------------------------------------------------
#[test]
fn parse_version_requested() {
    let spec = Spec::new().flag('v', "verbose", "").version("1.2.3");
    let argv = ["x", "--version"]
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    match parse(&spec, &argv) {
        Err(CliError::VersionRequested) => {}
        other => panic!("expected VersionRequested, got {:?}", other),
    }
}

// ---------------------------------------------------------------------------
// Test 11: render_help produces non-empty usage text
// ---------------------------------------------------------------------------
#[test]
fn render_help_contains_usage() {
    let spec = Spec::new()
        .program("myutil")
        .usage("[OPTIONS] FILE...")
        .flag('v', "verbose", "be verbose");
    let help = render_help(&spec);
    assert!(help.contains("myutil"), "help should contain program name");
    assert!(help.contains("verbose"), "help should list --verbose");
    assert!(help.contains("be verbose"), "help should show help text");
}

// ---------------------------------------------------------------------------
// Test 12: positional args without flags
// ---------------------------------------------------------------------------
#[test]
fn parse_only_positionals() {
    let spec = Spec::new().flag('v', "verbose", "");
    let argv = ["cp", "src", "dst"]
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    let p = parse(&spec, &argv).unwrap();
    assert!(!p.is_set("verbose"));
    assert_eq!(p.positional, vec!["src".to_string(), "dst".to_string()]);
}
