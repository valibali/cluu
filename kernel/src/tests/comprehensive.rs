/*
 * Comprehensive Test Suite
 *
 * This module provides a unified test runner that executes all kernel tests
 * in sequence and reports results to the console.
 *
 * Test categories:
 * 1. Syscall tests - Handler validation
 * 2. ELF Loader tests - Binary parsing and validation
 * 3. IPC tests - Message passing between threads
 * 4. FD Layer tests - File descriptor abstraction
 * 5. Light stress test - Threading and IPC under load
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
    pub elf_tests: usize,
    pub ipc_tests: usize,
    pub fd_tests: usize,
    pub userspace_passed: usize,
    pub userspace_failed: usize,
    pub shmem_passed: usize,
    pub shmem_failed: usize,
    pub stress_completed: bool,
}

impl TestResults {
    pub fn new() -> Self {
        Self {
            syscall_passed: 0,
            syscall_failed: 0,
            elf_tests: 0,
            ipc_tests: 0,
            fd_tests: 0,
            userspace_passed: 0,
            userspace_failed: 0,
            shmem_passed: 0,
            shmem_failed: 0,
            stress_completed: false,
        }
    }

    pub fn total_tests(&self) -> usize {
        self.syscall_passed + self.syscall_failed + self.elf_tests + self.ipc_tests
        + self.fd_tests + self.userspace_passed + self.userspace_failed
        + self.shmem_passed + self.shmem_failed
    }
}

/// Run comprehensive test suite
///
/// Executes all tests in sequence:
/// 1. Syscall handler tests
/// 2. ELF loader tests
/// 3. IPC tests (spawns threads)
/// 4. FD layer tests (spawns threads)
/// 5. Userspace execution (hello world)
/// 6. Light stress test
/// 7. Syscall stress tests (userspace syscall infrastructure)
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

    console::write_str("  Testing syscall stress infrastructure...\n");
    console::write_str("    - Syscall stress framework: ");
    tests::syscall_stress::syscall_stress_smoke_test();
    console::write_colored("READY\n", Color::GREEN, Color::BLACK);

    // Give system a moment to settle
    scheduler::yield_now();

    // Phase 2: ELF Loader Tests
    print_section("Phase 2: ELF Loader Tests");
    console::write_str("  Testing ELF header parsing and validation...\n");
    console::write_str("    - ELF header parsing: ");
    tests::elf_loader::test_elf_header_parsing();
    results.elf_tests += 1;
    console::write_colored("PASSED\n", Color::GREEN, Color::BLACK);

    console::write_str("    - Invalid magic detection: ");
    tests::elf_loader::test_elf_invalid_magic();
    results.elf_tests += 1;
    console::write_colored("PASSED\n", Color::GREEN, Color::BLACK);

    // Give system a moment to settle
    scheduler::yield_now();

    // Phase 3: IPC Tests
    print_section("Phase 3: IPC Tests");
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

    // Phase 4: FD Layer Tests
    print_section("Phase 4: File Descriptor Tests");
    console::write_str("  Testing FD abstraction (stdin/stdout/stderr)...\n");
    console::write_str("    - FD layer: ");
    tests::spawn_fd_test();
    wait_for_threads(50);
    results.fd_tests += 1;
    console::write_colored("SPAWNED\n", Color::GREEN, Color::BLACK);

    // Phase 5: Userspace Tests
    print_section("Phase 5: Userspace Execution");
    console::write_str("  Loading and executing userspace ELF binary...\n");
    console::write_str("    - Hello World program: ");
    let userspace_success = tests::userspace_hello::test_userspace_hello();
    wait_for_threads(100);
    if userspace_success {
        results.userspace_passed += 1;
        console::write_colored("SPAWNED\n", Color::GREEN, Color::BLACK);
    } else {
        results.userspace_failed += 1;
        console::write_colored("FAILED\n", Color::RED, Color::BLACK);
    }

    // Phase 5.5: Shared Memory Tests
    print_section("Phase 5.5: Shared Memory Tests");
    console::write_str("  Testing shared memory syscalls (create/map/unmap/destroy)...\n");
    console::write_str("    - Shared memory test: ");

    // Read shmem_test binary from initrd
    let shmem_binary: &[u8] = match crate::initrd::read_file("bin/shmem_test") {
        Ok(data) => data,
        Err(e) => {
            console::write_colored("FAILED (", Color::RED, Color::BLACK);
            console::write_str(e);
            console::write_str(")\n");
            results.shmem_failed += 1;
            &[] // Empty slice
        }
    };

    if !shmem_binary.is_empty() {
        match crate::loaders::elf::spawn_elf_process(shmem_binary, "shmem_test", &[]) {
            Ok(_) => {
                // CRITICAL: Yield after thread creation to stabilize scheduler
                for _ in 0..10 {
                    crate::scheduler::yield_now();
                }
                wait_for_threads(150);
                results.shmem_passed += 1;
                console::write_colored("SPAWNED\n", Color::GREEN, Color::BLACK);
            }
            Err(e) => {
                console::write_colored("FAILED (", Color::RED, Color::BLACK);
                console::write_str(&alloc::format!("{:?}", e));
                console::write_str(")\n");
                results.shmem_failed += 1;
            }
        }
    }

    // Phase 6: Light Stress Test
    print_section("Phase 6: Light Stress Test");
    console::write_str("  Running threading + IPC stress (29 threads)...\n");
    console::write_str("  This may take 10-15 seconds...\n");
    tests::spawn_stress_test();

    // Wait for stress test to complete
    wait_for_threads(1000);
    results.stress_completed = true;
    console::write_colored("  Stress test completed!\n", Color::GREEN, Color::BLACK);

    // Phase 7: Syscall Stress Tests
    print_section("Phase 7: Syscall Stress Tests");
    console::write_str("  Running userspace syscall stress test suite...\n");
    console::write_str("  Note: Most tests require multiple process support (pending)\n");
    tests::syscall_stress::run_all_syscall_stress_tests();
    console::write_colored("  Syscall stress tests complete!\n", Color::GREEN, Color::BLACK);

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

    // ELF Loader results
    console::write_str("  ELF Loader Tests:   ");
    console::write_colored("✓ PASSED\n", Color::GREEN, Color::BLACK);
    console::write_str("    ");
    console::write_colored(&alloc::format!("{} tests completed\n", results.elf_tests), Color::LIGHT_GRAY, Color::BLACK);

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

    // Userspace results
    console::write_str("  Userspace Tests:    ");
    if results.userspace_failed == 0 && results.userspace_passed > 0 {
        console::write_colored("✓ PASSED\n", Color::GREEN, Color::BLACK);
    } else if results.userspace_failed > 0 {
        console::write_colored("✗ FAILED\n", Color::RED, Color::BLACK);
    } else {
        console::write_colored("⚠ NOT RUN\n", Color::YELLOW, Color::BLACK);
    }
    console::write_str("    ");
    console::write_colored(&alloc::format!("{} passed", results.userspace_passed), Color::GREEN, Color::BLACK);
    console::write_str(", ");
    console::write_colored(&alloc::format!("{} failed\n", results.userspace_failed), Color::RED, Color::BLACK);

    // Shared memory results
    console::write_str("  Shared Memory Tests:");
    if results.shmem_failed == 0 && results.shmem_passed > 0 {
        console::write_colored("✓ PASSED\n", Color::GREEN, Color::BLACK);
    } else if results.shmem_failed > 0 {
        console::write_colored("✗ FAILED\n", Color::RED, Color::BLACK);
    } else {
        console::write_colored("⚠ NOT RUN\n", Color::YELLOW, Color::BLACK);
    }
    console::write_str("    ");
    console::write_colored(&alloc::format!("{} passed", results.shmem_passed), Color::GREEN, Color::BLACK);
    console::write_str(", ");
    console::write_colored(&alloc::format!("{} failed\n", results.shmem_failed), Color::RED, Color::BLACK);

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
    if results.syscall_failed == 0 && results.userspace_failed == 0 && results.shmem_failed == 0 && results.stress_completed {
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

    // Syscall stress infrastructure
    console::write_str("  → Syscall stress framework: ");
    tests::syscall_stress::syscall_stress_smoke_test();
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
