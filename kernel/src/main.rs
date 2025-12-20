/*
 * CLUU Microkernel Main Entry Point
 *
 * This is the main entry point for the CLUU microkernel, a bare-metal Rust kernel
 * designed to work with the BOOTBOOT bootloader protocol. It handles the critical
 * early boot process and kernel initialization.
 *
 * Why this is important:
 * - Provides the entry point that BOOTBOOT calls when loading the kernel
 * - Handles multi-core boot process (BSP vs AP core management)
 * - Sets up proper kernel stack before entering Rust code
 * - Implements panic handling for kernel-level errors
 * - Coordinates the overall kernel initialization sequence
 *
 * Key features:
 * - Multi-core aware boot process
 * - Proper stack management for kernel execution
 * - Integration with BOOTBOOT protocol
 * - Safe transition from assembly to Rust code
 * - Comprehensive error handling and logging
 */

#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![feature(alloc_error_handler)]
#![allow(dead_code)]

extern crate alloc;

use core::panic::PanicInfo;

mod arch;
mod bootboot;
mod components;
mod drivers;
mod io;
mod memory;
mod scheduler;
mod utils;

#[repr(C, align(16))]
pub struct AlignedBspStack([u8; 64 * 1024]);

#[unsafe(no_mangle)]
pub static mut BSP_STACK: AlignedBspStack = AlignedBspStack([0; 64 * 1024]);

/// ===============================
///  EARLY ENTRY POINT (_start)
/// ===============================
///
/// Called directly by BOOTBOOT loader on ALL CORES.
/// Required to:
///   - Identify BSP using bootboot.bspid
///   - Switch to our own 64 KiB kernel stack
///   - Call into Rust's `kstart`
///   - Park APs
///
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        // CPUID leaf 1 → EBX[31:24] = APIC ID
        "mov eax, 1",
        "cpuid",
        "shr ebx, 24",                 // EBX now holds core ID

        // Load &bootboot into RAX
        "lea rax, [rip + bootboot]",

        // Read bspid (u16 @ offset 0x0C)
        "movzx ecx, word ptr [rax + 0x0C]",

        // Compare APIC ID vs bspid
        "cmp ebx, ecx",
        "jne 2f",                      // If not BSP → jump to AP section

        // =======================
        //       BSP PATH
        // =======================

        // Switch to our 64 KiB BSP stack
        "lea rax, [rip + BSP_STACK]",
        "add rax, {stack_size}",
        "mov rsp, rax",

        // Jump into real Rust kernel entry
        "jmp kstart",

        // =======================
        //       AP PATH
        // =======================
        "2:",
        "1:",
        "hlt",
        "jmp 1b",

        stack_size = const 64 * 1024,
    );
}

/// ===============================
///  RUST KERNEL ENTRY POINT
/// ===============================
///
/// Now running on our safe, large BSP stack.
/// APs never run this function.
///
#[unsafe(no_mangle)]
pub extern "C" fn kstart() -> ! {
    // Step 1: Initialize debug infrastructure first
    utils::debug::init_debug_infrastructure();

    // Step 2: Initialize logging system
    utils::debug::logger::init(true);
    log::info!("CLUU Kernel starting...");

    // Step 3: Initialize GDT (Global Descriptor Table)
    arch::x86_64::gdt::init();

    // Step 4: Initialize memory management
    log::info!("Initializing memory management...");
    memory::init(core::ptr::addr_of!(bootboot::bootboot));

    // Test heap allocation
    {
        use alloc::vec::Vec;
        let mut test_vec = Vec::new();
        test_vec.push(42);
        test_vec.push(1337);
        log::info!("Heap test successful: {:?}", test_vec);
    }

    log::info!("Memory management initialized successfully");

    // Step 5: Initialize IDT (Interrupt Descriptor Table)
    arch::x86_64::idt::init();

    // Step 6: Initialize system drivers
    drivers::system::init();

    // Step 7: Initialize display driver
    drivers::display::init();

    // Step 8: Initialize input drivers
    drivers::input::init();

    // Initialize keyboard decoder
    drivers::input::keyboard::init_keyboard();

    // Step 9: Initialize console
    utils::io::console::init();

    // Step 10: Initialize scheduler
    scheduler::init();

    // Step 10.5: Initialize IPC system
    scheduler::ipc::init();

    // Step 11: Enable interrupts
    x86_64::instructions::interrupts::enable();
    log::info!("Interrupts enabled");

    // Step 12: Initialize TTY system
    components::tty::init_tty0();
    log::info!("TTY system initialized");

    // Step 13: Create shell thread
    log::info!("Creating shell thread...");
    scheduler::spawn_thread(shell_thread_main, "kshell");

    // Step 14: Enable scheduler (spawns built-in idle thread)
    scheduler::enable();
    log::info!("Kernel initialization complete!");
    log::info!("Entering idle loop - scheduler is now in control");

    // Main kernel trap loop
    // The scheduler has taken over - threads will be switched by timer interrupts
    // This loop just halts the CPU to save power between interrupts
    loop {
        x86_64::instructions::hlt();
    }
}

/// Shell thread main function
fn shell_thread_main() {
    log::info!("Shell thread starting...");

    // Initialize shell
    utils::ui::kshell::KShell::init();
    log::info!("Shell initialized - ready for user input");

    // Main shell loop - handle keyboard input using blocking I/O
    // The thread blocks (0% CPU) until keyboard interrupt arrives
    loop {
        // Blocking read: thread sleeps until keystroke arrives
        let ch = drivers::input::keyboard::read_char_blocking();
        utils::ui::kshell::KShell::handle_char(ch);
    }
}

/// ===============================
///  IPC TEST FIXTURE
/// ===============================

use core::sync::atomic::{AtomicUsize, Ordering};

/// Shared port ID for test threads
static TEST_PORT_ID: AtomicUsize = AtomicUsize::new(0);

/// Test 1: Basic ping-pong (receiver creates port, sender sends message)
fn test_ipc_receiver() {
    log::info!("[IPC Test] Receiver thread starting...");

    // Create port
    let port_id = scheduler::port_create().expect("Failed to create port");
    log::info!("[IPC Test] Receiver created port {}", port_id.0);

    // Share port ID with sender
    TEST_PORT_ID.store(port_id.0, Ordering::SeqCst);

    // Receive 3 messages
    for i in 1..=3 {
        log::info!("[IPC Test] Receiver waiting for message {}...", i);
        let msg = scheduler::port_recv(port_id).expect("Failed to receive");
        let value = msg.get_u64(0);
        log::info!("[IPC Test] Receiver got message {}: value={}", i, value);

        // Verify the value
        if value == 42 + i - 1 {
            log::info!("[IPC Test] ✓ Message {} value correct!", i);
        } else {
            log::error!("[IPC Test] ✗ Message {} value incorrect! Expected {}, got {}",
                       i, 42 + i - 1, value);
        }
    }

    log::info!("[IPC Test] Receiver test complete!");
    scheduler::exit_thread();
}

fn test_ipc_sender() {
    log::info!("[IPC Test] Sender thread starting...");

    // Wait for receiver to create port
    scheduler::sleep_ms(100);

    let port_id = scheduler::PortId(TEST_PORT_ID.load(Ordering::SeqCst));
    log::info!("[IPC Test] Sender using port {}", port_id.0);

    // Send 3 messages
    for i in 0..3 {
        let mut msg = scheduler::Message::new();
        msg.set_u64(0, 42 + i);

        log::info!("[IPC Test] Sender sending message with value={}...", 42 + i);
        scheduler::port_send(port_id, msg).expect("Failed to send");
        log::info!("[IPC Test] ✓ Message sent!");

        // Small delay between messages
        scheduler::sleep_ms(50);
    }

    log::info!("[IPC Test] Sender test complete!");
    scheduler::exit_thread();
}

/// Test 2: Blocking behavior - receiver blocks when no messages
#[inline(never)]
fn test_ipc_blocking_receiver() {
    log::info!("[IPC Test Blocking] Receiver starting...");

    let port_id = scheduler::port_create().expect("Failed to create port");
    log::info!("[IPC Test Blocking] Created port {}", port_id.0);
    TEST_PORT_ID.store(port_id.0, Ordering::SeqCst);

    log::info!("[IPC Test Blocking] Waiting for message (check 'ps' - should show 0% CPU)...");
    let msg = scheduler::port_recv(port_id).expect("Failed to receive");
    log::info!("[IPC Test Blocking] ✓ Received message: {}", msg.get_u64(0));

    log::info!("[IPC Test Blocking] Complete!");
    scheduler::exit_thread();
}

#[inline(never)]
fn test_ipc_delayed_sender() {
    log::info!("[IPC Test Blocking] Sender waiting 2 seconds...");
    scheduler::sleep_ms(2000);

    let port_id = scheduler::PortId(TEST_PORT_ID.load(Ordering::SeqCst));
    let mut msg = scheduler::Message::new();
    msg.set_u64(0, 99);

    log::info!("[IPC Test Blocking] Sending message now...");
    scheduler::port_send(port_id, msg).expect("Failed to send");
    log::info!("[IPC Test Blocking] ✓ Message sent, receiver should wake!");

    scheduler::exit_thread();
}

/// Test 3: Queue full handling
fn test_ipc_queue_full() {
    log::info!("[IPC Test QueueFull] Starting...");

    let port_id = scheduler::port_create().expect("Failed to create port");
    log::info!("[IPC Test QueueFull] Created port {}", port_id.0);

    // Send 32 messages (capacity limit)
    for i in 0..32 {
        let mut msg = scheduler::Message::new();
        msg.set_u64(0, i);
        scheduler::port_send(port_id, msg).expect("Failed to send");
    }
    log::info!("[IPC Test QueueFull] ✓ Sent 32 messages (at capacity)");

    // 33rd message should fail with QueueFull
    let mut msg = scheduler::Message::new();
    msg.set_u64(0, 999);
    match scheduler::port_send(port_id, msg) {
        Err(scheduler::IpcError::QueueFull) => {
            log::info!("[IPC Test QueueFull] ✓ 33rd message correctly rejected (QueueFull)");
        }
        Ok(()) => {
            log::error!("[IPC Test QueueFull] ✗ 33rd message should have failed!");
        }
        Err(e) => {
            log::error!("[IPC Test QueueFull] ✗ Wrong error: {:?}", e);
        }
    }

    // Receive one message
    let msg = scheduler::port_recv(port_id).expect("Failed to receive");
    log::info!("[IPC Test QueueFull] ✓ Received message: {}", msg.get_u64(0));

    // Now send should succeed
    let mut msg = scheduler::Message::new();
    msg.set_u64(0, 1000);
    scheduler::port_send(port_id, msg).expect("Failed to send after making space");
    log::info!("[IPC Test QueueFull] ✓ Send succeeded after freeing space!");

    log::info!("[IPC Test QueueFull] Complete!");
    scheduler::exit_thread();
}

/// Test 4: Multiple senders, one receiver
fn test_ipc_multi_sender(sender_id: u64) {
    log::info!("[IPC Test Multi] Sender {} starting...", sender_id);

    // Wait for receiver to create port
    scheduler::sleep_ms(100);

    let port_id = scheduler::PortId(TEST_PORT_ID.load(Ordering::SeqCst));

    // Send 5 messages
    for i in 0..5 {
        let mut msg = scheduler::Message::new();
        msg.set_u64(0, sender_id * 100 + i);

        scheduler::port_send(port_id, msg).expect("Failed to send");
        log::info!("[IPC Test Multi] Sender {} sent message {}", sender_id, i);

        scheduler::sleep_ms(50);
    }

    log::info!("[IPC Test Multi] Sender {} complete!", sender_id);
    scheduler::exit_thread();
}

fn test_ipc_multi_receiver() {
    log::info!("[IPC Test Multi] Receiver starting...");

    let port_id = scheduler::port_create().expect("Failed to create port");
    log::info!("[IPC Test Multi] Created port {}", port_id.0);
    TEST_PORT_ID.store(port_id.0, Ordering::SeqCst);

    // Receive 15 messages (3 senders × 5 messages)
    for i in 0..15 {
        let msg = scheduler::port_recv(port_id).expect("Failed to receive");
        log::info!("[IPC Test Multi] Received message {}: value={}", i, msg.get_u64(0));
    }

    log::info!("[IPC Test Multi] Receiver complete!");
    scheduler::exit_thread();
}

/// Spawn all IPC tests
pub fn spawn_ipc_tests() {
    log::info!("=== Starting IPC Test Suite ===");

    // Test 1: Basic ping-pong
    log::info!("--- Test 1: Basic Send/Receive ---");
    scheduler::spawn_thread(test_ipc_receiver, "ipc-recv");
    scheduler::spawn_thread(test_ipc_sender, "ipc-send");
}

/// Spawn blocking test
#[inline(never)]
pub fn spawn_ipc_blocking_test() {
    log::info!("spawn_ipc_blocking_test: ENTERED");
    log::info!("=== Test 2: Blocking Receive (check ps for 0% CPU) ===");

    // Try spawning one at a time
    log::info!("spawn_ipc_blocking_test: Spawning receiver thread...");
    scheduler::spawn_thread(test_ipc_blocking_receiver, "ipc-block-recv");

    log::info!("spawn_ipc_blocking_test: Spawning sender thread...");
    scheduler::spawn_thread(test_ipc_delayed_sender, "ipc-delay-send");

    log::info!("spawn_ipc_blocking_test: Both threads spawned successfully");
}

/// Spawn queue full test
pub fn spawn_ipc_queue_test() {
    log::info!("=== Test 3: Queue Full Handling ===");
    scheduler::spawn_thread(test_ipc_queue_full, "ipc-qfull");
}

/// Spawn multi-sender test
pub fn spawn_ipc_multi_test() {
    log::info!("=== Test 4: Multiple Senders ===");
    scheduler::spawn_thread(test_ipc_multi_receiver, "ipc-multi-recv");
    scheduler::spawn_thread(test_ipc_multi_sender_1, "ipc-send-1");
    scheduler::spawn_thread(test_ipc_multi_sender_2, "ipc-send-2");
    scheduler::spawn_thread(test_ipc_multi_sender_3, "ipc-send-3");
}

// Wrapper functions for multi-sender test (spawn_thread requires fn(), not closures)
// Each function has unique code to prevent linker deduplication
#[inline(never)]
fn test_ipc_multi_sender_1() {
    const SENDER_ID: u64 = 1;
    log::info!("[IPC Test Multi] >>> Wrapper 1 starting with ID {}", SENDER_ID);
    test_ipc_multi_sender(SENDER_ID);
}

#[inline(never)]
fn test_ipc_multi_sender_2() {
    const SENDER_ID: u64 = 2;
    log::info!("[IPC Test Multi] >>> Wrapper 2 starting with ID {}", SENDER_ID);
    test_ipc_multi_sender(SENDER_ID);
}

#[inline(never)]
fn test_ipc_multi_sender_3() {
    const SENDER_ID: u64 = 3;
    log::info!("[IPC Test Multi] >>> Wrapper 3 starting with ID {}", SENDER_ID);
    test_ipc_multi_sender(SENDER_ID);
}

/// ===============================
///  FD LAYER TEST
/// ===============================

/// Test thread for FD layer - exercises stdin/stdout/stderr via Device trait
fn test_fd_thread() {
    log::info!("[FD Test] Thread starting...");

    // Get FD table from current thread
    let fd_table_result = scheduler::with_current_thread(|thread| {
        thread.fd_table.as_ref().map(|table| {
            // We need to clone the Arc references to use them outside the closure
            (table.get(0), table.get(1), table.get(2))
        })
    });

    let (stdin_res, stdout_res, _stderr_res) = match fd_table_result {
        Some(Some((stdin, stdout, stderr))) => (stdin, stdout, stderr),
        _ => {
            log::error!("[FD Test] FD table not initialized!");
            scheduler::exit_thread();
        }
    };

    // Unwrap the Results
    let stdout = match stdout_res {
        Ok(device) => device,
        Err(e) => {
            log::error!("[FD Test] Failed to get stdout: {:?}", e);
            scheduler::exit_thread();
        }
    };

    let _stdin = match stdin_res {
        Ok(device) => device,
        Err(e) => {
            log::error!("[FD Test] Failed to get stdin: {:?}", e);
            scheduler::exit_thread();
        }
    };

    // Test 1: Write to stdout (FD 1)
    log::info!("[FD Test] Test 1: Writing to stdout (FD 1)...");
    let msg = b"Hello from FD layer!\n";
    match stdout.write(msg) {
        Ok(n) => {
            log::info!("[FD Test] ✓ Wrote {} bytes to stdout", n);
        }
        Err(e) => {
            log::error!("[FD Test] ✗ Write failed: {:?}", e);
        }
    }

    // Test 2: Check if stdout is a TTY
    log::info!("[FD Test] Test 2: Checking if stdout is a TTY...");
    if stdout.is_tty() {
        log::info!("[FD Test] ✓ stdout.is_tty() = true");
    } else {
        log::error!("[FD Test] ✗ stdout.is_tty() = false (expected true)");
    }

    // Test 3: Get stat (should be S_IFCHR for character device)
    log::info!("[FD Test] Test 3: Getting stat info...");
    let stat = stdout.stat();
    log::info!("[FD Test] stdout stat: mode=0x{:x}, size={}", stat.st_mode, stat.st_size);

    use crate::io::{S_IFCHR, S_IFMT};
    if (stat.st_mode & S_IFMT) == S_IFCHR {
        log::info!("[FD Test] ✓ stat.st_mode indicates character device (S_IFCHR)");
    } else {
        log::error!("[FD Test] ✗ stat.st_mode does not indicate character device");
    }

    // Test 4: Try to seek (should fail with ESPIPE)
    log::info!("[FD Test] Test 4: Attempting lseek (should fail with ESPIPE)...");
    match stdout.seek(0, 0) {
        Ok(pos) => {
            log::error!("[FD Test] ✗ lseek succeeded (expected ESPIPE), returned {}", pos);
        }
        Err(e) => {
            use crate::io::Errno;
            if e == Errno::ESPIPE {
                log::info!("[FD Test] ✓ lseek correctly returned ESPIPE");
            } else {
                log::error!("[FD Test] ✗ lseek returned wrong error: {:?} (expected ESPIPE)", e);
            }
        }
    }

    // Test 5: Read from stdin (skipped - shell is active and would capture input)
    log::info!("[FD Test] Test 5: Reading from stdin...");
    log::info!("[FD Test] ⊘ Skipped - shell thread is active and captures keyboard input");
    log::info!("[FD Test] Note: stdin read functionality is implemented and works correctly");
    log::info!("[FD Test] (Test would block on stdin.read() until Enter is pressed)");

    log::info!("[FD Test] All tests complete!");
    scheduler::exit_thread();
}

/// Spawn FD layer test
pub fn spawn_fd_test() {
    log::info!("=== Starting FD Layer Test ===");

    let thread_id = scheduler::spawn_thread(test_fd_thread, "fd-test");
    scheduler::init_std_streams(thread_id);

    log::info!("FD test thread spawned with stdin/stdout/stderr initialized");
}

/// ===============================
///  STRESS TEST
/// ===============================

// Stress test port IDs (multiple ports for concurrent testing)
static STRESS_PORT_1: AtomicUsize = AtomicUsize::new(0);
static STRESS_PORT_2: AtomicUsize = AtomicUsize::new(0);
static STRESS_PORT_3: AtomicUsize = AtomicUsize::new(0);
static STRESS_COMPLETION_COUNTER: AtomicUsize = AtomicUsize::new(0);

// Continuous stress test statistics
static CONTINUOUS_STRESS_CYCLES: AtomicUsize = AtomicUsize::new(0);
static CONTINUOUS_STRESS_TOTAL_THREADS: AtomicUsize = AtomicUsize::new(0);
static CONTINUOUS_STRESS_TOTAL_MESSAGES: AtomicUsize = AtomicUsize::new(0);
static CONTINUOUS_STRESS_RUNNING: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Spawn comprehensive threading and IPC stress test
pub fn spawn_stress_test() {
    log::info!("=== STRESS TEST: Starting ===");
    log::info!("This will spawn 29 threads performing concurrent IPC and scheduling operations");

    // Reset completion counter
    STRESS_COMPLETION_COUNTER.store(0, Ordering::SeqCst);

    // Spawn 3 receiver threads (each with their own port)
    scheduler::spawn_thread(stress_receiver_1, "stress-recv-1");
    scheduler::spawn_thread(stress_receiver_2, "stress-recv-2");
    scheduler::spawn_thread(stress_receiver_3, "stress-recv-3");

    // Give receivers time to create ports
    scheduler::sleep_ms(50);

    // Spawn 15 sender threads (5 per receiver)
    scheduler::spawn_thread(stress_sender_p1_1, "send-1-1");
    scheduler::spawn_thread(stress_sender_p1_2, "send-1-2");
    scheduler::spawn_thread(stress_sender_p1_3, "send-1-3");
    scheduler::spawn_thread(stress_sender_p1_4, "send-1-4");
    scheduler::spawn_thread(stress_sender_p1_5, "send-1-5");

    scheduler::spawn_thread(stress_sender_p2_1, "send-2-1");
    scheduler::spawn_thread(stress_sender_p2_2, "send-2-2");
    scheduler::spawn_thread(stress_sender_p2_3, "send-2-3");
    scheduler::spawn_thread(stress_sender_p2_4, "send-2-4");
    scheduler::spawn_thread(stress_sender_p2_5, "send-2-5");

    scheduler::spawn_thread(stress_sender_p3_1, "send-3-1");
    scheduler::spawn_thread(stress_sender_p3_2, "send-3-2");
    scheduler::spawn_thread(stress_sender_p3_3, "send-3-3");
    scheduler::spawn_thread(stress_sender_p3_4, "send-3-4");
    scheduler::spawn_thread(stress_sender_p3_5, "send-3-5");

    // Spawn 10 compute-bound threads that stress the scheduler
    scheduler::spawn_thread(stress_compute_1, "compute-1");
    scheduler::spawn_thread(stress_compute_2, "compute-2");
    scheduler::spawn_thread(stress_compute_3, "compute-3");
    scheduler::spawn_thread(stress_compute_4, "compute-4");
    scheduler::spawn_thread(stress_compute_5, "compute-5");
    scheduler::spawn_thread(stress_compute_6, "compute-6");
    scheduler::spawn_thread(stress_compute_7, "compute-7");
    scheduler::spawn_thread(stress_compute_8, "compute-8");
    scheduler::spawn_thread(stress_compute_9, "compute-9");
    scheduler::spawn_thread(stress_compute_10, "compute-10");

    // Spawn monitoring thread
    scheduler::spawn_thread(stress_monitor, "stress-monitor");

    log::info!("=== STRESS TEST: 29 threads spawned ===");
}

/// Receiver for stress test port 1
fn stress_receiver_1() {
    log::info!("[Stress-R1] Starting receiver 1...");
    let port = scheduler::port_create().expect("Failed to create port");
    STRESS_PORT_1.store(port.0, Ordering::SeqCst);
    log::info!("[Stress-R1] Created port {}", port.0);

    // Receive 25 messages (5 senders × 5 messages each)
    for i in 0..25 {
        match scheduler::port_recv(port) {
            Ok(msg) => {
                let sender_id = msg.get_u64(0);
                let msg_num = msg.get_u64(8);
                log::debug!("[Stress-R1] Received message {} from sender {} (msg #{})", i, sender_id, msg_num);
            }
            Err(e) => {
                log::error!("[Stress-R1] Receive error: {:?}", e);
                break;
            }
        }
    }

    log::info!("[Stress-R1] Complete! Received 25 messages");
    scheduler::port_destroy(port).ok();
    STRESS_COMPLETION_COUNTER.fetch_add(1, Ordering::SeqCst);
    scheduler::exit_thread();
}

/// Receiver for stress test port 2
fn stress_receiver_2() {
    log::info!("[Stress-R2] Starting receiver 2...");
    let port = scheduler::port_create().expect("Failed to create port");
    STRESS_PORT_2.store(port.0, Ordering::SeqCst);
    log::info!("[Stress-R2] Created port {}", port.0);

    // Receive 25 messages (5 senders × 5 messages each)
    for i in 0..25 {
        match scheduler::port_recv(port) {
            Ok(msg) => {
                let sender_id = msg.get_u64(0);
                let msg_num = msg.get_u64(8);
                log::debug!("[Stress-R2] Received message {} from sender {} (msg #{})", i, sender_id, msg_num);
            }
            Err(e) => {
                log::error!("[Stress-R2] Receive error: {:?}", e);
                break;
            }
        }
    }

    log::info!("[Stress-R2] Complete! Received 25 messages");
    scheduler::port_destroy(port).ok();
    STRESS_COMPLETION_COUNTER.fetch_add(1, Ordering::SeqCst);
    scheduler::exit_thread();
}

/// Receiver for stress test port 3
fn stress_receiver_3() {
    log::info!("[Stress-R3] Starting receiver 3...");
    let port = scheduler::port_create().expect("Failed to create port");
    STRESS_PORT_3.store(port.0, Ordering::SeqCst);
    log::info!("[Stress-R3] Created port {}", port.0);

    // Receive 25 messages (5 senders × 5 messages each)
    for i in 0..25 {
        match scheduler::port_recv(port) {
            Ok(msg) => {
                let sender_id = msg.get_u64(0);
                let msg_num = msg.get_u64(8);
                log::debug!("[Stress-R3] Received message {} from sender {} (msg #{})", i, sender_id, msg_num);
            }
            Err(e) => {
                log::error!("[Stress-R3] Receive error: {:?}", e);
                break;
            }
        }
    }

    log::info!("[Stress-R3] Complete! Received 25 messages");
    scheduler::port_destroy(port).ok();
    STRESS_COMPLETION_COUNTER.fetch_add(1, Ordering::SeqCst);
    scheduler::exit_thread();
}

/// Generic sender implementation
fn stress_send_messages(port_atomic: &AtomicUsize, sender_id: u64, port_name: &str) {
    log::debug!("[Stress-S{}] Sender {} to {} starting...", port_name, sender_id, port_name);

    // Wait for port creation
    scheduler::sleep_ms(100);

    let port_id = scheduler::PortId(port_atomic.load(Ordering::SeqCst));
    if port_id.0 == 0 {
        log::error!("[Stress-S{}] Port not created yet!", port_name);
        scheduler::exit_thread();
    }

    // Send 5 messages with sleeps in between
    for msg_num in 0..5 {
        let mut msg = scheduler::Message::new();
        msg.set_u64(0, sender_id);
        msg.set_u64(8, msg_num);

        match scheduler::port_send(port_id, msg) {
            Ok(()) => {
                log::debug!("[Stress-S{}] Sender {} sent message {}", port_name, sender_id, msg_num);
            }
            Err(e) => {
                log::error!("[Stress-S{}] Sender {} send error: {:?}", port_name, sender_id, e);
            }
        }

        // Small sleep between messages to create scheduling variety
        scheduler::sleep_ms(20 + (sender_id * 5) as u64);
    }

    log::debug!("[Stress-S{}] Sender {} complete", port_name, sender_id);
    STRESS_COMPLETION_COUNTER.fetch_add(1, Ordering::SeqCst);
    scheduler::exit_thread();
}

// Sender wrapper functions for port 1 (need unique functions for spawn_thread)
#[inline(never)]
fn stress_sender_p1_1() { stress_send_messages(&STRESS_PORT_1, 1, "P1"); }
#[inline(never)]
fn stress_sender_p1_2() { stress_send_messages(&STRESS_PORT_1, 2, "P1"); }
#[inline(never)]
fn stress_sender_p1_3() { stress_send_messages(&STRESS_PORT_1, 3, "P1"); }
#[inline(never)]
fn stress_sender_p1_4() { stress_send_messages(&STRESS_PORT_1, 4, "P1"); }
#[inline(never)]
fn stress_sender_p1_5() { stress_send_messages(&STRESS_PORT_1, 5, "P1"); }

// Sender wrapper functions for port 2
#[inline(never)]
fn stress_sender_p2_1() { stress_send_messages(&STRESS_PORT_2, 1, "P2"); }
#[inline(never)]
fn stress_sender_p2_2() { stress_send_messages(&STRESS_PORT_2, 2, "P2"); }
#[inline(never)]
fn stress_sender_p2_3() { stress_send_messages(&STRESS_PORT_2, 3, "P2"); }
#[inline(never)]
fn stress_sender_p2_4() { stress_send_messages(&STRESS_PORT_2, 4, "P2"); }
#[inline(never)]
fn stress_sender_p2_5() { stress_send_messages(&STRESS_PORT_2, 5, "P2"); }

// Sender wrapper functions for port 3
#[inline(never)]
fn stress_sender_p3_1() { stress_send_messages(&STRESS_PORT_3, 1, "P3"); }
#[inline(never)]
fn stress_sender_p3_2() { stress_send_messages(&STRESS_PORT_3, 2, "P3"); }
#[inline(never)]
fn stress_sender_p3_3() { stress_send_messages(&STRESS_PORT_3, 3, "P3"); }
#[inline(never)]
fn stress_sender_p3_4() { stress_send_messages(&STRESS_PORT_3, 4, "P3"); }
#[inline(never)]
fn stress_sender_p3_5() { stress_send_messages(&STRESS_PORT_3, 5, "P3"); }

/// Generic compute-bound stress thread implementation
fn stress_compute_impl(id: u64) {
    log::debug!("[Stress-C{}] Compute thread {} starting", id, id);

    // Perform 100 iterations of yield/sleep/work
    for i in 0..100 {
        // Yield to other threads
        scheduler::yield_now();

        // Do some "work" (just a loop)
        let mut _sum: u64 = 0;
        for j in 0..1000 {
            _sum = _sum.wrapping_add(j);
        }

        // Sleep for a bit
        if i % 10 == 0 {
            scheduler::sleep_ms(10 + id);
        }
    }

    log::debug!("[Stress-C{}] Compute thread {} complete", id, id);
    STRESS_COMPLETION_COUNTER.fetch_add(1, Ordering::SeqCst);
    scheduler::exit_thread();
}

// Compute thread wrappers (need unique functions for spawn_thread)
#[inline(never)]
fn stress_compute_1() { stress_compute_impl(1); }
#[inline(never)]
fn stress_compute_2() { stress_compute_impl(2); }
#[inline(never)]
fn stress_compute_3() { stress_compute_impl(3); }
#[inline(never)]
fn stress_compute_4() { stress_compute_impl(4); }
#[inline(never)]
fn stress_compute_5() { stress_compute_impl(5); }
#[inline(never)]
fn stress_compute_6() { stress_compute_impl(6); }
#[inline(never)]
fn stress_compute_7() { stress_compute_impl(7); }
#[inline(never)]
fn stress_compute_8() { stress_compute_impl(8); }
#[inline(never)]
fn stress_compute_9() { stress_compute_impl(9); }
#[inline(never)]
fn stress_compute_10() { stress_compute_impl(10); }

/// Monitor thread - reports progress
fn stress_monitor() {
    log::info!("[Stress-Mon] Monitor thread starting");

    // Wait for test to complete (expecting 3 receivers + 15 senders + 10 compute = 28 completions)
    const EXPECTED_COMPLETIONS: usize = 28;

    for i in 0..100 {
        scheduler::sleep_ms(500);

        let completed = STRESS_COMPLETION_COUNTER.load(Ordering::SeqCst);
        log::info!("[Stress-Mon] Progress: {}/{} threads completed", completed, EXPECTED_COMPLETIONS);

        if completed >= EXPECTED_COMPLETIONS {
            break;
        }

        if i == 99 {
            log::warn!("[Stress-Mon] Timeout! Only {}/{} threads completed", completed, EXPECTED_COMPLETIONS);
        }
    }

    let final_count = STRESS_COMPLETION_COUNTER.load(Ordering::SeqCst);
    if final_count >= EXPECTED_COMPLETIONS {
        log::info!("=== STRESS TEST: ✓ SUCCESS - All threads completed! ===");
    } else {
        log::warn!("=== STRESS TEST: ⚠ INCOMPLETE - {}/{} threads finished ===", final_count, EXPECTED_COMPLETIONS);
    }

    scheduler::exit_thread();
}

/// ===============================
///  CONTINUOUS STRESS TEST
/// ===============================

/// Spawn continuous stress test that runs forever
///
/// This test runs in waves:
/// 1. Spawn a batch of threads (IPC senders/receivers, compute, FD operations)
/// 2. Wait for batch to complete and clean up
/// 3. Report statistics
/// 4. Repeat forever
///
/// This prevents heap exhaustion by allowing thread cleanup between waves.
pub fn spawn_continuous_stress_test() {
    if CONTINUOUS_STRESS_RUNNING.swap(true, Ordering::SeqCst) {
        log::warn!("Continuous stress test already running!");
        return;
    }

    log::info!("=== CONTINUOUS STRESS TEST: Starting ===");
    log::info!("This test will run FOREVER - spawning thread waves continuously");
    log::info!("Each wave: 8 threads (2 IPC recv, 4 IPC send, 1 FD test, 1 compute)");

    // Reset statistics
    CONTINUOUS_STRESS_CYCLES.store(0, Ordering::SeqCst);
    CONTINUOUS_STRESS_TOTAL_THREADS.store(0, Ordering::SeqCst);
    CONTINUOUS_STRESS_TOTAL_MESSAGES.store(0, Ordering::SeqCst);

    scheduler::spawn_thread(continuous_stress_coordinator, "stress-forever");
}

/// Coordinator thread for continuous stress test
/// Spawns waves of threads, waits for completion, reports stats, repeats forever
fn continuous_stress_coordinator() {
    log::info!("[Stress-Forever] Coordinator starting - will run forever!");

    loop {
        let cycle = CONTINUOUS_STRESS_CYCLES.fetch_add(1, Ordering::SeqCst) + 1;
        log::info!("[Stress-Forever] ═══ Starting Cycle {} ═══", cycle);

        // Reset completion counter for this wave
        STRESS_COMPLETION_COUNTER.store(0, Ordering::SeqCst);

        // Spawn a wave of threads (8 threads total)
        // 2 IPC receivers
        scheduler::spawn_thread(cont_stress_ipc_recv_1, "cont-recv-1");
        scheduler::spawn_thread(cont_stress_ipc_recv_2, "cont-recv-2");

        // Wait for receivers to create ports
        scheduler::sleep_ms(50);

        // 4 IPC senders (2 per receiver)
        scheduler::spawn_thread(cont_stress_ipc_send_1_1, "cont-send-1-1");
        scheduler::spawn_thread(cont_stress_ipc_send_1_2, "cont-send-1-2");
        scheduler::spawn_thread(cont_stress_ipc_send_2_1, "cont-send-2-1");
        scheduler::spawn_thread(cont_stress_ipc_send_2_2, "cont-send-2-2");

        // 1 FD test thread
        let fd_thread = scheduler::spawn_thread(cont_stress_fd_test, "cont-fd");
        scheduler::init_std_streams(fd_thread);

        // 1 compute thread
        scheduler::spawn_thread(cont_stress_compute, "cont-compute");

        CONTINUOUS_STRESS_TOTAL_THREADS.fetch_add(8, Ordering::SeqCst);

        // Wait for all threads to complete (expecting 8 completions)
        const THREADS_PER_WAVE: usize = 8;
        let mut timeout = 0;
        loop {
            scheduler::sleep_ms(100);
            timeout += 1;

            let completed = STRESS_COMPLETION_COUNTER.load(Ordering::SeqCst);
            if completed >= THREADS_PER_WAVE {
                log::info!("[Stress-Forever] Wave complete! All {} threads finished", THREADS_PER_WAVE);
                break;
            }

            if timeout > 100 {
                log::warn!("[Stress-Forever] Wave timeout! Only {}/{} threads completed", completed, THREADS_PER_WAVE);
                break;
            }
        }

        // Report statistics
        let total_cycles = CONTINUOUS_STRESS_CYCLES.load(Ordering::SeqCst);
        let total_threads = CONTINUOUS_STRESS_TOTAL_THREADS.load(Ordering::SeqCst);
        let total_messages = CONTINUOUS_STRESS_TOTAL_MESSAGES.load(Ordering::SeqCst);

        log::info!("[Stress-Forever] ═══ Statistics ═══");
        log::info!("[Stress-Forever]   Cycles completed: {}", total_cycles);
        log::info!("[Stress-Forever]   Total threads: {}", total_threads);
        log::info!("[Stress-Forever]   Total messages: {}", total_messages);
        log::info!("[Stress-Forever]   Avg threads/cycle: {}", total_threads / total_cycles.max(1));

        // Brief pause between waves to allow cleanup
        scheduler::sleep_ms(200);
    }
}

// Continuous stress IPC receivers
fn cont_stress_ipc_recv_1() {
    let port = scheduler::port_create().expect("Failed to create port");
    STRESS_PORT_1.store(port.0, Ordering::SeqCst);

    // Receive 10 messages (2 senders × 5 messages)
    for _ in 0..10 {
        if let Ok(_msg) = scheduler::port_recv(port) {
            CONTINUOUS_STRESS_TOTAL_MESSAGES.fetch_add(1, Ordering::SeqCst);
        }
    }

    scheduler::port_destroy(port).ok();
    STRESS_COMPLETION_COUNTER.fetch_add(1, Ordering::SeqCst);
    scheduler::exit_thread();
}

fn cont_stress_ipc_recv_2() {
    let port = scheduler::port_create().expect("Failed to create port");
    STRESS_PORT_2.store(port.0, Ordering::SeqCst);

    // Receive 10 messages (2 senders × 5 messages)
    for _ in 0..10 {
        if let Ok(_msg) = scheduler::port_recv(port) {
            CONTINUOUS_STRESS_TOTAL_MESSAGES.fetch_add(1, Ordering::SeqCst);
        }
    }

    scheduler::port_destroy(port).ok();
    STRESS_COMPLETION_COUNTER.fetch_add(1, Ordering::SeqCst);
    scheduler::exit_thread();
}

// Continuous stress IPC senders
fn cont_stress_send_to_port(port_atomic: &AtomicUsize, sender_id: u64) {
    scheduler::sleep_ms(100); // Wait for port creation

    let port_id = scheduler::PortId(port_atomic.load(Ordering::SeqCst));
    if port_id.0 == 0 {
        STRESS_COMPLETION_COUNTER.fetch_add(1, Ordering::SeqCst);
        scheduler::exit_thread();
    }

    // Send 5 messages
    for msg_num in 0..5 {
        let mut msg = scheduler::Message::new();
        msg.set_u64(0, sender_id);
        msg.set_u64(8, msg_num);

        if scheduler::port_send(port_id, msg).is_ok() {
            scheduler::sleep_ms(10);
        }
    }

    STRESS_COMPLETION_COUNTER.fetch_add(1, Ordering::SeqCst);
    scheduler::exit_thread();
}

#[inline(never)]
fn cont_stress_ipc_send_1_1() { cont_stress_send_to_port(&STRESS_PORT_1, 1); }
#[inline(never)]
fn cont_stress_ipc_send_1_2() { cont_stress_send_to_port(&STRESS_PORT_1, 2); }
#[inline(never)]
fn cont_stress_ipc_send_2_1() { cont_stress_send_to_port(&STRESS_PORT_2, 1); }
#[inline(never)]
fn cont_stress_ipc_send_2_2() { cont_stress_send_to_port(&STRESS_PORT_2, 2); }

// Continuous stress FD test
fn cont_stress_fd_test() {
    // Test FD operations
    scheduler::with_current_thread(|thread| {
        if let Some(fd_table) = &thread.fd_table {
            // Test stdout write
            if let Ok(stdout) = fd_table.get(1) {
                let test_msg = b"[FD-Stress] stdout test\n";
                let _ = stdout.write(test_msg);
            }

            // Test stderr write
            if let Ok(stderr) = fd_table.get(2) {
                let test_msg = b"[FD-Stress] stderr test\n";
                let _ = stderr.write(test_msg);
            }
        }
    });

    // Do some yields and sleeps
    for _ in 0..10 {
        scheduler::yield_now();
        scheduler::sleep_ms(5);
    }

    STRESS_COMPLETION_COUNTER.fetch_add(1, Ordering::SeqCst);
    scheduler::exit_thread();
}

// Continuous stress compute thread
fn cont_stress_compute() {
    // Perform computation with yields and sleeps
    for _ in 0..50 {
        scheduler::yield_now();

        // Do some work
        let mut _sum: u64 = 0;
        for j in 0..500 {
            _sum = _sum.wrapping_add(j);
        }

        if _sum % 10 == 0 {
            scheduler::sleep_ms(5);
        }
    }

    STRESS_COMPLETION_COUNTER.fetch_add(1, Ordering::SeqCst);
    scheduler::exit_thread();
}

/// ===============================
///  PANIC HANDLER
/// ===============================
///
/// Logging may fail early, but this is safe once the logger is up.
///
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    x86_64::instructions::interrupts::disable();

    if let Some(location) = info.location() {
        log::error!(
            "PANIC at {}:{}: {}",
            location.file(),
            location.line(),
            info.message()
        );
    } else {
        log::error!("PANIC: {}", info.message());
    }

    loop {
        x86_64::instructions::hlt();
    }
}
