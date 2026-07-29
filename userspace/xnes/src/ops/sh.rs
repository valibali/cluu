use crate::bus::Bus;
use crate::cpu::CpuRp2a03;
use crate::ops::addr_modes;

/// Determine H value for SH instructions.
/// H is derived from the data bus after reading the operand's high byte.
/// On DMC DMA pending, H is forced to 0xFF (ignore H).
fn sh_h(hi: u8, bus: &mut Bus) -> u8 {
    let ignore = core::mem::replace(&mut bus.dmc_just_fired, false) || bus.apu.dmc_dma_pending();
    if ignore { 0xFF } else { hi.wrapping_add(1) }
}

/// Compute page-cross write address for SH instructions.
/// On page cross, the address high byte equals the stored value itself.
fn sh_page_addr(addr: u16, page: u8, val: u8) -> u16 {
    if page != 0 {
        (addr as u8 as u16) | ((val as u16) << 8)
    } else {
        addr
    }
}

// ---- SHA (Store A & X & H at address) ----
pub fn sha_indy(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let ptr = bus.read(cpu.pc()) as u16;
    bus.apu.tick_dmc();
    bus.dmc_ticks += 1;
    // Zero-page wrap: when ptr = $FF, high byte is read from $00, not $100
    let hi_ptr = (ptr as u8).wrapping_add(1) as u16;
    let base = bus.read(ptr) as u16;
    bus.apu.tick_dmc();
    bus.dmc_ticks += 1;
    let hi = bus.read(hi_ptr) as u16;
    bus.apu.tick_dmc();
    bus.dmc_ticks += 1;
    let base = base | (hi << 8);
    cpu.set_pc(cpu.pc().wrapping_add(1));
    let addr = base.wrapping_add(cpu.y() as u16);
    let page = (((base ^ addr) >> 8) as u8) & 1;
    let h = sh_h((base >> 8) as u8, bus);
    let val = cpu.a() & cpu.x() & h;
    let _ = bus.read(addr);
    bus.apu.tick_dmc();
    bus.dmc_ticks += 1;
    let final_addr = sh_page_addr(addr, page, val);
    bus.write(final_addr, val);
    bus.apu.tick_dmc();
    bus.dmc_ticks += 1;
    6
}

pub fn sha_absy(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let lo = bus.read(cpu.pc()) as u16;
    bus.apu.tick_dmc();
    bus.dmc_ticks += 1;
    let hi = bus.read(cpu.pc().wrapping_add(1)) as u16;
    bus.apu.tick_dmc();
    bus.dmc_ticks += 1;
    let base = lo | (hi << 8);
    cpu.set_pc(cpu.pc().wrapping_add(2));
    let addr = base.wrapping_add(cpu.y() as u16);
    let page = (((base ^ addr) >> 8) as u8) & 1;
    let h = sh_h(hi as u8, bus);
    let val = cpu.a() & cpu.x() & h;
    let _ = bus.read(addr);
    bus.apu.tick_dmc();
    bus.dmc_ticks += 1;
    let final_addr = sh_page_addr(addr, page, val);
    bus.write(final_addr, val);
    bus.apu.tick_dmc();
    bus.dmc_ticks += 1;
    5
}

// ---- SHS (Store A & X & H at address, also set SP = A & X) ----
pub fn shs_absy(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let lo = bus.read(cpu.pc()) as u16;
    bus.apu.tick_dmc();
    bus.dmc_ticks += 1;
    let hi = bus.read(cpu.pc().wrapping_add(1)) as u16;
    bus.apu.tick_dmc();
    bus.dmc_ticks += 1;
    let base = lo | (hi << 8);
    cpu.set_pc(cpu.pc().wrapping_add(2));
    let addr = base.wrapping_add(cpu.y() as u16);
    let page = (((base ^ addr) >> 8) as u8) & 1;
    let sp_val = cpu.a() & cpu.x();
    cpu.set_st(sp_val);
    let h = sh_h(hi as u8, bus);
    let val = sp_val & h;
    let _ = bus.read(addr);
    bus.apu.tick_dmc();
    bus.dmc_ticks += 1;
    let final_addr = sh_page_addr(addr, page, val);
    bus.write(final_addr, val);
    bus.apu.tick_dmc();
    bus.dmc_ticks += 1;
    5
}

// ---- SHY (Store Y & H at address) ----
pub fn shy_absx(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let lo = bus.read(cpu.pc()) as u16;
    bus.apu.tick_dmc();
    bus.dmc_ticks += 1;
    let hi = bus.read(cpu.pc().wrapping_add(1)) as u16;
    bus.apu.tick_dmc();
    bus.dmc_ticks += 1;
    let base = lo | (hi << 8);
    cpu.set_pc(cpu.pc().wrapping_add(2));
    let addr = base.wrapping_add(cpu.x() as u16);
    let page = (((base ^ addr) >> 8) as u8) & 1;
    let h = sh_h(hi as u8, bus);
    let val = cpu.y() & h;
    let _ = bus.read(addr);
    bus.apu.tick_dmc();
    bus.dmc_ticks += 1;
    let final_addr = sh_page_addr(addr, page, val);
    bus.write(final_addr, val);
    bus.apu.tick_dmc();
    bus.dmc_ticks += 1;
    5
}

// ---- SHX (Store X & H at address) ----
pub fn shx_absy(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let lo = bus.read(cpu.pc()) as u16;
    bus.apu.tick_dmc();
    bus.dmc_ticks += 1;
    let hi = bus.read(cpu.pc().wrapping_add(1)) as u16;
    bus.apu.tick_dmc();
    bus.dmc_ticks += 1;
    let base = lo | (hi << 8);
    cpu.set_pc(cpu.pc().wrapping_add(2));
    let addr = base.wrapping_add(cpu.y() as u16);
    let page = (((base ^ addr) >> 8) as u8) & 1;
    let h = sh_h(hi as u8, bus);
    let val = cpu.x() & h;
    let _ = bus.read(addr);
    bus.apu.tick_dmc();
    bus.dmc_ticks += 1;
    let final_addr = sh_page_addr(addr, page, val);
    bus.write(final_addr, val);
    bus.apu.tick_dmc();
    bus.dmc_ticks += 1;
    5
}

// ---- LAE (Load A with SP & operand, X = A, SP = A) ----
pub fn lae_absy(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let (addr, page) = addr_modes::absy(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(2));
    let val = cpu.st() & bus.read(addr);
    cpu.set_a(val);
    cpu.set_x(val);
    cpu.set_st(val);
    cpu.set_sign(val);
    cpu.set_zero(val);
    4 + page
}
