// ---------------------------------------------------------------------------
// Shared macros for generating opcode addressing-mode variants.
// Uses `$crate` paths so they work from any `ops/` submodule.
// ---------------------------------------------------------------------------

macro_rules! op_read {
    ($name:ident, $inner:expr, imm, $cycles:literal) => {
        pub fn $name(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
            let operand = bus.read(cpu.pc());
            cpu.set_pc(cpu.pc().wrapping_add(1));
            $inner(cpu, operand);
            $cycles
        }
    };
    ($name:ident, $inner:expr, zp, $cycles:literal) => {
        pub fn $name(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
            let addr = $crate::ops::addr_modes::zp(cpu, bus);
            cpu.set_pc(cpu.pc().wrapping_add(1));
            $inner(cpu, bus.read(addr));
            $cycles
        }
    };
    ($name:ident, $inner:expr, zpx, $cycles:literal) => {
        pub fn $name(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
            let addr = $crate::ops::addr_modes::zpx(cpu, bus);
            cpu.set_pc(cpu.pc().wrapping_add(1));
            $inner(cpu, bus.read(addr));
            $cycles
        }
    };
    ($name:ident, $inner:expr, zpy, $cycles:literal) => {
        pub fn $name(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
            let addr = $crate::ops::addr_modes::zpy(cpu, bus);
            cpu.set_pc(cpu.pc().wrapping_add(1));
            $inner(cpu, bus.read(addr));
            $cycles
        }
    };
    ($name:ident, $inner:expr, abs, $cycles:literal) => {
        pub fn $name(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
            let addr = $crate::ops::addr_modes::abs(cpu, bus);
            cpu.set_pc(cpu.pc().wrapping_add(2));
            $inner(cpu, bus.read(addr));
            $cycles
        }
    };
    ($name:ident, $inner:expr, absx, $base:literal) => {
        pub fn $name(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
            let (addr, page) = $crate::ops::addr_modes::absx(cpu, bus);
            cpu.set_pc(cpu.pc().wrapping_add(2));
            $inner(cpu, bus.read(addr));
            $base + page
        }
    };
    ($name:ident, $inner:expr, absy, $base:literal) => {
        pub fn $name(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
            let (addr, page) = $crate::ops::addr_modes::absy(cpu, bus);
            cpu.set_pc(cpu.pc().wrapping_add(2));
            $inner(cpu, bus.read(addr));
            $base + page
        }
    };
    ($name:ident, $inner:expr, indx, $cycles:literal) => {
        pub fn $name(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
            let addr = $crate::ops::addr_modes::indx(cpu, bus);
            cpu.set_pc(cpu.pc().wrapping_add(1));
            $inner(cpu, bus.read(addr));
            $cycles
        }
    };
    ($name:ident, $inner:expr, indy, $base:literal) => {
        pub fn $name(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
            let (addr, page) = $crate::ops::addr_modes::indy(cpu, bus);
            cpu.set_pc(cpu.pc().wrapping_add(1));
            $inner(cpu, bus.read(addr));
            $base + page
        }
    };
}

// ---- Store: sta, stx, sty ----
// `$reg` is a method name on CpuRp2a03 (a, x, or y).

macro_rules! op_store {
    ($name:ident, $reg:ident, zp, $cycles:literal) => {
        pub fn $name(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
            let addr = $crate::ops::addr_modes::zp(cpu, bus);
            cpu.set_pc(cpu.pc().wrapping_add(1));
            bus.write(addr, cpu.$reg());
            $cycles
        }
    };
    ($name:ident, $reg:ident, zpx, $cycles:literal) => {
        pub fn $name(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
            let addr = $crate::ops::addr_modes::zpx(cpu, bus);
            cpu.set_pc(cpu.pc().wrapping_add(1));
            bus.write(addr, cpu.$reg());
            $cycles
        }
    };
    ($name:ident, $reg:ident, zpy, $cycles:literal) => {
        pub fn $name(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
            let addr = $crate::ops::addr_modes::zpy(cpu, bus);
            cpu.set_pc(cpu.pc().wrapping_add(1));
            bus.write(addr, cpu.$reg());
            $cycles
        }
    };
    ($name:ident, $reg:ident, abs, $cycles:literal) => {
        pub fn $name(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
            let addr = $crate::ops::addr_modes::abs(cpu, bus);
            cpu.set_pc(cpu.pc().wrapping_add(2));
            bus.write(addr, cpu.$reg());
            $cycles
        }
    };
    ($name:ident, $reg:ident, absx, $cycles:literal) => {
        pub fn $name(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
            let (addr, _) = $crate::ops::addr_modes::absx(cpu, bus);
            cpu.set_pc(cpu.pc().wrapping_add(2));
            let _ = bus.read(addr);
            bus.write(addr, cpu.$reg());
            $cycles
        }
    };
    ($name:ident, $reg:ident, absy, $cycles:literal) => {
        pub fn $name(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
            let (addr, _) = $crate::ops::addr_modes::absy(cpu, bus);
            cpu.set_pc(cpu.pc().wrapping_add(2));
            let _ = bus.read(addr);
            bus.write(addr, cpu.$reg());
            $cycles
        }
    };
    ($name:ident, $reg:ident, indx, $cycles:literal) => {
        pub fn $name(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
            let addr = $crate::ops::addr_modes::indx(cpu, bus);
            cpu.set_pc(cpu.pc().wrapping_add(1));
            bus.write(addr, cpu.$reg());
            $cycles
        }
    };
    ($name:ident, $reg:ident, indy, $cycles:literal) => {
        pub fn $name(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
            let (addr, _) = $crate::ops::addr_modes::indy(cpu, bus);
            cpu.set_pc(cpu.pc().wrapping_add(1));
            let _ = bus.read(addr);
            bus.write(addr, cpu.$reg());
            $cycles
        }
    };
}

// ---- RMW without carry (inc, dec) ----
// `$op:expr` is a closure `|v: u8| -> u8`.

macro_rules! op_rmw {
    ($name:ident, $op:expr, a, $cycles:literal) => {
        pub fn $name(cpu: &mut CpuRp2a03, _bus: &mut Bus) -> u8 {
            let result = ($op)(cpu.a());
            cpu.set_a(result);
            cpu.set_sign(result);
            cpu.set_zero(result);
            $cycles
        }
    };
    ($name:ident, $op:expr, zp, $cycles:literal) => {
        pub fn $name(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
            let addr = $crate::ops::addr_modes::zp(cpu, bus);
            cpu.set_pc(cpu.pc().wrapping_add(1));
            let val = bus.read(addr);
            bus.write(addr, val);
            let result = ($op)(val);
            bus.write(addr, result);
            cpu.set_sign(result);
            cpu.set_zero(result);
            $cycles
        }
    };
    ($name:ident, $op:expr, zpx, $cycles:literal) => {
        pub fn $name(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
            let addr = $crate::ops::addr_modes::zpx(cpu, bus);
            cpu.set_pc(cpu.pc().wrapping_add(1));
            let val = bus.read(addr);
            bus.write(addr, val);
            let result = ($op)(val);
            bus.write(addr, result);
            cpu.set_sign(result);
            cpu.set_zero(result);
            $cycles
        }
    };
    ($name:ident, $op:expr, abs, $cycles:literal) => {
        pub fn $name(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
            let addr = $crate::ops::addr_modes::abs(cpu, bus);
            cpu.set_pc(cpu.pc().wrapping_add(2));
            let val = bus.read(addr);
            bus.write(addr, val);
            let result = ($op)(val);
            bus.write(addr, result);
            cpu.set_sign(result);
            cpu.set_zero(result);
            $cycles
        }
    };
    ($name:ident, $op:expr, absx, $cycles:literal) => {
        pub fn $name(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
            let (addr, _) = $crate::ops::addr_modes::absx(cpu, bus);
            cpu.set_pc(cpu.pc().wrapping_add(2));
            let val = bus.read(addr);
            bus.write(addr, val);
            let result = ($op)(val);
            bus.write(addr, result);
            cpu.set_sign(result);
            cpu.set_zero(result);
            $cycles
        }
    };
}

// ---- RMW with carry output (asl, lsr) ----
// `$op:expr` is `|v: u8| -> (u8, bool)`.

macro_rules! op_rmw_carry {
    ($name:ident, $op:expr, a, $cycles:literal) => {
        pub fn $name(cpu: &mut CpuRp2a03, _bus: &mut Bus) -> u8 {
            let val = cpu.a();
            let (result, carry) = ($op)(val);
            cpu.set_flag($crate::cpu::FLAG_CARRY, carry);
            cpu.set_a(result);
            cpu.set_sign(result);
            cpu.set_zero(result);
            $cycles
        }
    };
    ($name:ident, $op:expr, zp, $cycles:literal) => {
        pub fn $name(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
            let addr = $crate::ops::addr_modes::zp(cpu, bus);
            cpu.set_pc(cpu.pc().wrapping_add(1));
            let val = bus.read(addr);
            bus.write(addr, val);
            let (result, carry) = ($op)(val);
            cpu.set_flag($crate::cpu::FLAG_CARRY, carry);
            bus.write(addr, result);
            cpu.set_sign(result);
            cpu.set_zero(result);
            $cycles
        }
    };
    ($name:ident, $op:expr, zpx, $cycles:literal) => {
        pub fn $name(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
            let addr = $crate::ops::addr_modes::zpx(cpu, bus);
            cpu.set_pc(cpu.pc().wrapping_add(1));
            let val = bus.read(addr);
            bus.write(addr, val);
            let (result, carry) = ($op)(val);
            cpu.set_flag($crate::cpu::FLAG_CARRY, carry);
            bus.write(addr, result);
            cpu.set_sign(result);
            cpu.set_zero(result);
            $cycles
        }
    };
    ($name:ident, $op:expr, abs, $cycles:literal) => {
        pub fn $name(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
            let addr = $crate::ops::addr_modes::abs(cpu, bus);
            cpu.set_pc(cpu.pc().wrapping_add(2));
            let val = bus.read(addr);
            bus.write(addr, val);
            let (result, carry) = ($op)(val);
            cpu.set_flag($crate::cpu::FLAG_CARRY, carry);
            bus.write(addr, result);
            cpu.set_sign(result);
            cpu.set_zero(result);
            $cycles
        }
    };
    ($name:ident, $op:expr, absx, $cycles:literal) => {
        pub fn $name(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
            let (addr, _) = $crate::ops::addr_modes::absx(cpu, bus);
            cpu.set_pc(cpu.pc().wrapping_add(2));
            let val = bus.read(addr);
            bus.write(addr, val);
            let (result, carry) = ($op)(val);
            cpu.set_flag($crate::cpu::FLAG_CARRY, carry);
            bus.write(addr, result);
            cpu.set_sign(result);
            cpu.set_zero(result);
            $cycles
        }
    };
}

// ---- RMW with carry input + output (rol, ror) ----
// `$op:expr` is `|v: u8, carry_in: bool| -> (u8, bool)`.

macro_rules! op_rmw_rotate {
    ($name:ident, $op:expr, a, $cycles:literal) => {
        pub fn $name(cpu: &mut CpuRp2a03, _bus: &mut Bus) -> u8 {
            let val = cpu.a();
            let carry = cpu.get_flag($crate::cpu::FLAG_CARRY);
            let (result, new_carry) = ($op)(val, carry);
            cpu.set_flag($crate::cpu::FLAG_CARRY, new_carry);
            cpu.set_a(result);
            cpu.set_sign(result);
            cpu.set_zero(result);
            $cycles
        }
    };
    ($name:ident, $op:expr, zp, $cycles:literal) => {
        pub fn $name(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
            let addr = $crate::ops::addr_modes::zp(cpu, bus);
            cpu.set_pc(cpu.pc().wrapping_add(1));
            let val = bus.read(addr);
            bus.write(addr, val);
            let carry = cpu.get_flag($crate::cpu::FLAG_CARRY);
            let (result, new_carry) = ($op)(val, carry);
            cpu.set_flag($crate::cpu::FLAG_CARRY, new_carry);
            bus.write(addr, result);
            cpu.set_sign(result);
            cpu.set_zero(result);
            $cycles
        }
    };
    ($name:ident, $op:expr, zpx, $cycles:literal) => {
        pub fn $name(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
            let addr = $crate::ops::addr_modes::zpx(cpu, bus);
            cpu.set_pc(cpu.pc().wrapping_add(1));
            let val = bus.read(addr);
            bus.write(addr, val);
            let carry = cpu.get_flag($crate::cpu::FLAG_CARRY);
            let (result, new_carry) = ($op)(val, carry);
            cpu.set_flag($crate::cpu::FLAG_CARRY, new_carry);
            bus.write(addr, result);
            cpu.set_sign(result);
            cpu.set_zero(result);
            $cycles
        }
    };
    ($name:ident, $op:expr, abs, $cycles:literal) => {
        pub fn $name(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
            let addr = $crate::ops::addr_modes::abs(cpu, bus);
            cpu.set_pc(cpu.pc().wrapping_add(2));
            let val = bus.read(addr);
            bus.write(addr, val);
            let carry = cpu.get_flag($crate::cpu::FLAG_CARRY);
            let (result, new_carry) = ($op)(val, carry);
            cpu.set_flag($crate::cpu::FLAG_CARRY, new_carry);
            bus.write(addr, result);
            cpu.set_sign(result);
            cpu.set_zero(result);
            $cycles
        }
    };
    ($name:ident, $op:expr, absx, $cycles:literal) => {
        pub fn $name(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
            let (addr, _) = $crate::ops::addr_modes::absx(cpu, bus);
            cpu.set_pc(cpu.pc().wrapping_add(2));
            let val = bus.read(addr);
            bus.write(addr, val);
            let carry = cpu.get_flag($crate::cpu::FLAG_CARRY);
            let (result, new_carry) = ($op)(val, carry);
            cpu.set_flag($crate::cpu::FLAG_CARRY, new_carry);
            bus.write(addr, result);
            cpu.set_sign(result);
            cpu.set_zero(result);
            $cycles
        }
    };
}
