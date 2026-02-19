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
 *
 * Note: We avoid lazy_static here because the spin::Once atomic operations can
 * cause GPF under KVM during early boot. Instead, we use static mut with manual
 * initialization.
 */

use core::mem::MaybeUninit;
use x86_64::{
    structures::{
        gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector},
        tss::TaskStateSegment,
    },
    VirtAddr,
};

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;
pub const GPF_IST_INDEX: u16 = 1;
pub const PF_IST_INDEX: u16 = 2;

/// IST stacks - 4KB each, 16-byte aligned
#[repr(C, align(16))]
struct IstStack([u8; 4096]);

static mut DOUBLE_FAULT_STACK: IstStack = IstStack([0; 4096]);
static mut GPF_STACK: IstStack = IstStack([0; 4096]);
static mut PF_STACK: IstStack = IstStack([0; 4096]);

/// TSS - initialized at runtime
static mut TSS: MaybeUninit<TaskStateSegment> = MaybeUninit::uninit();

/// GDT - initialized at runtime
static mut GDT: MaybeUninit<GlobalDescriptorTable> = MaybeUninit::uninit();

/// Segment selectors - set during init
static mut SELECTORS: Selectors = Selectors {
    code_selector: SegmentSelector(0),
    data_selector: SegmentSelector(0),
    tss_selector: SegmentSelector(0),
    user_data_selector: SegmentSelector(0),
    user_code_selector: SegmentSelector(0),
};

/// Track if GDT is initialized
static mut GDT_INITIALIZED: bool = false;

#[derive(Clone, Copy)]
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
        segmentation::{Segment, CS, DS, ES, FS, GS, SS},
        tables::load_tss,
    };

    klibcluu::info("Loading GDT...");

    unsafe {
        // Initialize TSS first
        let df_stack_end = &raw const DOUBLE_FAULT_STACK as u64 + 4096;
        let gpf_stack_end = &raw const GPF_STACK as u64 + 4096;
        let pf_stack_end = &raw const PF_STACK as u64 + 4096;
        let mut tss = TaskStateSegment::new();
        tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = VirtAddr::new(df_stack_end);
        tss.interrupt_stack_table[GPF_IST_INDEX as usize] = VirtAddr::new(gpf_stack_end);
        tss.interrupt_stack_table[PF_IST_INDEX as usize] = VirtAddr::new(pf_stack_end);
        core::ptr::addr_of_mut!(TSS)
            .cast::<TaskStateSegment>()
            .write(tss);

        // Now initialize GDT with reference to TSS
        let mut gdt = GlobalDescriptorTable::new();
        let code_selector = gdt.append(Descriptor::kernel_code_segment());
        let data_selector = gdt.append(Descriptor::kernel_data_segment());
        let tss_selector = {
            let tss_ref: &'static TaskStateSegment =
                &*core::ptr::addr_of!(TSS).cast::<TaskStateSegment>();
            gdt.append(Descriptor::tss_segment(tss_ref))
        };
        let user_data_selector = gdt.append(Descriptor::user_data_segment());
        let user_code_selector = gdt.append(Descriptor::user_code_segment());

        SELECTORS = Selectors {
            code_selector,
            data_selector,
            tss_selector,
            user_data_selector,
            user_code_selector,
        };

        core::ptr::addr_of_mut!(GDT)
            .cast::<GlobalDescriptorTable>()
            .write(gdt);
        GDT_INITIALIZED = true;

        // Load the GDT
        let gdt_ref: &'static GlobalDescriptorTable =
            &*core::ptr::addr_of!(GDT).cast::<GlobalDescriptorTable>();
        gdt_ref.load();

        klibcluu::info("Setting segment registers...");
        // Reload CS to the new code segment
        CS::set_reg(SELECTORS.code_selector);

        // CRITICAL: reload all data segments to the new data segment
        // This fixes the triple fault by ensuring all segment registers
        // point to valid descriptors in our new GDT
        DS::set_reg(SELECTORS.data_selector);
        ES::set_reg(SELECTORS.data_selector);
        SS::set_reg(SELECTORS.data_selector);
        FS::set_reg(SELECTORS.data_selector);
        GS::set_reg(SELECTORS.data_selector);

        klibcluu::info("Loading TSS...");
        load_tss(SELECTORS.tss_selector);
    }

    klibcluu::info("GDT initialized successfully");
}

/// Get the kernel code segment selector (Ring 0)
///
/// Returns the segment selector for kernel mode code execution.
pub fn kernel_code_selector() -> SegmentSelector {
    unsafe { SELECTORS.code_selector }
}

/// Get the kernel data segment selector (Ring 0)
///
/// Returns the segment selector for kernel mode data access.
pub fn kernel_data_selector() -> SegmentSelector {
    unsafe { SELECTORS.data_selector }
}

/// Get the user code segment selector (Ring 3)
///
/// Returns the segment selector for user mode code execution.
/// The selector has RPL=3 (Ring 3) set by the x86_64 crate.
pub fn user_code_selector() -> SegmentSelector {
    unsafe { SELECTORS.user_code_selector }
}

/// Get the user data segment selector (Ring 3)
///
/// Returns the segment selector for user mode data access.
/// The selector has RPL=3 (Ring 3) set by the x86_64 crate.
pub fn user_data_selector() -> SegmentSelector {
    unsafe { SELECTORS.user_data_selector }
}

/// Update the TSS RSP0 field for userspace thread support
///
/// This must be called when switching to a userspace thread to ensure
/// the CPU knows where to find the kernel stack when handling interrupts/
/// exceptions while in Ring 3.
///
/// # Safety
/// Must be called with a valid kernel stack pointer. This function uses
/// unsafe code to mutate the static TSS, but it's safe because:
/// - Only called from the scheduler with interrupts disabled
/// - TSS RSP0 is only used by hardware during privilege level transitions
pub fn set_tss_rsp0(kernel_stack_ptr: u64) {
    // DEBUG: Log entry
    klibcluu::log_hex(klibcluu::LogLevel::Warn, "  stack_ptr=", kernel_stack_ptr);

    unsafe {
        if !GDT_INITIALIZED {
            klibcluu::error("set_tss_rsp0 called before GDT init!");
            return;
        }
        let tss_ptr = core::ptr::addr_of_mut!(TSS).cast::<TaskStateSegment>();
        (*tss_ptr).privilege_stack_table[0] = VirtAddr::new(kernel_stack_ptr);
    }
}
