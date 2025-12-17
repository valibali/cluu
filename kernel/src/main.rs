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
    scheduler::spawn_thread(|| test_ipc_multi_sender(1), "ipc-send-1");
    scheduler::spawn_thread(|| test_ipc_multi_sender(2), "ipc-send-2");
    scheduler::spawn_thread(|| test_ipc_multi_sender(3), "ipc-send-3");
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
