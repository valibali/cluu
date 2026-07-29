use crate::bus::Bus;
use crate::cpu::CpuRp2a03;
use crate::ops::addr_modes;

// ---- NOP variants ----
pub fn nop_imm(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    bus.read(cpu.pc());
    cpu.set_pc(cpu.pc().wrapping_add(1));
    2
}

pub fn nop_zp(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let addr = addr_modes::zp(cpu, bus);
    // Dummy read from the zero page address (cycle 3 of NOP zp)
    let _ = bus.read(addr);
    cpu.set_pc(cpu.pc().wrapping_add(1));
    3
}

pub fn nop_zpx(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let addr = addr_modes::zpx(cpu, bus);
    // Dummy read from the ZPX address (cycle 4 of NOP zpx)
    let _ = bus.read(addr);
    cpu.set_pc(cpu.pc().wrapping_add(1));
    4
}

pub fn nop_abs(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let addr = addr_modes::abs(cpu, bus);
    // Dummy read from the absolute address (cycle 4 of NOP abs)
    let _ = bus.read(addr);
    cpu.set_pc(cpu.pc().wrapping_add(2));
    4
}

pub fn nop_absx(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let (addr, page) = addr_modes::absx(cpu, bus);
    // Dummy read from the final address (cycle 4/5 of NOP absx)
    let _ = bus.read(addr);
    cpu.set_pc(cpu.pc().wrapping_add(2));
    4 + page
}

pub fn nop(_cpu: &mut CpuRp2a03, _bus: &mut Bus) -> u8 {
    2
}
