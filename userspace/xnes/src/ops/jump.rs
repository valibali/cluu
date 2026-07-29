use crate::bus::Bus;
use crate::cpu::{CpuRp2a03, FLAG_BREAK, FLAG_INTERRUPT};
use crate::ops::{addr_modes, pull, push};

pub fn jmp_abs(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    cpu.set_pc(addr_modes::abs(cpu, bus));
    3
}

pub fn jmp_ind(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let ptr = addr_modes::abs(cpu, bus);
    let lo = bus.read(ptr) as u16;
    // 6502 bug: when ptr ends in $FF, high byte is read from same page
    let hi_ptr = if ptr & 0xFF == 0xFF {
        ptr & 0xFF00
    } else {
        ptr.wrapping_add(1)
    };
    let hi = bus.read(hi_ptr) as u16;
    cpu.set_pc(lo | (hi << 8));
    5
}

pub fn jsr(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let target = addr_modes::abs(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(2));
    let return_addr = cpu.pc().wrapping_sub(1);
    push(cpu, bus, (return_addr >> 8) as u8);
    push(cpu, bus, return_addr as u8);
    cpu.set_pc(target);
    6
}

pub fn rts(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let lo = pull(cpu, bus) as u16;
    let hi = pull(cpu, bus) as u16;
    cpu.set_pc((lo | (hi << 8)).wrapping_add(1));
    6
}

pub fn brk(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    cpu.set_pc(cpu.pc().wrapping_add(1));
    push(cpu, bus, (cpu.pc() >> 8) as u8);
    push(cpu, bus, cpu.pc() as u8);
    // BRK pushes SR with B flag SET and bit 5 always SET
    let sr = (cpu.sr() | FLAG_BREAK) | 0x20;
    push(cpu, bus, sr);
    cpu.set_flag(FLAG_INTERRUPT, true);
    cpu.set_pc(u16::from_le_bytes([bus.read(0xFFFE), bus.read(0xFFFF)]));
    7
}

pub fn rti(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let sr = (pull(cpu, bus) & !FLAG_BREAK) | 0x20;
    cpu.set_sr(sr);
    let lo = pull(cpu, bus) as u16;
    let hi = pull(cpu, bus) as u16;
    cpu.set_pc(lo | (hi << 8));
    6
}
