/*
 * System Timer and Uptime Management
 *
 * This module provides global uptime tracking in milliseconds and scheduler tick functionality.
 * It's designed to be called from the timer interrupt handler (IRQ0) and provides the foundation
 * for future scheduler implementation.
 *
 * ## Description
 *
 * The timer system is a critical component of the kernel that provides:
 *
 * ### Core Functionality:
 * - **Global Uptime Tracking**: Maintains a precise millisecond counter since system boot
 * - **Scheduler Tick Generation**: Provides regular scheduler quantum interrupts for future task switching
 * - **Timer Interrupt Management**: Handles IRQ0 timer interrupts from the Programmable Interval Timer (PIT)
 * - **Sleep Functionality**: Basic busy-wait sleep implementation for timing delays
 *
 * ### Architecture:
 * - Uses 100Hz timer interrupts (10ms resolution) from the PIT
 * - Thread-safe global counters protected by spin locks
 * - Designed for integration with future scheduler implementation
 * - Provides foundation for process time slicing and preemptive multitasking
 *
 * ### Timer Resolution:
 * - **PIT Frequency**: 100Hz (configurable via PIC initialization)
 * - **Uptime Resolution**: 10 milliseconds per tick
 * - **Scheduler Quantum**: Currently 10ms (1:1 with timer interrupts)
 * - **Future Enhancement**: APIC timer integration for microsecond precision
 *
 * ### Integration Points:
 * - Called from `timer_interrupt_handler` in IDT module
 * - Logs scheduler ticks every second for system monitoring
 * - Provides API for kernel modules requiring timing services
 * - Foundation for future scheduler tick distribution
 *
 * ### Performance Considerations:
 * - Minimal interrupt handler overhead
 * - Lock contention minimized through brief critical sections
 * - Logging throttled to prevent interrupt handler flooding
 * - Sleep function uses HLT instruction for power efficiency
 *
 * This timer system transforms basic hardware timer interrupts into meaningful
 * kernel timing services, enabling precise uptime tracking and laying the
 * groundwork for sophisticated process scheduling.
 */

use spin::Mutex;

/// Global uptime counter in milliseconds since boot
static UPTIME_MS: Mutex<u64> = Mutex::new(0);

/// Scheduler tick counter - increments every scheduler quantum
static SCHEDULER_TICKS: Mutex<u64> = Mutex::new(0);

/// How many timer interrupts equal one scheduler tick
/// With 100Hz timer, this gives us ~10ms scheduler quantum
const TIMER_INTERRUPTS_PER_SCHEDULER_TICK: u64 = 1;

/// Internal counter for timer interrupts
static TIMER_INTERRUPT_COUNT: Mutex<u64> = Mutex::new(0);

/// Called from the timer interrupt handler (IRQ0)
/// This should be called exactly once per timer interrupt
pub fn on_timer_interrupt() {
    let mut interrupt_count = TIMER_INTERRUPT_COUNT.lock();
    let mut uptime = UPTIME_MS.lock();

    *interrupt_count += 1;

    // With 100Hz timer, each interrupt represents 10ms
    *uptime += 10;

    // Check if we should trigger a scheduler tick
    if *interrupt_count % TIMER_INTERRUPTS_PER_SCHEDULER_TICK == 0 {
        let mut scheduler_ticks = SCHEDULER_TICKS.lock();
        *scheduler_ticks += 1;

        // Log every 100 scheduler ticks (1 second with current settings)
        if *scheduler_ticks % 100 == 0 {
            log::info!(
                "Scheduler tick: {}, Uptime: {}ms",
                *scheduler_ticks,
                *uptime
            );
        }

        // Future: call scheduler here
        // scheduler::tick();
    }
}

/// Get current system uptime in milliseconds
pub fn uptime_ms() -> u64 {
    *UPTIME_MS.lock()
}

/// Get current scheduler tick count
pub fn scheduler_ticks() -> u64 {
    *SCHEDULER_TICKS.lock()
}

/// Get total timer interrupt count
pub fn timer_interrupt_count() -> u64 {
    *TIMER_INTERRUPT_COUNT.lock()
}

/// Sleep for approximately the given number of milliseconds
/// Note: This is a busy-wait implementation and should be replaced
/// with proper scheduler-based sleeping in the future
pub fn sleep_ms(ms: u64) {
    let start_time = uptime_ms();
    while uptime_ms() - start_time < ms {
        x86_64::instructions::hlt();
    }
}
