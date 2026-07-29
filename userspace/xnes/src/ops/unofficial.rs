use crate::bus::Bus;
use crate::cpu::{CpuRp2a03, FLAG_CARRY, FLAG_OVERFLOW};
use crate::ops::addr_modes;
use crate::ops::arithmetic::{adc_inner, cmp_inner, sbc_inner};

// ---- SLO (ASL memory then ORA with A) ----
pub fn slo_indx(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let addr = addr_modes::indx(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(1));
    let val = bus.read(addr);
    bus.write(addr, val);
    cpu.set_flag(FLAG_CARRY, val & 0x80 != 0);
    let shifted = val << 1;
    bus.write(addr, shifted);
    let result = shifted | cpu.a();
    cpu.set_a(result);
    cpu.set_sign(result);
    cpu.set_zero(result);
    8
}

pub fn slo_zp(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let addr = addr_modes::zp(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(1));
    let val = bus.read(addr);
    bus.write(addr, val);
    cpu.set_flag(FLAG_CARRY, val & 0x80 != 0);
    let shifted = val << 1;
    bus.write(addr, shifted);
    let result = shifted | cpu.a();
    cpu.set_a(result);
    cpu.set_sign(result);
    cpu.set_zero(result);
    5
}

pub fn slo_abs(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let addr = addr_modes::abs(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(2));
    let val = bus.read(addr);
    bus.write(addr, val);
    cpu.set_flag(FLAG_CARRY, val & 0x80 != 0);
    let shifted = val << 1;
    bus.write(addr, shifted);
    let result = shifted | cpu.a();
    cpu.set_a(result);
    cpu.set_sign(result);
    cpu.set_zero(result);
    6
}

pub fn slo_indy(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let (addr, page) = addr_modes::indy(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(1));
    let val = bus.read(addr);
    bus.write(addr, val);
    cpu.set_flag(FLAG_CARRY, val & 0x80 != 0);
    let shifted = val << 1;
    bus.write(addr, shifted);
    let result = shifted | cpu.a();
    cpu.set_a(result);
    cpu.set_sign(result);
    cpu.set_zero(result);
    8 + page
}

pub fn slo_zpx(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let addr = addr_modes::zpx(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(1));
    let val = bus.read(addr);
    bus.write(addr, val);
    cpu.set_flag(FLAG_CARRY, val & 0x80 != 0);
    let shifted = val << 1;
    bus.write(addr, shifted);
    let result = shifted | cpu.a();
    cpu.set_a(result);
    cpu.set_sign(result);
    cpu.set_zero(result);
    6
}

pub fn slo_absy(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let (addr, _) = addr_modes::absy(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(2));
    let val = bus.read(addr);
    bus.write(addr, val);
    cpu.set_flag(FLAG_CARRY, val & 0x80 != 0);
    let shifted = val << 1;
    bus.write(addr, shifted);
    let result = shifted | cpu.a();
    cpu.set_a(result);
    cpu.set_sign(result);
    cpu.set_zero(result);
    7
}

pub fn slo_absx(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let (addr, _) = addr_modes::absx(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(2));
    let val = bus.read(addr);
    bus.write(addr, val);
    cpu.set_flag(FLAG_CARRY, val & 0x80 != 0);
    let shifted = val << 1;
    bus.write(addr, shifted);
    let result = shifted | cpu.a();
    cpu.set_a(result);
    cpu.set_sign(result);
    cpu.set_zero(result);
    7
}

// ---- RLA (ROL memory then AND with A) ----
pub fn rla_indx(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let addr = addr_modes::indx(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(1));
    let val = bus.read(addr);
    bus.write(addr, val);
    let carry = cpu.get_flag(FLAG_CARRY) as u8;
    cpu.set_flag(FLAG_CARRY, val & 0x80 != 0);
    let shifted = (val << 1) | carry;
    bus.write(addr, shifted);
    let result = shifted & cpu.a();
    cpu.set_a(result);
    cpu.set_sign(result);
    cpu.set_zero(result);
    8
}

pub fn rla_zp(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let addr = addr_modes::zp(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(1));
    let val = bus.read(addr);
    bus.write(addr, val);
    let carry = cpu.get_flag(FLAG_CARRY) as u8;
    cpu.set_flag(FLAG_CARRY, val & 0x80 != 0);
    let shifted = (val << 1) | carry;
    bus.write(addr, shifted);
    let result = shifted & cpu.a();
    cpu.set_a(result);
    cpu.set_sign(result);
    cpu.set_zero(result);
    5
}

pub fn rla_abs(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let addr = addr_modes::abs(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(2));
    let val = bus.read(addr);
    bus.write(addr, val);
    let carry = cpu.get_flag(FLAG_CARRY) as u8;
    cpu.set_flag(FLAG_CARRY, val & 0x80 != 0);
    let shifted = (val << 1) | carry;
    bus.write(addr, shifted);
    let result = shifted & cpu.a();
    cpu.set_a(result);
    cpu.set_sign(result);
    cpu.set_zero(result);
    6
}

pub fn rla_indy(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let (addr, page) = addr_modes::indy(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(1));
    let val = bus.read(addr);
    bus.write(addr, val);
    let carry = cpu.get_flag(FLAG_CARRY) as u8;
    cpu.set_flag(FLAG_CARRY, val & 0x80 != 0);
    let shifted = (val << 1) | carry;
    bus.write(addr, shifted);
    let result = shifted & cpu.a();
    cpu.set_a(result);
    cpu.set_sign(result);
    cpu.set_zero(result);
    8 + page
}

pub fn rla_zpx(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let addr = addr_modes::zpx(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(1));
    let val = bus.read(addr);
    bus.write(addr, val);
    let carry = cpu.get_flag(FLAG_CARRY) as u8;
    cpu.set_flag(FLAG_CARRY, val & 0x80 != 0);
    let shifted = (val << 1) | carry;
    bus.write(addr, shifted);
    let result = shifted & cpu.a();
    cpu.set_a(result);
    cpu.set_sign(result);
    cpu.set_zero(result);
    6
}

pub fn rla_absy(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let (addr, _) = addr_modes::absy(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(2));
    let val = bus.read(addr);
    bus.write(addr, val);
    let carry = cpu.get_flag(FLAG_CARRY) as u8;
    cpu.set_flag(FLAG_CARRY, val & 0x80 != 0);
    let shifted = (val << 1) | carry;
    bus.write(addr, shifted);
    let result = shifted & cpu.a();
    cpu.set_a(result);
    cpu.set_sign(result);
    cpu.set_zero(result);
    7
}

pub fn rla_absx(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let (addr, _) = addr_modes::absx(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(2));
    let val = bus.read(addr);
    bus.write(addr, val);
    let carry = cpu.get_flag(FLAG_CARRY) as u8;
    cpu.set_flag(FLAG_CARRY, val & 0x80 != 0);
    let shifted = (val << 1) | carry;
    bus.write(addr, shifted);
    let result = shifted & cpu.a();
    cpu.set_a(result);
    cpu.set_sign(result);
    cpu.set_zero(result);
    7
}

// ---- SRE (LSR memory then EOR with A) ----
pub fn sre_indx(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let addr = addr_modes::indx(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(1));
    let val = bus.read(addr);
    bus.write(addr, val);
    cpu.set_flag(FLAG_CARRY, val & 0x01 != 0);
    let shifted = val >> 1;
    bus.write(addr, shifted);
    let result = shifted ^ cpu.a();
    cpu.set_a(result);
    cpu.set_sign(result);
    cpu.set_zero(result);
    8
}

pub fn sre_zp(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let addr = addr_modes::zp(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(1));
    let val = bus.read(addr);
    bus.write(addr, val);
    cpu.set_flag(FLAG_CARRY, val & 0x01 != 0);
    let shifted = val >> 1;
    bus.write(addr, shifted);
    let result = shifted ^ cpu.a();
    cpu.set_a(result);
    cpu.set_sign(result);
    cpu.set_zero(result);
    5
}

pub fn sre_abs(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let addr = addr_modes::abs(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(2));
    let val = bus.read(addr);
    bus.write(addr, val);
    cpu.set_flag(FLAG_CARRY, val & 0x01 != 0);
    let shifted = val >> 1;
    bus.write(addr, shifted);
    let result = shifted ^ cpu.a();
    cpu.set_a(result);
    cpu.set_sign(result);
    cpu.set_zero(result);
    6
}

pub fn sre_indy(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let (addr, page) = addr_modes::indy(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(1));
    let val = bus.read(addr);
    bus.write(addr, val);
    cpu.set_flag(FLAG_CARRY, val & 0x01 != 0);
    let shifted = val >> 1;
    bus.write(addr, shifted);
    let result = shifted ^ cpu.a();
    cpu.set_a(result);
    cpu.set_sign(result);
    cpu.set_zero(result);
    8 + page
}

pub fn sre_zpx(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let addr = addr_modes::zpx(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(1));
    let val = bus.read(addr);
    bus.write(addr, val);
    cpu.set_flag(FLAG_CARRY, val & 0x01 != 0);
    let shifted = val >> 1;
    bus.write(addr, shifted);
    let result = shifted ^ cpu.a();
    cpu.set_a(result);
    cpu.set_sign(result);
    cpu.set_zero(result);
    6
}

pub fn sre_absy(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let (addr, _) = addr_modes::absy(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(2));
    let val = bus.read(addr);
    bus.write(addr, val);
    cpu.set_flag(FLAG_CARRY, val & 0x01 != 0);
    let shifted = val >> 1;
    bus.write(addr, shifted);
    let result = shifted ^ cpu.a();
    cpu.set_a(result);
    cpu.set_sign(result);
    cpu.set_zero(result);
    7
}

pub fn sre_absx(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let (addr, _) = addr_modes::absx(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(2));
    let val = bus.read(addr);
    bus.write(addr, val);
    cpu.set_flag(FLAG_CARRY, val & 0x01 != 0);
    let shifted = val >> 1;
    bus.write(addr, shifted);
    let result = shifted ^ cpu.a();
    cpu.set_a(result);
    cpu.set_sign(result);
    cpu.set_zero(result);
    7
}

// ---- RRA (ROR memory then ADC with A) ----
pub fn rra_indx(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let addr = addr_modes::indx(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(1));
    let val = bus.read(addr);
    bus.write(addr, val);
    let carry = cpu.get_flag(FLAG_CARRY) as u8;
    cpu.set_flag(FLAG_CARRY, val & 0x01 != 0);
    let rotated = (val >> 1) | (carry << 7);
    bus.write(addr, rotated);
    adc_inner(cpu, rotated);
    8
}

pub fn rra_zp(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let addr = addr_modes::zp(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(1));
    let val = bus.read(addr);
    bus.write(addr, val);
    let carry = cpu.get_flag(FLAG_CARRY) as u8;
    cpu.set_flag(FLAG_CARRY, val & 0x01 != 0);
    let rotated = (val >> 1) | (carry << 7);
    bus.write(addr, rotated);
    adc_inner(cpu, rotated);
    5
}

pub fn rra_abs(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let addr = addr_modes::abs(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(2));
    let val = bus.read(addr);
    bus.write(addr, val);
    let carry = cpu.get_flag(FLAG_CARRY) as u8;
    cpu.set_flag(FLAG_CARRY, val & 0x01 != 0);
    let rotated = (val >> 1) | (carry << 7);
    bus.write(addr, rotated);
    adc_inner(cpu, rotated);
    6
}

pub fn rra_indy(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let (addr, page) = addr_modes::indy(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(1));
    let val = bus.read(addr);
    bus.write(addr, val);
    let carry = cpu.get_flag(FLAG_CARRY) as u8;
    cpu.set_flag(FLAG_CARRY, val & 0x01 != 0);
    let rotated = (val >> 1) | (carry << 7);
    bus.write(addr, rotated);
    adc_inner(cpu, rotated);
    8 + page
}

pub fn rra_zpx(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let addr = addr_modes::zpx(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(1));
    let val = bus.read(addr);
    bus.write(addr, val);
    let carry = cpu.get_flag(FLAG_CARRY) as u8;
    cpu.set_flag(FLAG_CARRY, val & 0x01 != 0);
    let rotated = (val >> 1) | (carry << 7);
    bus.write(addr, rotated);
    adc_inner(cpu, rotated);
    6
}

pub fn rra_absy(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let (addr, _) = addr_modes::absy(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(2));
    let val = bus.read(addr);
    bus.write(addr, val);
    let carry = cpu.get_flag(FLAG_CARRY) as u8;
    cpu.set_flag(FLAG_CARRY, val & 0x01 != 0);
    let rotated = (val >> 1) | (carry << 7);
    bus.write(addr, rotated);
    adc_inner(cpu, rotated);
    7
}

pub fn rra_absx(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let (addr, _) = addr_modes::absx(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(2));
    let val = bus.read(addr);
    bus.write(addr, val);
    let carry = cpu.get_flag(FLAG_CARRY) as u8;
    cpu.set_flag(FLAG_CARRY, val & 0x01 != 0);
    let rotated = (val >> 1) | (carry << 7);
    bus.write(addr, rotated);
    adc_inner(cpu, rotated);
    7
}

// ---- SAX (Store A & X at address) ----
pub fn sax_indx(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let addr = addr_modes::indx(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(1));
    bus.write(addr, cpu.a() & cpu.x());
    6
}

pub fn sax_zp(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let addr = addr_modes::zp(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(1));
    bus.write(addr, cpu.a() & cpu.x());
    3
}

pub fn sax_abs(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let addr = addr_modes::abs(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(2));
    bus.write(addr, cpu.a() & cpu.x());
    4
}

pub fn sax_zpy(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let addr = addr_modes::zpy(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(1));
    bus.write(addr, cpu.a() & cpu.x());
    4
}

// ---- LAX (Load A and X from address) ----
pub fn lax_indx(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let addr = addr_modes::indx(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(1));
    let val = bus.read(addr);
    cpu.set_a(val);
    cpu.set_x(val);
    cpu.set_sign(val);
    cpu.set_zero(val);
    6
}

pub fn lax_zp(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let addr = addr_modes::zp(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(1));
    let val = bus.read(addr);
    cpu.set_a(val);
    cpu.set_x(val);
    cpu.set_sign(val);
    cpu.set_zero(val);
    3
}

pub fn lax_abs(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let addr = addr_modes::abs(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(2));
    let val = bus.read(addr);
    cpu.set_a(val);
    cpu.set_x(val);
    cpu.set_sign(val);
    cpu.set_zero(val);
    4
}

pub fn lax_indy(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let (addr, page) = addr_modes::indy(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(1));
    let val = bus.read(addr);
    cpu.set_a(val);
    cpu.set_x(val);
    cpu.set_sign(val);
    cpu.set_zero(val);
    5 + page
}

pub fn lax_zpy(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let addr = addr_modes::zpy(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(1));
    let val = bus.read(addr);
    cpu.set_a(val);
    cpu.set_x(val);
    cpu.set_sign(val);
    cpu.set_zero(val);
    4
}

pub fn lax_absy(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let (addr, page) = addr_modes::absy(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(2));
    let val = bus.read(addr);
    cpu.set_a(val);
    cpu.set_x(val);
    cpu.set_sign(val);
    cpu.set_zero(val);
    4 + page
}

// ---- DCP (DEC memory then CMP with A) ----
pub fn dcp_indx(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let addr = addr_modes::indx(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(1));
    let val = bus.read(addr);
    bus.write(addr, val);
    let dec = val.wrapping_sub(1);
    bus.write(addr, dec);
    cmp_inner(cpu, cpu.a(), dec);
    8
}

pub fn dcp_zp(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let addr = addr_modes::zp(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(1));
    let val = bus.read(addr);
    bus.write(addr, val);
    let dec = val.wrapping_sub(1);
    bus.write(addr, dec);
    cmp_inner(cpu, cpu.a(), dec);
    5
}

pub fn dcp_abs(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let addr = addr_modes::abs(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(2));
    let val = bus.read(addr);
    bus.write(addr, val);
    let dec = val.wrapping_sub(1);
    bus.write(addr, dec);
    cmp_inner(cpu, cpu.a(), dec);
    6
}

pub fn dcp_indy(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let (addr, page) = addr_modes::indy(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(1));
    let val = bus.read(addr);
    bus.write(addr, val);
    let dec = val.wrapping_sub(1);
    bus.write(addr, dec);
    cmp_inner(cpu, cpu.a(), dec);
    8 + page
}

pub fn dcp_zpx(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let addr = addr_modes::zpx(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(1));
    let val = bus.read(addr);
    bus.write(addr, val);
    let dec = val.wrapping_sub(1);
    bus.write(addr, dec);
    cmp_inner(cpu, cpu.a(), dec);
    6
}

pub fn dcp_absy(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let (addr, _) = addr_modes::absy(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(2));
    let val = bus.read(addr);
    bus.write(addr, val);
    let dec = val.wrapping_sub(1);
    bus.write(addr, dec);
    cmp_inner(cpu, cpu.a(), dec);
    7
}

pub fn dcp_absx(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let (addr, _) = addr_modes::absx(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(2));
    let val = bus.read(addr);
    bus.write(addr, val);
    let dec = val.wrapping_sub(1);
    bus.write(addr, dec);
    cmp_inner(cpu, cpu.a(), dec);
    7
}

// ---- ISC (INC memory then SBC with A) ----
pub fn isc_indx(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let addr = addr_modes::indx(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(1));
    let val = bus.read(addr);
    bus.write(addr, val);
    let inc = val.wrapping_add(1);
    bus.write(addr, inc);
    sbc_inner(cpu, inc);
    8
}

pub fn isc_zp(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let addr = addr_modes::zp(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(1));
    let val = bus.read(addr);
    bus.write(addr, val);
    let inc = val.wrapping_add(1);
    bus.write(addr, inc);
    sbc_inner(cpu, inc);
    5
}

pub fn isc_abs(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let addr = addr_modes::abs(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(2));
    let val = bus.read(addr);
    bus.write(addr, val);
    let inc = val.wrapping_add(1);
    bus.write(addr, inc);
    sbc_inner(cpu, inc);
    6
}

pub fn isc_indy(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let (addr, page) = addr_modes::indy(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(1));
    let val = bus.read(addr);
    bus.write(addr, val);
    let inc = val.wrapping_add(1);
    bus.write(addr, inc);
    sbc_inner(cpu, inc);
    8 + page
}

pub fn isc_zpx(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let addr = addr_modes::zpx(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(1));
    let val = bus.read(addr);
    bus.write(addr, val);
    let inc = val.wrapping_add(1);
    bus.write(addr, inc);
    sbc_inner(cpu, inc);
    6
}

pub fn isc_absy(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let (addr, _) = addr_modes::absy(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(2));
    let val = bus.read(addr);
    bus.write(addr, val);
    let inc = val.wrapping_add(1);
    bus.write(addr, inc);
    sbc_inner(cpu, inc);
    7
}

pub fn isc_absx(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let (addr, _) = addr_modes::absx(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(2));
    let val = bus.read(addr);
    bus.write(addr, val);
    let inc = val.wrapping_add(1);
    bus.write(addr, inc);
    sbc_inner(cpu, inc);
    7
}

// ---- ASR (AND with A, then LSR A) ----
pub fn asr_imm(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let val = bus.read(cpu.pc()) & cpu.a();
    cpu.set_pc(cpu.pc().wrapping_add(1));
    cpu.set_flag(FLAG_CARRY, val & 0x01 != 0);
    let result = val >> 1;
    cpu.set_a(result);
    cpu.set_sign(result);
    cpu.set_zero(result);
    2
}

// ---- ARR (AND with A, then ROR A with unusual flags) ----
pub fn arr_imm(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let val = bus.read(cpu.pc()) & cpu.a();
    cpu.set_pc(cpu.pc().wrapping_add(1));
    let old_carry = cpu.get_flag(FLAG_CARRY) as u8;
    let result = (val >> 1) | (old_carry << 7);
    cpu.set_a(result);
    cpu.set_sign(result);
    cpu.set_zero(result);
    cpu.set_flag(FLAG_CARRY, val & 0x80 != 0);
    cpu.set_flag(FLAG_OVERFLOW, (((val >> 5) ^ (val >> 6)) & 1) != 0);
    2
}

// ---- AXS (X = (A & X) - operand) ----
pub fn axs_imm(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let operand = bus.read(cpu.pc());
    cpu.set_pc(cpu.pc().wrapping_add(1));
    let val = cpu.a() & cpu.x();
    let result = val.wrapping_sub(operand);
    cpu.set_x(result);
    cpu.set_sign(result);
    cpu.set_zero(result);
    cpu.set_flag(FLAG_CARRY, val >= operand);
    2
}
