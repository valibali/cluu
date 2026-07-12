#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use cluu_ld_cluu::{
    apply_relocs,
    tls::{TlsBlock, TlsModule, __tls_get_addr, init_thread_tls},
    DynamicInfo,
};
use libcluu::debug_print;
use libcluu::Result;

#[repr(C)]
struct RelaEntry {
    offset: u64,
    info: u64,
    addend: i64,
}

#[repr(C)]
struct DynEntry {
    tag: u64,
    val: u64,
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    match run() {
        Ok(_) => 0,
        Err(e) => {
            let _ = debug_print(&format!("dynprobe: FAIL {:?}", e));
            -1
        }
    }
}

fn run() -> Result<()> {
    let _ = debug_print("dynprobe: starting");

    test_relocs()?;
    test_dynamic_parsing()?;
    test_tls()?;

    let _ = debug_print("dynprobe: PASS DYNPROBE_OK");
    Ok(())
}

fn test_relocs() -> Result<()> {
    let _ = debug_print("dynprobe: testing R_X86_64_RELATIVE reloc");

    let buf = alloc::vec![0u8; 256];
    let base = buf.as_ptr() as usize;

    let target_offsets = [0usize, 8, 16];
    let rela_offset = 64usize;
    let rela_count = 3;
    let addends: [i64; 3] = [0x1000, 0x2000, -0x100];

    let rela_ptr = (base + rela_offset) as *mut RelaEntry;
    for i in 0..rela_count {
        unsafe {
            *rela_ptr.add(i) = RelaEntry {
                offset: target_offsets[i] as u64,
                info: 8,
                addend: addends[i],
            };
        }
    }

    apply_relocs(
        base,
        rela_offset as u64,
        (rela_count * core::mem::size_of::<RelaEntry>()) as u64,
        0,
    ).map_err(|e| {
        let _ = debug_print(&format!("dynprobe: reloc error {:?}", e));
        libcluu::Error::InvalidArgument
    })?;

    for i in 0..rela_count {
        let target = (base + target_offsets[i]) as *const u64;
        let val = unsafe { core::ptr::read_volatile(target) };
        let expected = (base as i64 + addends[i]) as u64;
        if val != expected {
            let _ = debug_print(&format!("dynprobe: reloc[{}] = 0x{:x} expected 0x{:x}", i, val, expected));
            return Err(libcluu::Error::InvalidArgument);
        }
    }

    let _ = debug_print("dynprobe: RELATIVE relocs OK");
    Ok(())
}

fn test_dynamic_parsing() -> Result<()> {
    let _ = debug_print("dynprobe: testing Dynamic section parsing");

    let dyn_entries: [DynEntry; 6] = [
        DynEntry { tag: 5, val: 0x1000 },  // DT_STRTAB
        DynEntry { tag: 6, val: 0x2000 },  // DT_SYMTAB
        DynEntry { tag: 10, val: 0x100 },  // DT_STRSZ
        DynEntry { tag: 7, val: 0x3000 },  // DT_RELA
        DynEntry { tag: 8, val: 0x200 },   // DT_RELASZ
        DynEntry { tag: 0, val: 0 },       // DT_NULL
    ];

    let info = DynamicInfo::parse(unsafe {
        core::slice::from_raw_parts(dyn_entries.as_ptr() as *const cluu_ld_cluu::dynamic::DynEntry, 6)
    });

    if info.strtab != 0x1000 {
        let _ = debug_print(&format!("dynprobe: strtab=0x{:x} expected 0x1000", info.strtab));
        return Err(libcluu::Error::InvalidArgument);
    }
    if info.symtab != 0x2000 {
        let _ = debug_print(&format!("dynprobe: symtab=0x{:x} expected 0x2000", info.symtab));
        return Err(libcluu::Error::InvalidArgument);
    }
    if info.strsz != 0x100 {
        let _ = debug_print(&format!("dynprobe: strsz=0x{:x} expected 0x100", info.strsz));
        return Err(libcluu::Error::InvalidArgument);
    }
    if info.rela != 0x3000 {
        let _ = debug_print(&format!("dynprobe: rela=0x{:x} expected 0x3000", info.rela));
        return Err(libcluu::Error::InvalidArgument);
    }
    if info.relasz != 0x200 {
        let _ = debug_print(&format!("dynprobe: relasz=0x{:x} expected 0x200", info.relasz));
        return Err(libcluu::Error::InvalidArgument);
    }

    let _ = debug_print("dynprobe: Dynamic parsing OK");
    Ok(())
}

fn test_tls() -> Result<()> {
    let _ = debug_print("dynprobe: testing TLS __tls_get_addr");

    let mut block = TlsBlock::new();

    block.register_module(TlsModule {
        module_id: 1,
        tls_image: 0,
        tls_size: 256,
        tls_align: 16,
    });

    block.register_module(TlsModule {
        module_id: 2,
        tls_image: 0,
        tls_size: 128,
        tls_align: 8,
    });

    init_thread_tls(&mut block, 0x7000_0000);

    let addr1 = __tls_get_addr(&mut block, 1, 0);
    if addr1 != 0x7000_1000 {
        let _ = debug_print(&format!("dynprobe: tls addr1=0x{:x} expected 0x70001000", addr1));
        return Err(libcluu::Error::InvalidArgument);
    }

    let addr2 = __tls_get_addr(&mut block, 2, 0);
    if addr2 != 0x7000_2000 {
        let _ = debug_print(&format!("dynprobe: tls addr2=0x{:x} expected 0x70002000", addr2));
        return Err(libcluu::Error::InvalidArgument);
    }

    let addr1_offset = __tls_get_addr(&mut block, 1, 0x40);
    if addr1_offset != 0x7000_1040 {
        let _ = debug_print(&format!("dynprobe: tls offset=0x{:x} expected 0x70001040", addr1_offset));
        return Err(libcluu::Error::InvalidArgument);
    }

    let bad_addr = __tls_get_addr(&mut block, 99, 0);
    if bad_addr != 0 {
        let _ = debug_print(&format!("dynprobe: tls bad=0x{:x} expected 0", bad_addr));
        return Err(libcluu::Error::InvalidArgument);
    }

    let _ = debug_print("dynprobe: TLS __tls_get_addr OK");
    Ok(())
}
