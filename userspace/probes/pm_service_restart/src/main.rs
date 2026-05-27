//! Probe: pm_service_restart
//!
//! Goal (per task spec): verify that a service spawned with
//! `RestartPolicy::Always` or `RestartPolicy::OnFailure` is restarted by
//! procmgr after it crashes, and that the new instance reports a *different*
//! PID than the original instance.
//!
//! Option A (runtime restart observation) is **not feasible** from a userspace
//! probe at HEAD:
//!   - `RestartPolicy` is declared in the image's Cluufile / `manifest.toml`
//!     (spec 1 §11). It is *not* a `SpawnEnvelope` field, so a probe cannot
//!     ask procmgr to spawn an arbitrary image under `Always` / `OnFailure`.
//!   - The only images at HEAD that declare `RESTART always` are system
//!     primordials (getty / kbd / vtmgr / console / compositor). A probe must
//!     not kill these — doing so would destabilise the system under test and
//!     the probe still has no PID-by-name lookup to observe the re-spawn.
//!   - libcluu exposes no `process::query(pid)` or `procmgr::list()` API the
//!     probe could poll for "second instance, different PID".
//!
//! We therefore execute **Option B (structural check)** documented in the task
//! brief: prove the wire-protocol pieces that the supervisor will rely on are
//! present and correct, and document the runtime gap in the PASS line so
//! coverage is recorded honestly.
//!
//! Structural cases:
//!   case_a — `RestartPolicy::Never`        postcard roundtrip
//!   case_b — `RestartPolicy::Always`       postcard roundtrip
//!   case_c — `RestartPolicy::OnFailure { max, window_ms }` postcard roundtrip
//!   case_d — discriminants are stable & distinct (defensive — guards against
//!            an accidental reordering that would silently change the wire).
//!
//! On success the probe emits:
//!   `pm_service_restart: PASS structural check — runtime restart needs supervisor manifest TODO`

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use cluu_wire::spawn::RestartPolicy;
use libcluu::debug_print;

const LABEL: &str = "pm_service_restart";

#[no_mangle]
pub extern "C" fn main() -> i32 {
    match run() {
        Ok(()) => 0,
        Err(()) => 1,
    }
}

fn run() -> Result<(), ()> {
    let _ = debug_print(&format!("{}: start\n", LABEL));

    // --- case_a: Never roundtrip ---
    if let Err(reason) = roundtrip(RestartPolicy::Never) {
        let _ = debug_print(&format!("{}: FAIL case_a Never {}\n", LABEL, reason));
        return Err(());
    }
    let _ = debug_print(&format!("{}: case_a Never roundtrip ok\n", LABEL));

    // --- case_b: Always roundtrip ---
    if let Err(reason) = roundtrip(RestartPolicy::Always) {
        let _ = debug_print(&format!("{}: FAIL case_b Always {}\n", LABEL, reason));
        return Err(());
    }
    let _ = debug_print(&format!("{}: case_b Always roundtrip ok\n", LABEL));

    // --- case_c: OnFailure { max, window_ms } roundtrip ---
    let onfail = RestartPolicy::OnFailure { max: 5, window_ms: 30_000 };
    if let Err(reason) = roundtrip(onfail) {
        let _ = debug_print(&format!("{}: FAIL case_c OnFailure {}\n", LABEL, reason));
        return Err(());
    }
    // Verify the payload values survived the trip, not just the variant.
    let bytes = match postcard::to_allocvec(&onfail) {
        Ok(b) => b,
        Err(_) => {
            let _ = debug_print(&format!("{}: FAIL case_c serialise\n", LABEL));
            return Err(());
        }
    };
    let decoded: RestartPolicy = match postcard::from_bytes(&bytes) {
        Ok(d) => d,
        Err(_) => {
            let _ = debug_print(&format!("{}: FAIL case_c deserialise\n", LABEL));
            return Err(());
        }
    };
    match decoded {
        RestartPolicy::OnFailure { max, window_ms } => {
            if max != 5 || window_ms != 30_000 {
                let _ = debug_print(&format!(
                    "{}: FAIL case_c payload mismatch max={} window_ms={}\n",
                    LABEL, max, window_ms
                ));
                return Err(());
            }
        }
        other => {
            let _ = debug_print(&format!(
                "{}: FAIL case_c variant drift decoded={:?}\n",
                LABEL, other
            ));
            return Err(());
        }
    }
    let _ = debug_print(&format!(
        "{}: case_c OnFailure roundtrip ok max=5 window_ms=30000\n",
        LABEL
    ));

    // --- case_d: discriminants are stable & distinct ---
    // Postcard encodes enum tags as a varint of the *declaration order*.
    // If somebody reorders RestartPolicy variants the wire silently breaks,
    // so pin the first byte of each encoding here.
    let tag_never = first_tag_byte(&RestartPolicy::Never);
    let tag_always = first_tag_byte(&RestartPolicy::Always);
    let tag_onfail = first_tag_byte(&RestartPolicy::OnFailure { max: 1, window_ms: 1 });
    if tag_never == tag_always || tag_never == tag_onfail || tag_always == tag_onfail {
        let _ = debug_print(&format!(
            "{}: FAIL case_d tag collision never={} always={} onfail={}\n",
            LABEL, tag_never, tag_always, tag_onfail
        ));
        return Err(());
    }
    // Declaration order in cluu_wire::spawn is Never (0), Always (1), OnFailure (2).
    if tag_never != 0 || tag_always != 1 || tag_onfail != 2 {
        let _ = debug_print(&format!(
            "{}: FAIL case_d tag drift never={} always={} onfail={} (expected 0,1,2)\n",
            LABEL, tag_never, tag_always, tag_onfail
        ));
        return Err(());
    }
    let _ = debug_print(&format!(
        "{}: case_d tags stable never=0 always=1 onfail=2\n",
        LABEL
    ));

    let _ = debug_print(&format!(
        "{}: PASS structural check - runtime restart needs supervisor manifest TODO\n",
        LABEL
    ));
    Ok(())
}

/// Serialize then deserialize a `RestartPolicy`, asserting the variant
/// survives. Returns `Err(reason)` with a short tag for the PASS/FAIL line.
fn roundtrip(p: RestartPolicy) -> Result<(), &'static str> {
    let bytes = postcard::to_allocvec(&p).map_err(|_| "serialise")?;
    let decoded: RestartPolicy = postcard::from_bytes(&bytes).map_err(|_| "deserialise")?;
    match (p, decoded) {
        (RestartPolicy::Never, RestartPolicy::Never) => Ok(()),
        (RestartPolicy::Always, RestartPolicy::Always) => Ok(()),
        (
            RestartPolicy::OnFailure { max: a, window_ms: b },
            RestartPolicy::OnFailure { max: c, window_ms: d },
        ) if a == c && b == d => Ok(()),
        _ => Err("variant_mismatch"),
    }
}

/// Postcard encodes enum tags as a leading varint; for tags 0..=127 that is
/// a single byte, which is the case for all three RestartPolicy variants.
fn first_tag_byte(p: &RestartPolicy) -> u8 {
    let bytes = postcard::to_allocvec(p).expect("serialise tag probe");
    *bytes.first().unwrap_or(&0xFF)
}
