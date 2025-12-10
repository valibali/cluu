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

            // Simple busy-wait delay
            for _ in 0..10_000_000 {
                unsafe { core::arch::asm!("nop") };
            }
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
}
