// Read-modify-write (INC, DEC) and simple register ops (DEX, DEY, INX, INY).
// INC/DEC variants are generated via macros.

use crate::bus::Bus;
use crate::cpu::CpuRp2a03;

// ---- INC ----

op_rmw!(inc_zp, |v: u8| v.wrapping_add(1), zp, 5);
op_rmw!(inc_zpx, |v: u8| v.wrapping_add(1), zpx, 6);
op_rmw!(inc_abs, |v: u8| v.wrapping_add(1), abs, 6);
op_rmw!(inc_absx, |v: u8| v.wrapping_add(1), absx, 7);

// ---- DEC ----

op_rmw!(dec_zp, |v: u8| v.wrapping_sub(1), zp, 5);
op_rmw!(dec_zpx, |v: u8| v.wrapping_sub(1), zpx, 6);
op_rmw!(dec_abs, |v: u8| v.wrapping_sub(1), abs, 6);
op_rmw!(dec_absx, |v: u8| v.wrapping_sub(1), absx, 7);

// ---- DEX ----

pub fn dex(cpu: &mut CpuRp2a03, _bus: &mut Bus) -> u8 {
    let result = cpu.x().wrapping_sub(1);
    cpu.set_x(result);
    cpu.set_sign(result);
    cpu.set_zero(result);
    2
}

// ---- DEY ----

pub fn dey(cpu: &mut CpuRp2a03, _bus: &mut Bus) -> u8 {
    let result = cpu.y().wrapping_sub(1);
    cpu.set_y(result);
    cpu.set_sign(result);
    cpu.set_zero(result);
    2
}

// ---- INX ----

pub fn inx(cpu: &mut CpuRp2a03, _bus: &mut Bus) -> u8 {
    let result = cpu.x().wrapping_add(1);
    cpu.set_x(result);
    cpu.set_sign(result);
    cpu.set_zero(result);
    2
}

// ---- INY ----

pub fn iny(cpu: &mut CpuRp2a03, _bus: &mut Bus) -> u8 {
    let result = cpu.y().wrapping_add(1);
    cpu.set_y(result);
    cpu.set_sign(result);
    cpu.set_zero(result);
    2
}
