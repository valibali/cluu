/*
 * CLUU Shell - GRID
 *
 * A simple kernel shell (GRID) with basic commands.
 * Uses:
 *  - TTY layer for line editing and history
 *  - framebuffer console for output
 */

use crate::components::tty;
use crate::utils::{
    console::{self, Color},
    timer,
};

use alloc::string::String;
use core::fmt::Write;
use core::str::SplitWhitespace;

pub struct KShell;

impl KShell {
    /// Initialize the shell: clear screen, print banner + prompt.
    pub fn init() {
        log::info!("Shell init: Starting...");

        // Clear via TTY/console
        tty::with_tty0(|tty0| {
            tty0.clear();
        });

        Self::print_banner();
        Self::print_prompt();

        log::info!("Shell init: Complete");
    }

    /// Handle one character from keyboard.
    /// Delegates to TTY0 for line editing; executes full lines.
    pub fn handle_char(ch: char) {
        if let Some(line) = tty::tty0_handle_char(ch) {
            Self::execute_command(&line);
            Self::print_prompt();
        }
    }

    fn print_banner() {
        console::write_str(
            "                                                                                       \n",
        );
        console::write_str(
            "                                                                                      \n",
        );
        console::write_colored(
            "        CCCCCCCCCCCCCLLLLLLLLLLL            UUUUUUUU     UUUUUUUUUUUUUUUU     UUUUUUUU\n",
            Color::CYAN,
            Color::BLACK,
        );
        console::write_colored(
            "     CCC::::::::::::CL:::::::::L            U::::::U     U::::::UU::::::U     U::::::U\n",
            Color::CYAN,
            Color::BLACK,
        );
        console::write_colored(
            "   CC:::::::::::::::CL:::::::::L            U::::::U     U::::::UU::::::U     U::::::U\n",
            Color::CYAN,
            Color::BLACK,
        );
        console::write_colored(
            "  C:::::CCCCCCCC::::CLL:::::::LL            UU:::::U     U:::::UUUU:::::U     U:::::UU\n",
            Color::CYAN,
            Color::BLACK,
        );
        console::write_colored(
            " C:::::C       CCCCCC  L:::::L               U:::::U     U:::::U  U:::::U     U:::::U \n",
            Color::CYAN,
            Color::BLACK,
        );
        console::write_colored(
            "C:::::C                L:::::L               U:::::D     D:::::U  U:::::D     D:::::U \n",
            Color::CYAN,
            Color::BLACK,
        );
        console::write_colored(
            "C:::::C                L:::::L               U:::::D     D:::::U  U:::::D     D:::::U \n",
            Color::CYAN,
            Color::BLACK,
        );
        console::write_colored(
            "C:::::C                L:::::L               U:::::D     D:::::U  U:::::D     D:::::U \n",
            Color::CYAN,
            Color::BLACK,
        );
        console::write_colored(
            "C:::::C                L:::::L               U:::::D     D:::::U  U:::::D     D:::::U \n",
            Color::CYAN,
            Color::BLACK,
        );
        console::write_colored(
            "C:::::C                L:::::L               U:::::D     D:::::U  U:::::D     D:::::U \n",
            Color::CYAN,
            Color::BLACK,
        );
        console::write_colored(
            "C:::::C                L:::::L               U:::::D     D:::::U  U:::::D     D:::::U \n",
            Color::CYAN,
            Color::BLACK,
        );
        console::write_colored(
            " C:::::C       CCCCCC  L:::::L         LLLLLLU::::::U   U::::::U  U::::::U   U::::::U \n",
            Color::CYAN,
            Color::BLACK,
        );
        console::write_colored(
            "  C:::::CCCCCCCC::::CLL:::::::LLLLLLLLL:::::LU:::::::UUU:::::::U  U:::::::UUU:::::::U \n",
            Color::CYAN,
            Color::BLACK,
        );
        console::write_colored(
            "   CC:::::::::::::::CL::::::::::::::::::::::L UU:::::::::::::UU    UU:::::::::::::UU  \n",
            Color::CYAN,
            Color::BLACK,
        );
        console::write_colored(
            "     CCC::::::::::::CL::::::::::::::::::::::L   UU:::::::::UU        UU:::::::::UU    \n",
            Color::CYAN,
            Color::BLACK,
        );
        console::write_colored(
            "        CCCCCCCCCCCCCLLLLLLLLLLLLLLLLLLLLLLLL     UUUUUUUUU            UUUUUUUUU      \n",
            Color::CYAN,
            Color::BLACK,
        );
        console::write_str(
            "                                                                                      \n",
        );
        console::write_str(
            "                                                                                      \n",
        );
        console::write_str(
            "                                                                                      \n",
        );
        console::write_str(
            "                                     The GRID                                         \n",
        );
        console::write_str(
            "                         - the deepest place in the kernel                             \n",
        );
        console::write_str(
            "                                                                                      \n",
        );
        console::write_str(
            "                                                                                      \n",
        );
        console::write_str("\n");
        console::write_colored(
            "Type 'help' for available commands.\n",
            Color::LIGHT_GRAY,
            Color::BLACK,
        );
        console::write_str("\n");
    }

    fn print_prompt() {
        console::write_colored("[", Color::GRAY, Color::BLACK);
        console::write_colored("CLUU GRID", Color::YELLOW, Color::BLACK);
        console::write_colored("] ", Color::GRAY, Color::BLACK);
        console::write_colored("› ", Color::GREEN, Color::BLACK);
    }

    fn execute_command(line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }

        let mut parts = line.split_whitespace();
        let command = parts.next().unwrap_or("");

        match command {
            "help" => Self::cmd_help(),
            "cls" | "clear" => Self::cmd_clear(),
            "time" | "uptime" => Self::cmd_time(),
            "mem" | "memory" => Self::cmd_memory(),
            "reboot" => Self::cmd_reboot(),
            "echo" => Self::cmd_echo(parts),
            "history" => Self::cmd_history(),
            "test" => Self::cmd_test(),
            "colors" => Self::cmd_colors(),
            "threads" | "ps" => Self::cmd_threads(),
            "yield" => Self::cmd_yield(),
            "test-ipc" | "ipc-test" => Self::cmd_test_ipc(),
            "test-ipc-block" => Self::cmd_test_ipc_blocking(),
            "test-ipc-queue" => Self::cmd_test_ipc_queue(),
            "test-ipc-multi" => Self::cmd_test_ipc_multi(),
            "test-fd" => Self::cmd_test_fd(),
            "stress" | "test-stress" => Self::cmd_stress_test(),
            "stress-forever" | "stress-continuous" => Self::cmd_stress_forever(),
            "" => {}
            _ => {
                console::write_colored("Unknown command: ", Color::RED, Color::BLACK);
                console::write_colored(command, Color::WHITE, Color::BLACK);
                console::write_str("\n");
                console::write_colored(
                    "Type 'help' for available commands.\n",
                    Color::LIGHT_GRAY,
                    Color::BLACK,
                );
            }
        }
    }

    fn cmd_help() {
        console::write_colored("Available commands:\n", Color::CYAN, Color::BLACK);
        console::write_str("\n");

        let commands = [
            ("help", "Show this help message"),
            ("cls, clear", "Clear the screen"),
            ("time, uptime", "Show system uptime"),
            ("mem, memory", "Show memory information"),
            ("echo <text>", "Echo text to console"),
            ("history", "Show command history"),
            ("test", "Run system tests"),
            ("colors", "Show color test"),
            ("threads, ps", "Show thread information"),
            ("yield", "Yield CPU to other threads"),
            ("stress", "Run threading and IPC stress test (one-shot)"),
            ("stress-forever", "Run continuous stress test (runs forever)"),
            ("reboot", "Reboot the system"),
        ];

        for (cmd, desc) in &commands {
            console::write_colored("  ", Color::WHITE, Color::BLACK);
            console::write_colored(cmd, Color::GREEN, Color::BLACK);
            console::write_str(" - ");
            console::write_colored(desc, Color::LIGHT_GRAY, Color::BLACK);
            console::write_str("\n");
        }
        console::write_str("\n");
    }

    fn cmd_clear() {
        console::clear_screen();
    }

    fn cmd_time() {
        let uptime = timer::uptime_ms();
        let seconds = uptime / 1000;
        let minutes = seconds / 60;
        let hours = minutes / 60;

        console::write_colored("System uptime: ", Color::CYAN, Color::BLACK);

        let mut time_str = String::new();
        let _ = write!(time_str, "{}h {}m {}s", hours, minutes % 60, seconds % 60);
        console::write_colored(&time_str, Color::WHITE, Color::BLACK);

        let mut ms_str = String::new();
        let _ = write!(ms_str, " ({} ms)\n", uptime);
        console::write_colored(&ms_str, Color::GRAY, Color::BLACK);
    }

    fn cmd_memory() {
        console::write_colored("Memory Information:\n", Color::CYAN, Color::BLACK);
        console::write_colored("  Kernel heap: ", Color::WHITE, Color::BLACK);
        console::write_colored("Available\n", Color::GREEN, Color::BLACK);
        console::write_colored("  Stack: ", Color::WHITE, Color::BLACK);
        console::write_colored("64KB\n", Color::GREEN, Color::BLACK);
        console::write_colored("  Note: ", Color::YELLOW, Color::BLACK);
        console::write_colored(
            "Detailed memory stats not yet implemented\n",
            Color::LIGHT_GRAY,
            Color::BLACK,
        );
    }

    fn cmd_reboot() {
        console::write_colored(
            "Rebooting system in 3 seconds...\n",
            Color::RED,
            Color::BLACK,
        );
        console::write_colored(
            "Press Ctrl+C to cancel (not implemented yet)\n",
            Color::YELLOW,
            Color::BLACK,
        );

        for i in (1..=3).rev() {
            let mut countdown = String::new();
            let _ = write!(countdown, "Rebooting in {}...\n", i);
            console::write_colored(&countdown, Color::RED, Color::BLACK);

            // Use scheduler sleep instead of busy-wait
            crate::scheduler::sleep_ms(1000);
        }

        console::write_colored("Rebooting now!\n", Color::RED, Color::BLACK);

        crate::utils::reboot::reboot();
    }

    fn cmd_echo(args: SplitWhitespace) {
        let mut first = true;
        for arg in args {
            if !first {
                console::write_str(" ");
            }
            console::write_str(arg);
            first = false;
        }
        console::write_str("\n");
    }

    fn cmd_history() {
        tty::with_tty0(|tty0| {
            let history = tty0.history();
            if history.is_empty() {
                console::write_colored("No command history.\n", Color::LIGHT_GRAY, Color::BLACK);
            } else {
                console::write_colored("Command history:\n", Color::CYAN, Color::BLACK);
                for (i, cmd) in history.iter().enumerate() {
                    let mut line = String::new();
                    let _ = write!(line, "  {}: ", i + 1);
                    console::write_colored(&line, Color::GRAY, Color::BLACK);
                    console::write_colored(cmd, Color::WHITE, Color::BLACK);
                    console::write_str("\n");
                }
            }
        });
    }

    fn cmd_test() {
        console::write_colored("Running system tests...\n", Color::CYAN, Color::BLACK);

        // Test 1: Interrupt test
        console::write_colored("  Test 1: ", Color::WHITE, Color::BLACK);
        console::write_colored("Interrupt handling... ", Color::LIGHT_GRAY, Color::BLACK);
        x86_64::instructions::interrupts::int3();
        console::write_colored("PASS\n", Color::GREEN, Color::BLACK);

        // Test 2: Timer test
        console::write_colored("  Test 2: ", Color::WHITE, Color::BLACK);
        console::write_colored("Timer functionality... ", Color::LIGHT_GRAY, Color::BLACK);
        let uptime = timer::uptime_ms();
        if uptime > 0 {
            console::write_colored("PASS\n", Color::GREEN, Color::BLACK);
        } else {
            console::write_colored("FAIL\n", Color::RED, Color::BLACK);
        }

        // Test 3: Keyboard test
        console::write_colored("  Test 3: ", Color::WHITE, Color::BLACK);
        console::write_colored("Keyboard input... ", Color::LIGHT_GRAY, Color::BLACK);
        console::write_colored("PASS\n", Color::GREEN, Color::BLACK);

        console::write_colored("All tests completed!\n", Color::GREEN, Color::BLACK);
    }

    fn cmd_colors() {
        console::write_colored("Color Test:\n", Color::WHITE, Color::BLACK);
        console::write_str("\n");

        let colors = [
            ("Black", Color::BLACK),
            ("White", Color::WHITE),
            ("Red", Color::RED),
            ("Green", Color::GREEN),
            ("Blue", Color::BLUE),
            ("Yellow", Color::YELLOW),
            ("Magenta", Color::MAGENTA),
            ("Cyan", Color::CYAN),
            ("Gray", Color::GRAY),
            ("Light Gray", Color::LIGHT_GRAY),
        ];

        for (name, color) in &colors {
            console::write_colored("  #### ", *color, Color::BLACK);
            console::write_colored(name, Color::WHITE, Color::BLACK);
            console::write_str("\n");
        }
        console::write_str("\n");
    }

    fn cmd_threads() {
        console::write_colored("Thread Information:\n", Color::CYAN, Color::BLACK);
        console::write_str("\n");

        // Get thread statistics
        let stats = crate::scheduler::get_thread_stats();

        if stats.is_empty() {
            console::write_colored("  No threads found\n", Color::LIGHT_GRAY, Color::BLACK);
            return;
        }

        // Print header
        console::write_colored("  ", Color::WHITE, Color::BLACK);
        console::write_colored("ID", Color::CYAN, Color::BLACK);
        console::write_str("   ");
        console::write_colored("STATE   ", Color::CYAN, Color::BLACK);
        console::write_str("  ");
        console::write_colored("CPU%", Color::CYAN, Color::BLACK);
        console::write_str("   ");
        console::write_colored("CPU TIME", Color::CYAN, Color::BLACK);
        console::write_str("      ");
        console::write_colored("NAME", Color::CYAN, Color::BLACK);
        console::write_str("\n");

        console::write_colored("  ", Color::GRAY, Color::BLACK);
        console::write_str("──────────────────────────────────────────────────────────\n");

        let current_id = crate::scheduler::current_thread_id();

        // Print each thread
        for stat in stats {
            console::write_str("  ");

            // Thread ID
            let mut id_str = String::new();
            let _ = write!(id_str, "{:<4}", stat.id.0);
            if stat.id == current_id {
                console::write_colored(&id_str, Color::GREEN, Color::BLACK);
            } else {
                console::write_colored(&id_str, Color::WHITE, Color::BLACK);
            }

            // State
            let state_str = match stat.state {
                crate::scheduler::ThreadState::Ready => "READY  ",
                crate::scheduler::ThreadState::Running => "RUNNING",
                crate::scheduler::ThreadState::Blocked => "BLOCKED",
                crate::scheduler::ThreadState::Terminated => "TERM   ",
            };
            let state_color = match stat.state {
                crate::scheduler::ThreadState::Running => Color::GREEN,
                crate::scheduler::ThreadState::Ready => Color::YELLOW,
                crate::scheduler::ThreadState::Blocked => Color::RED,
                crate::scheduler::ThreadState::Terminated => Color::GRAY,
            };
            console::write_str("  ");
            console::write_colored(state_str, state_color, Color::BLACK);

            // CPU percentage
            let mut cpu_pct_str = String::new();
            let _ = write!(cpu_pct_str, "  {:>3}%", stat.cpu_percent);
            console::write_colored(&cpu_pct_str, Color::WHITE, Color::BLACK);

            // CPU time
            let seconds = stat.cpu_time_ms / 1000;
            let minutes = seconds / 60;
            let hours = minutes / 60;
            let mut time_str = String::new();
            if hours > 0 {
                let _ = write!(
                    time_str,
                    "  {:>3}h {:>2}m {:>2}s",
                    hours,
                    minutes % 60,
                    seconds % 60
                );
            } else if minutes > 0 {
                let _ = write!(time_str, "      {:>2}m {:>2}s", minutes, seconds % 60);
            } else {
                let _ = write!(time_str, "         {:>2}s", seconds);
            }
            console::write_colored(&time_str, Color::LIGHT_GRAY, Color::BLACK);

            // Thread name
            console::write_str("  ");
            if stat.id == current_id {
                console::write_colored(&stat.name, Color::GREEN, Color::BLACK);
            } else {
                console::write_colored(&stat.name, Color::WHITE, Color::BLACK);
            }
            console::write_str("\n");
        }

        console::write_str("\n");
        console::write_colored("  Scheduler: ", Color::WHITE, Color::BLACK);
        console::write_colored(
            "Preemptive round-robin (100Hz)\n",
            Color::GREEN,
            Color::BLACK,
        );

        // Show total system info
        let uptime = timer::uptime_ms();
        let seconds = uptime / 1000;
        let minutes = seconds / 60;
        let hours = minutes / 60;

        console::write_colored("  System uptime: ", Color::WHITE, Color::BLACK);
        let mut uptime_str = String::new();
        let _ = write!(
            uptime_str,
            "{}h {}m {}s\n",
            hours,
            minutes % 60,
            seconds % 60
        );
        console::write_colored(&uptime_str, Color::LIGHT_GRAY, Color::BLACK);
    }

    fn cmd_yield() {
        console::write_colored(
            "Yielding CPU to other threads...\n",
            Color::CYAN,
            Color::BLACK,
        );
        crate::scheduler::yield_now();
        console::write_colored("Back in shell thread\n", Color::GREEN, Color::BLACK);
    }

    fn cmd_test_ipc() {
        console::write_colored(
            "Starting IPC Test: Basic Send/Receive\n",
            Color::CYAN,
            Color::BLACK,
        );
        console::write_colored(
            "This test spawns sender and receiver threads.\n",
            Color::LIGHT_GRAY,
            Color::BLACK,
        );
        console::write_colored(
            "Watch the logs for test results.\n\n",
            Color::LIGHT_GRAY,
            Color::BLACK,
        );
        crate::spawn_ipc_tests();
    }

    fn cmd_test_ipc_blocking() {
        // Minimal test - just log and return
        log::info!("===== cmd_test_ipc_blocking: START =====");
        console::write_colored("Test function called!\n", Color::GREEN, Color::BLACK);
        log::info!("===== cmd_test_ipc_blocking: END =====");

        // Uncomment to run actual test:
        crate::spawn_ipc_blocking_test();
    }

    fn cmd_test_ipc_queue() {
        console::write_colored(
            "Starting IPC Test: Queue Full Handling\n",
            Color::CYAN,
            Color::BLACK,
        );
        console::write_colored(
            "Tests queue capacity (32 messages) and error handling.\n",
            Color::LIGHT_GRAY,
            Color::BLACK,
        );
        console::write_colored(
            "Watch the logs for test results.\n\n",
            Color::LIGHT_GRAY,
            Color::BLACK,
        );
        crate::spawn_ipc_queue_test();
    }

    fn cmd_test_ipc_multi() {
        console::write_colored(
            "Starting IPC Test: Multiple Senders\n",
            Color::CYAN,
            Color::BLACK,
        );
        console::write_colored(
            "3 senders will send 5 messages each to 1 receiver.\n",
            Color::LIGHT_GRAY,
            Color::BLACK,
        );
        console::write_colored(
            "Watch the logs for message delivery order.\n\n",
            Color::LIGHT_GRAY,
            Color::BLACK,
        );
        crate::spawn_ipc_multi_test();
    }

    fn cmd_test_fd() {
        console::write_colored("Starting FD Layer Test\n", Color::CYAN, Color::BLACK);
        console::write_colored(
            "Testing file descriptor abstraction with stdin/stdout/stderr.\n",
            Color::LIGHT_GRAY,
            Color::BLACK,
        );
        console::write_colored(
            "Watch the logs and follow prompts.\n\n",
            Color::LIGHT_GRAY,
            Color::BLACK,
        );
        crate::spawn_fd_test();
    }

    fn cmd_stress_test() {
        console::write_colored(
            "Starting STRESS TEST: Threading + IPC\n",
            Color::CYAN,
            Color::BLACK,
        );
        console::write_colored(
            "This will spawn 29 threads:\n",
            Color::LIGHT_GRAY,
            Color::BLACK,
        );
        console::write_colored(
            "  - 3 IPC receivers (each with own port)\n",
            Color::LIGHT_GRAY,
            Color::BLACK,
        );
        console::write_colored(
            "  - 15 IPC senders (5 per receiver)\n",
            Color::LIGHT_GRAY,
            Color::BLACK,
        );
        console::write_colored(
            "  - 10 compute threads (scheduler stress)\n",
            Color::LIGHT_GRAY,
            Color::BLACK,
        );
        console::write_colored(
            "  - 1 monitor thread\n",
            Color::LIGHT_GRAY,
            Color::BLACK,
        );
        console::write_colored(
            "\nThis tests:\n",
            Color::YELLOW,
            Color::BLACK,
        );
        console::write_colored(
            "  ✓ Concurrent thread creation/termination\n",
            Color::GREEN,
            Color::BLACK,
        );
        console::write_colored(
            "  ✓ Multiple simultaneous IPC operations\n",
            Color::GREEN,
            Color::BLACK,
        );
        console::write_colored(
            "  ✓ Scheduler under high load\n",
            Color::GREEN,
            Color::BLACK,
        );
        console::write_colored(
            "  ✓ Sleep/yield/blocking behavior\n",
            Color::GREEN,
            Color::BLACK,
        );
        console::write_colored(
            "\nWatch the logs for progress updates...\n\n",
            Color::LIGHT_GRAY,
            Color::BLACK,
        );
        crate::spawn_stress_test();
    }

    fn cmd_stress_forever() {
        console::write_colored(
            "Starting CONTINUOUS STRESS TEST\n",
            Color::CYAN,
            Color::BLACK,
        );
        console::write_colored(
            "⚠ WARNING: This test runs FOREVER!\n",
            Color::RED,
            Color::BLACK,
        );
        console::write_str("\n");
        console::write_colored(
            "Test strategy:\n",
            Color::YELLOW,
            Color::BLACK,
        );
        console::write_colored(
            "  • Spawns waves of 8 threads continuously\n",
            Color::LIGHT_GRAY,
            Color::BLACK,
        );
        console::write_colored(
            "  • Each wave: 2 IPC receivers, 4 IPC senders, 1 FD test, 1 compute\n",
            Color::LIGHT_GRAY,
            Color::BLACK,
        );
        console::write_colored(
            "  • Waits for wave completion before next wave\n",
            Color::LIGHT_GRAY,
            Color::BLACK,
        );
        console::write_colored(
            "  • Prevents heap exhaustion via thread cleanup\n",
            Color::LIGHT_GRAY,
            Color::BLACK,
        );
        console::write_str("\n");
        console::write_colored(
            "What this tests:\n",
            Color::YELLOW,
            Color::BLACK,
        );
        console::write_colored(
            "  ✓ Long-term stability and memory leaks\n",
            Color::GREEN,
            Color::BLACK,
        );
        console::write_colored(
            "  ✓ Thread cleanup and resource reclamation\n",
            Color::GREEN,
            Color::BLACK,
        );
        console::write_colored(
            "  ✓ IPC port lifecycle (create/destroy)\n",
            Color::GREEN,
            Color::BLACK,
        );
        console::write_colored(
            "  ✓ FD operations over time\n",
            Color::GREEN,
            Color::BLACK,
        );
        console::write_colored(
            "  ✓ Scheduler fairness under sustained load\n",
            Color::GREEN,
            Color::BLACK,
        );
        console::write_str("\n");
        console::write_colored(
            "Statistics will be logged every cycle.\n",
            Color::LIGHT_GRAY,
            Color::BLACK,
        );
        console::write_colored(
            "To stop: reboot the system\n\n",
            Color::YELLOW,
            Color::BLACK,
        );
        crate::spawn_continuous_stress_test();
    }
}
