use crate::bus::Bus;
use crate::cpu::CpuRp2a03;

// ---- LDA ----

op_read!(
    lda_imm,
    |cpu: &mut CpuRp2a03, op: u8| {
        cpu.set_a(op);
        cpu.set_sign(op);
        cpu.set_zero(op);
    },
    imm,
    2
);
op_read!(
    lda_zp,
    |cpu: &mut CpuRp2a03, op: u8| {
        cpu.set_a(op);
        cpu.set_sign(op);
        cpu.set_zero(op);
    },
    zp,
    3
);
op_read!(
    lda_zpx,
    |cpu: &mut CpuRp2a03, op: u8| {
        cpu.set_a(op);
        cpu.set_sign(op);
        cpu.set_zero(op);
    },
    zpx,
    4
);
op_read!(
    lda_abs,
    |cpu: &mut CpuRp2a03, op: u8| {
        cpu.set_a(op);
        cpu.set_sign(op);
        cpu.set_zero(op);
    },
    abs,
    4
);
op_read!(
    lda_absx,
    |cpu: &mut CpuRp2a03, op: u8| {
        cpu.set_a(op);
        cpu.set_sign(op);
        cpu.set_zero(op);
    },
    absx,
    4
);
op_read!(
    lda_absy,
    |cpu: &mut CpuRp2a03, op: u8| {
        cpu.set_a(op);
        cpu.set_sign(op);
        cpu.set_zero(op);
    },
    absy,
    4
);
op_read!(
    lda_indx,
    |cpu: &mut CpuRp2a03, op: u8| {
        cpu.set_a(op);
        cpu.set_sign(op);
        cpu.set_zero(op);
    },
    indx,
    6
);
op_read!(
    lda_indy,
    |cpu: &mut CpuRp2a03, op: u8| {
        cpu.set_a(op);
        cpu.set_sign(op);
        cpu.set_zero(op);
    },
    indy,
    5
);

// ---- STA ----

op_store!(sta_zp, a, zp, 3);
op_store!(sta_zpx, a, zpx, 4);
op_store!(sta_abs, a, abs, 4);
op_store!(sta_absx, a, absx, 5);
op_store!(sta_absy, a, absy, 5);
op_store!(sta_indx, a, indx, 6);
op_store!(sta_indy, a, indy, 6);

// ---- LDX ----

op_read!(
    ldx_imm,
    |cpu: &mut CpuRp2a03, op: u8| {
        cpu.set_x(op);
        cpu.set_sign(op);
        cpu.set_zero(op);
    },
    imm,
    2
);
op_read!(
    ldx_zp,
    |cpu: &mut CpuRp2a03, op: u8| {
        cpu.set_x(op);
        cpu.set_sign(op);
        cpu.set_zero(op);
    },
    zp,
    3
);
op_read!(
    ldx_zpy,
    |cpu: &mut CpuRp2a03, op: u8| {
        cpu.set_x(op);
        cpu.set_sign(op);
        cpu.set_zero(op);
    },
    zpy,
    4
);
op_read!(
    ldx_abs,
    |cpu: &mut CpuRp2a03, op: u8| {
        cpu.set_x(op);
        cpu.set_sign(op);
        cpu.set_zero(op);
    },
    abs,
    4
);
op_read!(
    ldx_absy,
    |cpu: &mut CpuRp2a03, op: u8| {
        cpu.set_x(op);
        cpu.set_sign(op);
        cpu.set_zero(op);
    },
    absy,
    4
);

// ---- LDY ----

op_read!(
    ldy_imm,
    |cpu: &mut CpuRp2a03, op: u8| {
        cpu.set_y(op);
        cpu.set_sign(op);
        cpu.set_zero(op);
    },
    imm,
    2
);
op_read!(
    ldy_zp,
    |cpu: &mut CpuRp2a03, op: u8| {
        cpu.set_y(op);
        cpu.set_sign(op);
        cpu.set_zero(op);
    },
    zp,
    3
);
op_read!(
    ldy_zpx,
    |cpu: &mut CpuRp2a03, op: u8| {
        cpu.set_y(op);
        cpu.set_sign(op);
        cpu.set_zero(op);
    },
    zpx,
    4
);
op_read!(
    ldy_abs,
    |cpu: &mut CpuRp2a03, op: u8| {
        cpu.set_y(op);
        cpu.set_sign(op);
        cpu.set_zero(op);
    },
    abs,
    4
);
op_read!(
    ldy_absx,
    |cpu: &mut CpuRp2a03, op: u8| {
        cpu.set_y(op);
        cpu.set_sign(op);
        cpu.set_zero(op);
    },
    absx,
    4
);

// ---- STX ----

op_store!(stx_zp, x, zp, 3);
op_store!(stx_zpy, x, zpy, 4);
op_store!(stx_abs, x, abs, 4);

// ---- STY ----

op_store!(sty_zp, y, zp, 3);
op_store!(sty_zpx, y, zpx, 4);
op_store!(sty_abs, y, abs, 4);

// ---- Register Transfers ----

pub fn tax(cpu: &mut CpuRp2a03, _bus: &mut Bus) -> u8 {
    let val = cpu.a();
    cpu.set_x(val);
    cpu.set_sign(val);
    cpu.set_zero(val);
    2
}

pub fn txa(cpu: &mut CpuRp2a03, _bus: &mut Bus) -> u8 {
    let val = cpu.x();
    cpu.set_a(val);
    cpu.set_sign(val);
    cpu.set_zero(val);
    2
}

pub fn tay(cpu: &mut CpuRp2a03, _bus: &mut Bus) -> u8 {
    let val = cpu.a();
    cpu.set_y(val);
    cpu.set_sign(val);
    cpu.set_zero(val);
    2
}

pub fn tya(cpu: &mut CpuRp2a03, _bus: &mut Bus) -> u8 {
    let val = cpu.y();
    cpu.set_a(val);
    cpu.set_sign(val);
    cpu.set_zero(val);
    2
}

pub fn tsx(cpu: &mut CpuRp2a03, _bus: &mut Bus) -> u8 {
    let val = cpu.st();
    cpu.set_x(val);
    cpu.set_sign(val);
    cpu.set_zero(val);
    2
}

pub fn txs(cpu: &mut CpuRp2a03, _bus: &mut Bus) -> u8 {
    cpu.set_st(cpu.x());
    2
}
