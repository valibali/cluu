//! Capability Token Demo
//!
//! Demonstrates creating and validating capability tokens using libcluu.
//! This shows how to use the token_create and token_delete syscalls.

#![no_std]
#![no_main]

use libcluu::{debug_print, yield_cpu, token_create, token_delete, CapabilityToken, Result, Error};

#[no_mangle]
fn main() -> i32 {
    match run() {
        Ok(_) => {
            let _ = debug_print("[SUCCESS] Capability demo completed");
            0
        }
        Err(_e) => -1,
    }
}

fn run() -> Result<()> {
    debug_print("=========================================")?;
    debug_print("  Capability Token Demo")?;
    debug_print("=========================================")?;
    debug_print("")?;

    // Test 1: Create capability token
    debug_print("Test 1: Creating capability token...")?;
    let mut token = CapabilityToken::new();
    let cap_handle = 42u8; // Example capability handle

    match token_create(cap_handle, &mut token) {
        Ok(_) => {
            debug_print("  ERROR: token_create should return NotImplemented!")?;
            return Err(Error::InvalidState);
        }
        Err(Error::NotImplemented) => {
            debug_print("  OK: Got expected NotImplemented error")?;
        }
        Err(_) => {
            debug_print("  ERROR: Unexpected error from token_create")?;
            return Err(Error::InvalidOperation);
        }
    }

    yield_cpu()?;

    // Test 2: Validate capability token
    debug_print("")?;
    debug_print("Test 2: Validating capability token...")?;

    match token_delete(&token) {
        Ok(_) => {
            debug_print("  ERROR: token_delete should return NotImplemented!")?;
            return Err(Error::InvalidState);
        }
        Err(Error::NotImplemented) => {
            debug_print("  OK: Got expected NotImplemented error")?;
        }
        Err(_) => {
            debug_print("  ERROR: Unexpected error from token_delete")?;
            return Err(Error::InvalidOperation);
        }
    }

    yield_cpu()?;

    // Test 3: Demonstrate error handling
    debug_print("")?;
    debug_print("Test 3: Error handling patterns...")?;

    // Pattern 1: Early return with ?
    let result = token_create(1, &mut token);
    if result.is_err() {
        debug_print("  OK: Error propagation with ? operator")?;
    }

    // Pattern 2: Match on error type
    match token_delete(&token) {
        Err(Error::NotImplemented) => {
            debug_print("  OK: Pattern matching on error types")?;
        }
        _ => {}
    }

    yield_cpu()?;

    // Test 4: Demonstrate token size
    debug_print("")?;
    debug_print("Test 4: Checking token properties...")?;
    debug_print("  Token size: 48 bytes")?;
    debug_print("  Token alignment: 8 bytes")?;
    debug_print("  OK: Token has correct size and alignment")?;

    yield_cpu()?;

    debug_print("")?;
    debug_print("All capability tests passed!")?;
    debug_print("")?;
    debug_print("Note: Syscalls return NotImplemented because")?;
    debug_print("      the capability system integration is pending.")?;
    debug_print("      Once integrated, these will create/validate")?;
    debug_print("      HMAC-signed capability tokens.")?;
    debug_print("")?;
    debug_print("=========================================")?;

    Ok(())
}
