//! :set framework. See spec §10.

extern crate alloc;
use alloc::string::String;
use crate::mode::{Editor, Settings};

pub fn dispatch(state: &mut Editor, args: &str) {
    if args.is_empty() {
        state.message = format_all(&state.settings);
        return;
    }
    for tok in args.split_whitespace() {
        if let Err(msg) = apply_one(&mut state.settings, tok) {
            state.message = msg;
            return;
        }
    }
}

fn apply_one(s: &mut Settings, tok: &str) -> Result<(), String> {
    if let Some((key, val)) = tok.split_once('=') {
        let key = canon(key);
        match key.as_str() {
            "tabstop"  => s.tabstop  = val.parse().map_err(|_| alloc::format!("E521: Number required: tabstop={}", val))?,
            "scrolloff" => s.scrolloff = val.parse().map_err(|_| alloc::format!("E521: Number required: scrolloff={}", val))?,
            _          => return Err(alloc::format!("E518: Unknown option: {}", key)),
        }
        return Ok(());
    }
    let (key, on) = if let Some(rest) = tok.strip_prefix("no") {
        (canon(rest), false)
    } else {
        (canon(tok), true)
    };
    match key.as_str() {
        "expandtab"   => s.expandtab   = on,
        "smartindent" => s.smartindent = on,
        "ignorecase"  => s.ignorecase  = on,
        "hlsearch"    => s.hlsearch    = on,
        "wrap"        => s.wrap        = on,
        "number"      => s.number      = on,
        _             => return Err(alloc::format!("E518: Unknown option: {}", tok)),
    }
    Ok(())
}

fn canon(short: &str) -> String {
    let s = match short {
        "et"  => "expandtab",
        "ts"  => "tabstop",
        "ic"  => "ignorecase",
        "hls" => "hlsearch",
        "nu"  => "number",
        "si"  => "smartindent",
        "so"  => "scrolloff",
        s     => s,
    };
    s.into()
}

fn format_all(s: &Settings) -> String {
    alloc::format!(
        "expandtab={} tabstop={} smartindent={} ignorecase={} hlsearch={} wrap={} number={} scrolloff={}",
        s.expandtab, s.tabstop, s.smartindent, s.ignorecase, s.hlsearch, s.wrap, s.number, s.scrolloff
    )
}
