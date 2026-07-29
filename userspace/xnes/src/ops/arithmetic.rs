use crate::bus::Bus;
use crate::cpu::{CpuRp2a03, FLAG_CARRY, FLAG_OVERFLOW};

pub fn adc_inner(cpu: &mut CpuRp2a03, operand: u8) {
    let a = cpu.a();
    let carry = cpu.get_flag(FLAG_CARRY) as u16;
    let result = a as u16 + operand as u16 + carry;
    let lo = result as u8;
    // NES RP2A03 does NOT support decimal mode for ADC/SBC
    cpu.set_sign(lo);
    cpu.set_zero(lo);
    cpu.set_flag(FLAG_CARRY, result > 0xFF);
    cpu.set_flag(
        FLAG_OVERFLOW,
        ((a ^ operand) & 0x80) == 0 && ((a ^ lo) & 0x80) != 0,
    );
    cpu.set_a(lo);
}

op_read!(adc_imm, adc_inner, imm, 2);
op_read!(adc_zp, adc_inner, zp, 3);
op_read!(adc_zpx, adc_inner, zpx, 4);
op_read!(adc_abs, adc_inner, abs, 4);
op_read!(adc_absx, adc_inner, absx, 4);
op_read!(adc_absy, adc_inner, absy, 4);
op_read!(adc_indx, adc_inner, indx, 6);
op_read!(adc_indy, adc_inner, indy, 5);

pub fn sbc_inner(cpu: &mut CpuRp2a03, operand: u8) {
    let a = cpu.a();
    let carry = cpu.get_flag(FLAG_CARRY) as u16;
    let result = (a as u16)
        .wrapping_sub(operand as u16)
        .wrapping_sub(1 - carry);
    let lo = result as u8;
    // NES RP2A03 does NOT support decimal mode for ADC/SBC
    cpu.set_sign(lo);
    cpu.set_zero(lo);
    cpu.set_flag(FLAG_CARRY, result < 0x100);
    cpu.set_flag(
        FLAG_OVERFLOW,
        ((a ^ lo) & 0x80) != 0 && ((a ^ operand) & 0x80) != 0,
    );
    cpu.set_a(lo);
}

op_read!(sbc_imm, sbc_inner, imm, 2);
op_read!(sbc_zp, sbc_inner, zp, 3);
op_read!(sbc_zpx, sbc_inner, zpx, 4);
op_read!(sbc_abs, sbc_inner, abs, 4);
op_read!(sbc_absx, sbc_inner, absx, 4);
op_read!(sbc_absy, sbc_inner, absy, 4);
op_read!(sbc_indx, sbc_inner, indx, 6);
op_read!(sbc_indy, sbc_inner, indy, 5);

// ---- Compare (CMP, CPX, CPY) ----

pub fn cmp_inner(cpu: &mut CpuRp2a03, reg: u8, mem: u8) {
    let diff = reg.wrapping_sub(mem);
    cpu.set_flag(FLAG_CARRY, reg >= mem);
    cpu.set_sign(diff);
    cpu.set_zero(diff);
}

op_read!(
    cmp_imm,
    |cpu: &mut CpuRp2a03, op: u8| cmp_inner(cpu, cpu.a(), op),
    imm,
    2
);
op_read!(
    cmp_zp,
    |cpu: &mut CpuRp2a03, op: u8| cmp_inner(cpu, cpu.a(), op),
    zp,
    3
);
op_read!(
    cmp_zpx,
    |cpu: &mut CpuRp2a03, op: u8| cmp_inner(cpu, cpu.a(), op),
    zpx,
    4
);
op_read!(
    cmp_abs,
    |cpu: &mut CpuRp2a03, op: u8| cmp_inner(cpu, cpu.a(), op),
    abs,
    4
);
op_read!(
    cmp_absx,
    |cpu: &mut CpuRp2a03, op: u8| cmp_inner(cpu, cpu.a(), op),
    absx,
    4
);
op_read!(
    cmp_absy,
    |cpu: &mut CpuRp2a03, op: u8| cmp_inner(cpu, cpu.a(), op),
    absy,
    4
);
op_read!(
    cmp_indx,
    |cpu: &mut CpuRp2a03, op: u8| cmp_inner(cpu, cpu.a(), op),
    indx,
    6
);
op_read!(
    cmp_indy,
    |cpu: &mut CpuRp2a03, op: u8| cmp_inner(cpu, cpu.a(), op),
    indy,
    5
);

op_read!(
    cpx_imm,
    |cpu: &mut CpuRp2a03, op: u8| cmp_inner(cpu, cpu.x(), op),
    imm,
    2
);
op_read!(
    cpx_zp,
    |cpu: &mut CpuRp2a03, op: u8| cmp_inner(cpu, cpu.x(), op),
    zp,
    3
);
op_read!(
    cpx_abs,
    |cpu: &mut CpuRp2a03, op: u8| cmp_inner(cpu, cpu.x(), op),
    abs,
    4
);

op_read!(
    cpy_imm,
    |cpu: &mut CpuRp2a03, op: u8| cmp_inner(cpu, cpu.y(), op),
    imm,
    2
);
op_read!(
    cpy_zp,
    |cpu: &mut CpuRp2a03, op: u8| cmp_inner(cpu, cpu.y(), op),
    zp,
    3
);
op_read!(
    cpy_abs,
    |cpu: &mut CpuRp2a03, op: u8| cmp_inner(cpu, cpu.y(), op),
    abs,
    4
);
