/*
 * Direct Syscall Path Test
 *
 * This test validates the SYSCALL/SYSRET mechanism by manually jumping
 * to Ring 3 (userspace) and executing a syscall instruction.
 *
 * This is a minimal test that doesn't require:
 * - Full scheduler integration
 * - ELF loader
 * - Process creation
 *
 * It manually:
 * 1. Allocates a userspace page
 * 2. Writes test code (syscall instruction)
 * 3. Sets up kernel stack for syscall entry
 * 4. Uses iretq to jump to Ring 3
 * 5. Executes syscall
 * 6. Returns to kernel
 */

use crate::memory::{phys, paging};
use crate::syscall;
use core::arch::asm;
use x86_64::{
    VirtAddr,
    structures::paging::PageTableFlags,
};

/// Test the syscall path by manually jumping to Ring 3 and back
///
/// This creates a minimal userspace environment, executes a syscall,
/// and verifies we can return safely.
///
/// Test procedure:
/// 1. Allocate userspace page at 0x400000
/// 2. Write test code: mov rax, 158; syscall; hlt
/// 3. Set SYSCALL_SCRATCH.kernel_rsp to current stack
/// 4. Jump to Ring 3 via iretq
/// 5. Execute syscall (SYS_YIELD)
/// 6. Return to kernel
/// 7. Clean up
pub fn test_syscall_ring3_transition() {
    log::info!("========================================");
    log::info!("DIRECT SYSCALL PATH TEST");
    log::info!("========================================");
    log::info!("");
    log::info!("This test validates SYSCALL/SYSRET by:");
    log::info!("  1. Jumping to Ring 3 (userspace)");
    log::info!("  2. Executing syscall instruction");
    log::info!("  3. Returning to Ring 0 (kernel)");
    log::info!("");
    log::info!("NOTE: This test uses kernel page tables, so it maps");
    log::info!("      pages in kernel-accessible memory for simplicity.");
    log::info!("");

    // Step 1: Allocate and map userspace pages
    // Try high userspace addresses (near kernel boundary) to avoid huge pages
    log::info!("Step 1: Allocating and mapping userspace pages...");

    let userspace_code_addr = VirtAddr::new(0x0000_7fff_f000_0000);
    let userspace_stack_addr = VirtAddr::new(0x0000_7fff_f100_0000);
    let userspace_stack_top = userspace_stack_addr + 4096u64;

    // Allocate physical frames
    let code_frame = match phys::alloc_frame() {
        Some(f) => f,
        None => {
            log::error!("  FAIL: Could not allocate frame for code");
            return;
        }
    };

    let stack_frame = match phys::alloc_frame() {
        Some(f) => f,
        None => {
            log::error!("  FAIL: Could not allocate frame for stack");
            phys::free_frame(code_frame);
            return;
        }
    };

    log::info!("  Allocated code frame at physical 0x{:x}", code_frame.start_address());
    log::info!("  Allocated stack frame at physical 0x{:x}", stack_frame.start_address());

    // Map code page with USER_ACCESSIBLE
    let code_phys = x86_64::PhysAddr::new(code_frame.start_address());
    let code_flags = PageTableFlags::PRESENT
        | PageTableFlags::WRITABLE
        | PageTableFlags::USER_ACCESSIBLE;

    if let Err(e) = paging::map_user_page(userspace_code_addr, code_phys, code_flags) {
        log::error!("  FAIL: Could not map code page: {:?}", e);
        phys::free_frame(code_frame);
        phys::free_frame(stack_frame);
        return;
    }

    // Map stack page with USER_ACCESSIBLE
    let stack_phys = x86_64::PhysAddr::new(stack_frame.start_address());
    let stack_flags = PageTableFlags::PRESENT
        | PageTableFlags::WRITABLE
        | PageTableFlags::USER_ACCESSIBLE;

    if let Err(e) = paging::map_user_page(userspace_stack_addr, stack_phys, stack_flags) {
        log::error!("  FAIL: Could not map stack page: {:?}", e);
        let _ = paging::unmap_page(userspace_code_addr);
        phys::free_frame(code_frame);
        phys::free_frame(stack_frame);
        return;
    }

    log::info!("  ✓ Mapped code at 0x{:x}", userspace_code_addr.as_u64());
    log::info!("  ✓ Mapped stack at 0x{:x}", userspace_stack_addr.as_u64());

    // Step 2: Write test code to userspace page
    log::info!("Step 2: Writing test code (syscall instruction)...");

    // Machine code for:
    //   mov rax, 158    ; SYS_YIELD (0x9e)
    //   syscall         ; Make the syscall
    //   mov rax, 60     ; SYS_EXIT (shouldn't reach here)
    //   mov rdi, 0      ; exit code 0
    //   syscall         ; Exit if we somehow return
    #[allow(clippy::unusual_byte_groupings)]
    let test_code: &[u8] = &[
        0x48, 0xc7, 0xc0, 0x9e, 0x00, 0x00, 0x00,  // mov rax, 158
        0x0f, 0x05,                                  // syscall
        0x48, 0xc7, 0xc0, 0x3c, 0x00, 0x00, 0x00,  // mov rax, 60
        0x48, 0x31, 0xff,                            // xor rdi, rdi
        0x0f, 0x05,                                  // syscall
    ];

    unsafe {
        let dest = userspace_code_addr.as_mut_ptr::<u8>();
        core::ptr::copy_nonoverlapping(test_code.as_ptr(), dest, test_code.len());
    }

    log::info!("  ✓ Test code written ({} bytes)", test_code.len());

    // Step 3: Set kernel stack for syscall entry
    log::info!("Step 3: Setting kernel stack for syscall entry...");

    // Get current kernel stack pointer (approximate - we're on it now)
    // We'll use a high address as the kernel stack top
    let mut kernel_stack_top: u64;
    unsafe {
        asm!("mov {}, rsp", out(reg) kernel_stack_top);
    }
    // Round up to nearest page and add some headroom (4KB)
    kernel_stack_top = (kernel_stack_top + 0xfff) & !0xfff;
    kernel_stack_top += 0x1000;

    log::info!("  Kernel stack top: 0x{:x}", kernel_stack_top);

    // Disable interrupts for safety during set_kernel_stack
    x86_64::instructions::interrupts::disable();
    syscall::set_kernel_stack(kernel_stack_top);
    x86_64::instructions::interrupts::enable();

    log::info!("  ✓ Kernel stack configured");

    // Step 4: Jump to Ring 3 and execute syscall
    log::info!("Step 4: Jumping to Ring 3 (userspace)...");
    log::info!("  User code at: 0x{:x}", userspace_code_addr.as_u64());
    log::info!("  User stack at: 0x{:x}", userspace_stack_top.as_u64());

    let result = unsafe {
        jump_to_userspace(
            userspace_code_addr.as_u64(),
            userspace_stack_top.as_u64(),
        )
    };

    log::info!("");
    if result == 0 {
        log::info!("  ✓ SYSCALL PATH TEST PASSED!");
        log::info!("  ✓ Successfully jumped to Ring 3");
        log::info!("  ✓ Executed syscall instruction");
        log::info!("  ✓ Returned to Ring 0");
    } else {
        log::error!("  ✗ SYSCALL PATH TEST FAILED!");
        log::error!("  Unexpected return value: {}", result);
    }

    // Step 5: Clean up
    log::info!("Step 5: Cleaning up...");
    let _ = paging::unmap_page(userspace_code_addr);
    let _ = paging::unmap_page(userspace_stack_addr);
    phys::free_frame(code_frame);
    phys::free_frame(stack_frame);
    log::info!("  ✓ Resources freed");

    log::info!("");
    log::info!("========================================");
    log::info!("TEST COMPLETE");
    log::info!("========================================");
}

/// Jump to userspace and return
///
/// This function uses iretq to transition to Ring 3, execute code,
/// and return via syscall.
///
/// # Safety
/// - user_rip must point to valid, executable userspace code
/// - user_rsp must point to valid userspace stack
/// - SYSCALL_SCRATCH.kernel_rsp must be set
///
/// # Returns
/// 0 on success (syscall returned cleanly)
#[inline(never)]
unsafe fn jump_to_userspace(user_rip: u64, user_rsp: u64) -> isize {
    let result: isize;

    // User segment selectors (from GDT):
    // - User data (SS): 0x20 + 3 (RPL) = 0x23
    // - User code (CS): 0x28 + 3 (RPL) = 0x2B
    const USER_DATA_SELECTOR: u64 = 0x23;
    const USER_CODE_SELECTOR: u64 = 0x2B;

    // RFLAGS value for userspace:
    // - IF (bit 9) = 1 (interrupts enabled)
    // - Reserved (bit 1) = 1 (always set)
    const USER_RFLAGS: u64 = 0x202;

    unsafe {
        asm!(
            // Build iretq frame on kernel stack
            "push {user_ss}",      // SS (user data segment)
            "push {user_rsp}",     // RSP (user stack pointer)
            "push {user_rflags}",  // RFLAGS
            "push {user_cs}",      // CS (user code segment)
            "push {user_rip}",     // RIP (user instruction pointer)

            // Jump to Ring 3
            "iretq",

            // We return here after syscall
            "mov {result}, rax",   // Save syscall return value

            user_ss = in(reg) USER_DATA_SELECTOR,
            user_rsp = in(reg) user_rsp,
            user_rflags = in(reg) USER_RFLAGS,
            user_cs = in(reg) USER_CODE_SELECTOR,
            user_rip = in(reg) user_rip,
            result = out(reg) result,
        );
    }

    result
}
