use core::mem;

pub const R_X86_64_RELATIVE: u32 = 8;
pub const R_X86_64_GLOB_DAT: u32 = 6;
pub const R_X86_64_JUMP_SLOT: u32 = 7;
pub const R_X86_64_64: u32 = 1;

#[derive(Debug)]
pub enum RelocError {
    InvalidRelaEntry,
    UnknownRelocType(u32),
    OutOfBounds,
}

pub type RelocResult<T> = core::result::Result<T, RelocError>;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RelaEntry {
    pub offset: u64,
    pub info: u64,
    pub addend: i64,
}

impl RelaEntry {
    pub fn r_type(&self) -> u32 {
        (self.info & 0xFFFFFFFF) as u32
    }
    pub fn r_sym(&self) -> u32 {
        (self.info >> 32) as u32
    }
}

pub fn apply_relocs(
    base: usize,
    rela_start: u64,
    rela_size: u64,
    symtab: u64,
) -> RelocResult<()> {
    if rela_size == 0 {
        return Ok(());
    }
    let entry_size = mem::size_of::<RelaEntry>();
    let count = (rela_size as usize) / entry_size;
    let rela_ptr = (base + rela_start as usize) as *const RelaEntry;

    for i in 0..count {
        let entry = unsafe { &*rela_ptr.add(i) };
        let r_type = entry.r_type();
        let target = (base + entry.offset as usize) as *mut u64;

        match r_type {
            R_X86_64_RELATIVE => {
                let value = (base as i64 + entry.addend) as u64;
                unsafe { core::ptr::write_volatile(target, value); }
            }
            R_X86_64_64 | R_X86_64_GLOB_DAT | R_X86_64_JUMP_SLOT => {
                if symtab == 0 {
                    return Err(RelocError::OutOfBounds);
                }
                let sym_idx = entry.r_sym() as usize;
                let sym_entry_size = 24usize;
                let sym_ptr = (base + symtab as usize + sym_idx * sym_entry_size) as *const u64;
                let sym_value = unsafe { core::ptr::read_volatile(sym_ptr) };
                let value = (sym_value as i64 + entry.addend) as u64;
                unsafe { core::ptr::write_volatile(target, value); }
            }
            _ => {
                return Err(RelocError::UnknownRelocType(r_type));
            }
        }
    }

    Ok(())
}
