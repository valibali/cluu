/*
 * Global Descriptor Table (GDT) Implementation
 *
 * The Global Descriptor Table (GDT) is a fundamental data structure in x86_64 architecture
 * that defines memory segments and their properties. While x86_64 uses a flat memory model
 * where segmentation is largely unused, the GDT is still required for:
 *
 * 1. Code/Data Segment Descriptors: Define kernel and user code/data segments
 * 2. Task State Segment (TSS): Contains CPU state information and stack pointers
 * 3. Privilege Level Management: Enforces ring 0 (kernel) vs ring 3 (user) separation
 * 4. Interrupt Stack Table: Provides separate stacks for different interrupt types
 *
 * For our microkernel, the GDT is essential for:
 * - Setting up proper privilege levels for kernel vs userspace
 * - Providing separate interrupt stacks to prevent stack overflow attacks
 * - Enabling proper context switching between processes
 */

use lazy_static::lazy_static;
use x86_64::{
    VirtAddr,
    structures::{
        gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector},
        tss::TaskStateSegment,
    },
};

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;

lazy_static! {
    static ref TSS: TaskStateSegment = {
        let mut tss = TaskStateSegment::new();
        tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = {
            const STACK_SIZE: usize = 4096;
            static mut STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];

            let stack_start = VirtAddr::from_ptr(&raw const STACK);
            let stack_end = stack_start + STACK_SIZE as u64;
            stack_end
        };
        tss
    };
}
lazy_static! {
    static ref GDT: (GlobalDescriptorTable, Selectors) = {
        let mut gdt = GlobalDescriptorTable::new();
        let code_selector = gdt.append(Descriptor::kernel_code_segment());
        let data_selector = gdt.append(Descriptor::kernel_data_segment());
        let tss_selector = gdt.append(Descriptor::tss_segment(&TSS));
        let user_data_selector = gdt.append(Descriptor::user_data_segment());
        let user_code_selector = gdt.append(Descriptor::user_code_segment());
        (
            gdt,
            Selectors {
                code_selector,
                data_selector,
                tss_selector,
                user_data_selector,
                user_code_selector,
            },
        )
    };
}

struct Selectors {
    code_selector: SegmentSelector,
    data_selector: SegmentSelector,
    tss_selector: SegmentSelector,
    user_data_selector: SegmentSelector,
    user_code_selector: SegmentSelector,
}

/// Initialize the Global Descriptor Table
///
/// This function sets up the GDT with kernel code segment and TSS.
/// Must be called before IDT initialization.
pub fn init() {
    use x86_64::instructions::{
        segmentation::{CS, DS, ES, SS, FS, GS, Segment},
        tables::load_tss,
    };

    log::info!("Loading GDT...");
    GDT.0.load();

    unsafe {
        log::info!("Setting segment registers...");
        // Reload CS to the new code segment
        CS::set_reg(GDT.1.code_selector);

        // CRITICAL: reload all data segments to the new data segment
        // This fixes the triple fault by ensuring all segment registers
        // point to valid descriptors in our new GDT
        DS::set_reg(GDT.1.data_selector);
        ES::set_reg(GDT.1.data_selector);
        SS::set_reg(GDT.1.data_selector);
        FS::set_reg(GDT.1.data_selector);
        GS::set_reg(GDT.1.data_selector);

        log::info!("Loading TSS...");
        load_tss(GDT.1.tss_selector);
    }

    log::info!("GDT initialized successfully");
}

/// Get the user code segment selector (Ring 3)
///
/// Returns the segment selector for user mode code execution.
/// The selector has RPL=3 (Ring 3) set by the x86_64 crate.
pub fn user_code_selector() -> SegmentSelector {
    GDT.1.user_code_selector
}

/// Get the user data segment selector (Ring 3)
///
/// Returns the segment selector for user mode data access.
/// The selector has RPL=3 (Ring 3) set by the x86_64 crate.
pub fn user_data_selector() -> SegmentSelector {
    GDT.1.user_data_selector
}
