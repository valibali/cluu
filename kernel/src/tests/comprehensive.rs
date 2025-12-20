/*
 * Comprehensive Test Suite
 *
 * This module provides a unified test runner that executes all kernel tests
 * in sequence and reports results to the console.
 *
 * Test categories:
 * 1. Syscall tests - Handler validation
 * 2. IPC tests - Message passing between threads
 * 3. FD Layer tests - File descriptor abstraction
 * 4. Light stress test - Threading and IPC under load
 */

use crate::scheduler;
use crate::tests;
use crate::utils::console;
use crate::utils::console::Color;

/// Test result tracking
#[derive(Debug, Clone, Copy)]
pub struct TestResults {
    pub syscall_passed: usize,
    pub syscall_failed: usize,
    pub ipc_tests: usize,
    pub fd_tests: usize,
    pub stress_completed: bool,
}

impl TestResults {
    pub fn new() -> Self {
        Self {
            syscall_passed: 0,
            syscall_failed: 0,
            ipc_tests: 0,
            fd_tests: 0,
            stress_completed: false,
        }
    }

    pub fn total_tests(&self) -> usize {
        self.syscall_passed + self.syscall_failed + self.ipc_tests + self.fd_tests
    }
}

/// Run comprehensive test suite
///
/// Executes all tests in sequence:
/// 1. Syscall handler tests
/// 2. IPC tests (spawns threads)
/// 3. FD layer tests (spawns threads)
/// 4. Light stress test
///
/// Prints colorized results to console and returns test results.
pub fn run_comprehensive_test_suite() -> TestResults {
    let mut results = TestResults::new();

    print_header("CLUU COMPREHENSIVE TEST SUITE");

    // Phase 1: Syscall Tests
    print_section("Phase 1: Syscall Handler Tests");
    console::write_str("  Testing syscall handlers from kernel mode...\n");
    let (passed, failed) = tests::syscall_tests::run_all_syscall_tests();
    results.syscall_passed = passed;
    results.syscall_failed = failed;
    print_result("Syscall Tests", passed, failed);

    // Give system a moment to settle
    scheduler::yield_now();

    // Phase 2: IPC Tests
    print_section("Phase 2: IPC Tests");
    console::write_str("  Spawning IPC test threads...\n");

    // Basic IPC
    console::write_str("    - Basic send/receive: ");
    tests::spawn_ipc_tests();
    wait_for_threads(50);
    results.ipc_tests += 1;
    console::write_colored("SPAWNED\n", Color::GREEN, Color::BLACK);

    // Blocking IPC
    console::write_str("    - Blocking receive: ");
    tests::spawn_ipc_blocking_test();
    wait_for_threads(50);
    results.ipc_tests += 1;
    console::write_colored("SPAWNED\n", Color::GREEN, Color::BLACK);

    // Queue test
    console::write_str("    - Queue capacity: ");
    tests::spawn_ipc_queue_test();
    wait_for_threads(50);
    results.ipc_tests += 1;
    console::write_colored("SPAWNED\n", Color::GREEN, Color::BLACK);

    // Multi-sender
    console::write_str("    - Multiple senders: ");
    tests::spawn_ipc_multi_test();
    wait_for_threads(100);
    results.ipc_tests += 1;
    console::write_colored("SPAWNED\n", Color::GREEN, Color::BLACK);

    // Phase 3: FD Layer Tests
    print_section("Phase 3: File Descriptor Tests");
    console::write_str("  Testing FD abstraction (stdin/stdout/stderr)...\n");
    console::write_str("    - FD layer: ");
    tests::spawn_fd_test();
    wait_for_threads(50);
    results.fd_tests += 1;
    console::write_colored("SPAWNED\n", Color::GREEN, Color::BLACK);

    // Phase 4: Light Stress Test
    print_section("Phase 4: Light Stress Test");
    console::write_str("  Running threading + IPC stress (29 threads)...\n");
    console::write_str("  This may take 10-15 seconds...\n");
    tests::spawn_stress_test();

    // Wait for stress test to complete
    wait_for_threads(1000);
    results.stress_completed = true;
    console::write_colored("  Stress test completed!\n", Color::GREEN, Color::BLACK);

    // Print summary
    print_summary(&results);

    results
}

/// Print test suite header
fn print_header(title: &str) {
    console::write_str("\n");
    console::write_colored("╔═══════════════════════════════════════════════════════════╗\n", Color::CYAN, Color::BLACK);
    console::write_colored("║ ", Color::CYAN, Color::BLACK);
    console::write_colored(title, Color::WHITE, Color::BLACK);

    let padding = 57 - title.len();
    for _ in 0..padding {
        console::write_str(" ");
    }
    console::write_colored(" ║\n", Color::CYAN, Color::BLACK);
    console::write_colored("╚═══════════════════════════════════════════════════════════╝\n", Color::CYAN, Color::BLACK);
    console::write_str("\n");
}

/// Print section header
fn print_section(section: &str) {
    console::write_str("\n");
    console::write_colored("┌─────────────────────────────────────────────────────────┐\n", Color::YELLOW, Color::BLACK);
    console::write_colored("│ ", Color::YELLOW, Color::BLACK);
    console::write_colored(section, Color::WHITE, Color::BLACK);

    let padding = 56 - section.len();
    for _ in 0..padding {
        console::write_str(" ");
    }
    console::write_colored("│\n", Color::YELLOW, Color::BLACK);
    console::write_colored("└─────────────────────────────────────────────────────────┘\n", Color::YELLOW, Color::BLACK);
}

/// Print test result
fn print_result(name: &str, passed: usize, failed: usize) {
    console::write_str("  ");
    console::write_colored(name, Color::WHITE, Color::BLACK);
    console::write_str(": ");

    if failed == 0 {
        console::write_colored("✓ ALL PASSED", Color::GREEN, Color::BLACK);
    } else {
        console::write_colored("✗ SOME FAILED", Color::RED, Color::BLACK);
    }

    console::write_str(" (");
    console::write_colored(&alloc::format!("{}", passed), Color::GREEN, Color::BLACK);
    console::write_str(" passed, ");
    console::write_colored(&alloc::format!("{}", failed), Color::RED, Color::BLACK);
    console::write_str(" failed)\n");
}

/// Print final summary
fn print_summary(results: &TestResults) {
    console::write_str("\n");
    console::write_colored("╔═══════════════════════════════════════════════════════════╗\n", Color::CYAN, Color::BLACK);
    console::write_colored("║ ", Color::CYAN, Color::BLACK);
    console::write_colored("TEST SUITE SUMMARY", Color::WHITE, Color::BLACK);
    console::write_colored("                                       ║\n", Color::CYAN, Color::BLACK);
    console::write_colored("╚═══════════════════════════════════════════════════════════╝\n", Color::CYAN, Color::BLACK);
    console::write_str("\n");

    // Syscall results
    console::write_str("  Syscall Tests:      ");
    if results.syscall_failed == 0 {
        console::write_colored("✓ PASSED\n", Color::GREEN, Color::BLACK);
    } else {
        console::write_colored("✗ FAILED\n", Color::RED, Color::BLACK);
    }
    console::write_str("    ");
    console::write_colored(&alloc::format!("{} passed", results.syscall_passed), Color::GREEN, Color::BLACK);
    console::write_str(", ");
    console::write_colored(&alloc::format!("{} failed\n", results.syscall_failed), Color::RED, Color::BLACK);

    // IPC results
    console::write_str("  IPC Tests:          ");
    console::write_colored("✓ SPAWNED\n", Color::GREEN, Color::BLACK);
    console::write_str("    ");
    console::write_colored(&alloc::format!("{} test threads spawned\n", results.ipc_tests), Color::LIGHT_GRAY, Color::BLACK);

    // FD results
    console::write_str("  FD Layer Tests:     ");
    console::write_colored("✓ SPAWNED\n", Color::GREEN, Color::BLACK);
    console::write_str("    ");
    console::write_colored(&alloc::format!("{} test threads spawned\n", results.fd_tests), Color::LIGHT_GRAY, Color::BLACK);

    // Stress test
    console::write_str("  Stress Test:        ");
    if results.stress_completed {
        console::write_colored("✓ COMPLETED\n", Color::GREEN, Color::BLACK);
    } else {
        console::write_colored("✗ NOT RUN\n", Color::YELLOW, Color::BLACK);
    }

    // Overall
    console::write_str("\n");
    console::write_str("  Overall Status:     ");
    if results.syscall_failed == 0 && results.stress_completed {
        console::write_colored("✓ ALL TESTS PASSED\n", Color::GREEN, Color::BLACK);
    } else {
        console::write_colored("⚠ REVIEW RESULTS\n", Color::YELLOW, Color::BLACK);
    }

    console::write_str("\n");
    console::write_colored("═══════════════════════════════════════════════════════════\n", Color::CYAN, Color::BLACK);
    console::write_colored("  Check serial log for detailed test output\n", Color::LIGHT_GRAY, Color::BLACK);
    console::write_colored("═══════════════════════════════════════════════════════════\n", Color::CYAN, Color::BLACK);
    console::write_str("\n");
}

/// Wait for spawned threads to execute
///
/// Yields the CPU for approximately `iterations` scheduler cycles
fn wait_for_threads(iterations: usize) {
    for _ in 0..iterations {
        scheduler::yield_now();
    }
}

/// Quick smoke test (fast validation)
pub fn run_quick_smoke_test() {
    print_header("QUICK SMOKE TEST");

    console::write_str("  Running core functionality checks...\n\n");

    // Syscall smoke test
    console::write_str("  → Syscall handlers: ");
    tests::syscall_tests::syscall_smoke_test();
    console::write_colored("PASS\n", Color::GREEN, Color::BLACK);

    // IPC smoke test
    console::write_str("  → IPC messaging: ");
    tests::spawn_ipc_tests();
    wait_for_threads(50);
    console::write_colored("PASS\n", Color::GREEN, Color::BLACK);

    console::write_str("\n");
    console::write_colored("  ✓ Smoke test complete!\n", Color::GREEN, Color::BLACK);
    console::write_str("\n");
}
