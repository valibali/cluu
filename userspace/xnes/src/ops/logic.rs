use crate::bus::Bus;
use crate::cpu::{CpuRp2a03, FLAG_CARRY, FLAG_OVERFLOW, FLAG_ZERO};

// ---- AND ----

op_read!(
    and_imm,
    |cpu: &mut CpuRp2a03, op: u8| {
        let v = cpu.a() & op;
        cpu.set_a(v);
        cpu.set_sign(v);
        cpu.set_zero(v);
    },
    imm,
    2
);
op_read!(
    and_zp,
    |cpu: &mut CpuRp2a03, op: u8| {
        let v = cpu.a() & op;
        cpu.set_a(v);
        cpu.set_sign(v);
        cpu.set_zero(v);
    },
    zp,
    3
);
op_read!(
    and_zpx,
    |cpu: &mut CpuRp2a03, op: u8| {
        let v = cpu.a() & op;
        cpu.set_a(v);
        cpu.set_sign(v);
        cpu.set_zero(v);
    },
    zpx,
    4
);
op_read!(
    and_abs,
    |cpu: &mut CpuRp2a03, op: u8| {
        let v = cpu.a() & op;
        cpu.set_a(v);
        cpu.set_sign(v);
        cpu.set_zero(v);
    },
    abs,
    4
);
op_read!(
    and_absx,
    |cpu: &mut CpuRp2a03, op: u8| {
        let v = cpu.a() & op;
        cpu.set_a(v);
        cpu.set_sign(v);
        cpu.set_zero(v);
    },
    absx,
    4
);
op_read!(
    and_absy,
    |cpu: &mut CpuRp2a03, op: u8| {
        let v = cpu.a() & op;
        cpu.set_a(v);
        cpu.set_sign(v);
        cpu.set_zero(v);
    },
    absy,
    4
);
op_read!(
    and_indx,
    |cpu: &mut CpuRp2a03, op: u8| {
        let v = cpu.a() & op;
        cpu.set_a(v);
        cpu.set_sign(v);
        cpu.set_zero(v);
    },
    indx,
    6
);
op_read!(
    and_indy,
    |cpu: &mut CpuRp2a03, op: u8| {
        let v = cpu.a() & op;
        cpu.set_a(v);
        cpu.set_sign(v);
        cpu.set_zero(v);
    },
    indy,
    5
);

// ---- ORA ----

op_read!(
    ora_imm,
    |cpu: &mut CpuRp2a03, op: u8| {
        let v = cpu.a() | op;
        cpu.set_a(v);
        cpu.set_sign(v);
        cpu.set_zero(v);
    },
    imm,
    2
);
op_read!(
    ora_zp,
    |cpu: &mut CpuRp2a03, op: u8| {
        let v = cpu.a() | op;
        cpu.set_a(v);
        cpu.set_sign(v);
        cpu.set_zero(v);
    },
    zp,
    3
);
op_read!(
    ora_zpx,
    |cpu: &mut CpuRp2a03, op: u8| {
        let v = cpu.a() | op;
        cpu.set_a(v);
        cpu.set_sign(v);
        cpu.set_zero(v);
    },
    zpx,
    4
);
op_read!(
    ora_abs,
    |cpu: &mut CpuRp2a03, op: u8| {
        let v = cpu.a() | op;
        cpu.set_a(v);
        cpu.set_sign(v);
        cpu.set_zero(v);
    },
    abs,
    4
);
op_read!(
    ora_absx,
    |cpu: &mut CpuRp2a03, op: u8| {
        let v = cpu.a() | op;
        cpu.set_a(v);
        cpu.set_sign(v);
        cpu.set_zero(v);
    },
    absx,
    4
);
op_read!(
    ora_absy,
    |cpu: &mut CpuRp2a03, op: u8| {
        let v = cpu.a() | op;
        cpu.set_a(v);
        cpu.set_sign(v);
        cpu.set_zero(v);
    },
    absy,
    4
);
op_read!(
    ora_indx,
    |cpu: &mut CpuRp2a03, op: u8| {
        let v = cpu.a() | op;
        cpu.set_a(v);
        cpu.set_sign(v);
        cpu.set_zero(v);
    },
    indx,
    6
);
op_read!(
    ora_indy,
    |cpu: &mut CpuRp2a03, op: u8| {
        let v = cpu.a() | op;
        cpu.set_a(v);
        cpu.set_sign(v);
        cpu.set_zero(v);
    },
    indy,
    5
);

// ---- EOR ----

op_read!(
    eor_imm,
    |cpu: &mut CpuRp2a03, op: u8| {
        let v = cpu.a() ^ op;
        cpu.set_a(v);
        cpu.set_sign(v);
        cpu.set_zero(v);
    },
    imm,
    2
);
op_read!(
    eor_zp,
    |cpu: &mut CpuRp2a03, op: u8| {
        let v = cpu.a() ^ op;
        cpu.set_a(v);
        cpu.set_sign(v);
        cpu.set_zero(v);
    },
    zp,
    3
);
op_read!(
    eor_zpx,
    |cpu: &mut CpuRp2a03, op: u8| {
        let v = cpu.a() ^ op;
        cpu.set_a(v);
        cpu.set_sign(v);
        cpu.set_zero(v);
    },
    zpx,
    4
);
op_read!(
    eor_abs,
    |cpu: &mut CpuRp2a03, op: u8| {
        let v = cpu.a() ^ op;
        cpu.set_a(v);
        cpu.set_sign(v);
        cpu.set_zero(v);
    },
    abs,
    4
);
op_read!(
    eor_absx,
    |cpu: &mut CpuRp2a03, op: u8| {
        let v = cpu.a() ^ op;
        cpu.set_a(v);
        cpu.set_sign(v);
        cpu.set_zero(v);
    },
    absx,
    4
);
op_read!(
    eor_absy,
    |cpu: &mut CpuRp2a03, op: u8| {
        let v = cpu.a() ^ op;
        cpu.set_a(v);
        cpu.set_sign(v);
        cpu.set_zero(v);
    },
    absy,
    4
);
op_read!(
    eor_indx,
    |cpu: &mut CpuRp2a03, op: u8| {
        let v = cpu.a() ^ op;
        cpu.set_a(v);
        cpu.set_sign(v);
        cpu.set_zero(v);
    },
    indx,
    6
);
op_read!(
    eor_indy,
    |cpu: &mut CpuRp2a03, op: u8| {
        let v = cpu.a() ^ op;
        cpu.set_a(v);
        cpu.set_sign(v);
        cpu.set_zero(v);
    },
    indy,
    5
);

// ---- BIT ----

pub fn bit_zp(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let addr = crate::ops::addr_modes::zp(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(1));
    let val = bus.read(addr);
    cpu.set_sign(val);
    cpu.set_flag(FLAG_OVERFLOW, val & 0x40 != 0);
    cpu.set_flag(FLAG_ZERO, cpu.a() & val == 0);
    3
}

pub fn bit_abs(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let addr = crate::ops::addr_modes::abs(cpu, bus);
    cpu.set_pc(cpu.pc().wrapping_add(2));
    let val = bus.read(addr);
    cpu.set_sign(val);
    cpu.set_flag(FLAG_OVERFLOW, val & 0x40 != 0);
    cpu.set_flag(FLAG_ZERO, cpu.a() & val == 0);
    4
}

// ---- ANC (AND with A, then copy N flag to C) ----

pub fn anc_imm(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let val = bus.read(cpu.pc()) & cpu.a();
    cpu.set_pc(cpu.pc().wrapping_add(1));
    cpu.set_a(val);
    cpu.set_sign(val);
    cpu.set_zero(val);
    cpu.set_flag(FLAG_CARRY, val & 0x80 != 0);
    2
}

// ---- ANE (A = (A | constant) & X & operand) ----

pub fn ane_imm(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let operand = bus.read(cpu.pc());
    cpu.set_pc(cpu.pc().wrapping_add(1));
    let val = (cpu.a() | 0xEE) & cpu.x() & operand;
    cpu.set_a(val);
    cpu.set_sign(val);
    cpu.set_zero(val);
    2
}

// ---- LXA (A = (A | constant) & operand, X = A) ----

pub fn lxa_imm(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let operand = bus.read(cpu.pc());
    cpu.set_pc(cpu.pc().wrapping_add(1));
    let val = (cpu.a() | 0xEE) & operand;
    cpu.set_a(val);
    cpu.set_x(val);
    cpu.set_sign(val);
    cpu.set_zero(val);
    2
}
