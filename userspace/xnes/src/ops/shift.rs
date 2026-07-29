// Shift and rotate instructions.
// Generated via macros — inner operations are inline closures.

use crate::bus::Bus;
use crate::cpu::CpuRp2a03;

// ---- ASL ----

op_rmw_carry!(asl_a, |v: u8| (v << 1, v & 0x80 != 0), a, 2);
op_rmw_carry!(asl_zp, |v: u8| (v << 1, v & 0x80 != 0), zp, 5);
op_rmw_carry!(asl_zpx, |v: u8| (v << 1, v & 0x80 != 0), zpx, 6);
op_rmw_carry!(asl_abs, |v: u8| (v << 1, v & 0x80 != 0), abs, 6);
op_rmw_carry!(asl_absx, |v: u8| (v << 1, v & 0x80 != 0), absx, 7);

// ---- LSR ----

op_rmw_carry!(lsr_a, |v: u8| (v >> 1, v & 0x01 != 0), a, 2);
op_rmw_carry!(lsr_zp, |v: u8| (v >> 1, v & 0x01 != 0), zp, 5);
op_rmw_carry!(lsr_zpx, |v: u8| (v >> 1, v & 0x01 != 0), zpx, 6);
op_rmw_carry!(lsr_abs, |v: u8| (v >> 1, v & 0x01 != 0), abs, 6);
op_rmw_carry!(lsr_absx, |v: u8| (v >> 1, v & 0x01 != 0), absx, 7);

// ---- ROL ----

op_rmw_rotate!(
    rol_a,
    |v: u8, c: bool| ((v << 1) | c as u8, v & 0x80 != 0),
    a,
    2
);
op_rmw_rotate!(
    rol_zp,
    |v: u8, c: bool| ((v << 1) | c as u8, v & 0x80 != 0),
    zp,
    5
);
op_rmw_rotate!(
    rol_zpx,
    |v: u8, c: bool| ((v << 1) | c as u8, v & 0x80 != 0),
    zpx,
    6
);
op_rmw_rotate!(
    rol_abs,
    |v: u8, c: bool| ((v << 1) | c as u8, v & 0x80 != 0),
    abs,
    6
);
op_rmw_rotate!(
    rol_absx,
    |v: u8, c: bool| ((v << 1) | c as u8, v & 0x80 != 0),
    absx,
    7
);

// ---- ROR ----

op_rmw_rotate!(
    ror_a,
    |v: u8, c: bool| ((v >> 1) | ((c as u8) << 7), v & 0x01 != 0),
    a,
    2
);
op_rmw_rotate!(
    ror_zp,
    |v: u8, c: bool| ((v >> 1) | ((c as u8) << 7), v & 0x01 != 0),
    zp,
    5
);
op_rmw_rotate!(
    ror_zpx,
    |v: u8, c: bool| ((v >> 1) | ((c as u8) << 7), v & 0x01 != 0),
    zpx,
    6
);
op_rmw_rotate!(
    ror_abs,
    |v: u8, c: bool| ((v >> 1) | ((c as u8) << 7), v & 0x01 != 0),
    abs,
    6
);
op_rmw_rotate!(
    ror_absx,
    |v: u8, c: bool| ((v >> 1) | ((c as u8) << 7), v & 0x01 != 0),
    absx,
    7
);
