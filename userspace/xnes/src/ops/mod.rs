use crate::bus::Bus;
use crate::cpu::CpuRp2a03;

pub(crate) type Op = fn(&mut CpuRp2a03, &mut Bus) -> u8;

#[macro_use]
mod macros;

#[inline(always)]
pub(crate) fn push(cpu: &mut CpuRp2a03, bus: &mut Bus, val: u8) {
    bus.write(0x0100 | cpu.st() as u16, val);
    cpu.set_st(cpu.st().wrapping_sub(1));
}

#[inline(always)]
pub(crate) fn pull(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    cpu.set_st(cpu.st().wrapping_add(1));
    bus.read(0x0100 | cpu.st() as u16)
}

mod addr_modes {
    use crate::bus::Bus;
    use crate::cpu::CpuRp2a03;

    #[inline(always)]
    pub fn abs(cpu: &CpuRp2a03, bus: &mut Bus) -> u16 {
        let lo = bus.read(cpu.pc()) as u16;
        let hi = bus.read(cpu.pc().wrapping_add(1)) as u16;
        lo | (hi << 8)
    }

    #[inline(always)]
    pub fn absx(cpu: &CpuRp2a03, bus: &mut Bus) -> (u16, u8) {
        let base = abs(cpu, bus);
        let addr = base.wrapping_add(cpu.x() as u16);
        let page = (((base ^ addr) >> 8) as u8) & 1;
        // Dummy read when page boundary is crossed
        if page != 0 {
            let dummy_addr = (base & 0xFF00) | (addr as u8 as u16);
            bus.read(dummy_addr);
        }
        (addr, page)
    }

    #[inline(always)]
    pub fn absy(cpu: &CpuRp2a03, bus: &mut Bus) -> (u16, u8) {
        let base = abs(cpu, bus);
        let addr = base.wrapping_add(cpu.y() as u16);
        let page = (((base ^ addr) >> 8) as u8) & 1;
        // Dummy read when page boundary is crossed
        if page != 0 {
            let dummy_addr = (base & 0xFF00) | (addr as u8 as u16);
            bus.read(dummy_addr);
        }
        (addr, page)
    }

    #[inline(always)]
    pub fn zp(cpu: &CpuRp2a03, bus: &mut Bus) -> u16 {
        bus.read(cpu.pc()) as u16
    }

    #[inline(always)]
    pub fn zpx(cpu: &CpuRp2a03, bus: &mut Bus) -> u16 {
        bus.read(cpu.pc()).wrapping_add(cpu.x()) as u16
    }

    #[inline(always)]
    pub fn zpy(cpu: &CpuRp2a03, bus: &mut Bus) -> u16 {
        bus.read(cpu.pc()).wrapping_add(cpu.y()) as u16
    }

    #[inline(always)]
    pub fn indx(cpu: &CpuRp2a03, bus: &mut Bus) -> u16 {
        let ptr = bus.read(cpu.pc()).wrapping_add(cpu.x()) as u16;
        let lo = bus.read(ptr) as u16;
        // Zero-page wrap: when ptr = $FF, read high byte from $00, not $100
        let hi_ptr = (ptr as u8).wrapping_add(1) as u16;
        let hi = bus.read(hi_ptr) as u16;
        lo | (hi << 8)
    }

    #[inline(always)]
    pub fn indy(cpu: &CpuRp2a03, bus: &mut Bus) -> (u16, u8) {
        let ptr = bus.read(cpu.pc()) as u16;
        // Zero-page wrap: when ptr = $FF, read high byte from $00, not $100
        let hi_ptr = (ptr as u8).wrapping_add(1) as u16;
        let base = bus.read(ptr) as u16 | (bus.read(hi_ptr) as u16) << 8;
        let addr = base.wrapping_add(cpu.y() as u16);
        let page = (((base ^ addr) >> 8) as u8) & 1;
        // Dummy read when page boundary is crossed
        if page != 0 {
            let dummy_addr = (base & 0xFF00) | (addr as u8 as u16);
            bus.read(dummy_addr);
        }
        (addr, page)
    }
}

/// Base (minimum) CPU cycles for each opcode, used by `tick()` to pre-tick APU
/// before instruction execution so DMC DMA timing is detected.
pub static BASE_CYCLES: [u8; 256] = [
    // 0x00-0x0F
    7, 6, 2, 8, 3, 3, 5, 5, 3, 2, 2, 2, 4, 4, 6, 6, // 0x10-0x1F
    2, 5, 2, 8, 4, 4, 6, 6, 2, 4, 2, 7, 4, 4, 7, 7, // 0x20-0x2F
    6, 6, 2, 8, 3, 3, 5, 5, 4, 2, 2, 2, 4, 4, 6, 6, // 0x30-0x3F
    2, 5, 2, 8, 4, 4, 6, 6, 2, 4, 2, 7, 4, 4, 7, 7, // 0x40-0x4F
    7, 6, 2, 8, 3, 3, 5, 5, 3, 2, 2, 2, 3, 4, 6, 6, // 0x50-0x5F
    2, 5, 2, 8, 4, 4, 6, 6, 2, 4, 2, 7, 4, 4, 7, 7, // 0x60-0x6F
    6, 6, 2, 8, 3, 3, 5, 5, 4, 2, 2, 2, 5, 4, 6, 6, // 0x70-0x7F
    2, 5, 2, 8, 4, 4, 6, 6, 2, 4, 2, 7, 4, 4, 7, 7, // 0x80-0x8F
    2, 6, 2, 6, 3, 3, 3, 3, 2, 2, 2, 2, 4, 4, 4, 4, // 0x90-0x9F
    2, 6, 2, 6, 4, 4, 4, 4, 2, 5, 2, 5, 5, 5, 5, 5, // 0xA0-0xAF
    2, 6, 2, 6, 3, 3, 3, 3, 2, 2, 2, 2, 4, 4, 4, 4, // 0xB0-0xBF
    2, 5, 2, 5, 4, 4, 4, 4, 2, 4, 2, 4, 4, 4, 4, 4, // 0xC0-0xCF
    2, 6, 2, 8, 3, 3, 5, 5, 2, 2, 2, 2, 4, 4, 6, 6, // 0xD0-0xDF
    2, 5, 2, 8, 4, 4, 6, 6, 2, 4, 2, 7, 4, 4, 7, 7, // 0xE0-0xEF
    2, 6, 2, 8, 3, 3, 5, 5, 2, 2, 2, 2, 4, 4, 6, 6, // 0xF0-0xFF
    2, 5, 2, 8, 4, 4, 6, 6, 2, 4, 2, 7, 4, 4, 7, 7,
];

pub static TABLE: [Op; 256] = {
    use self::arithmetic::*;
    use self::branch::*;
    use self::flag::*;
    use self::jump::*;
    use self::logic::*;
    use self::nop::*;
    use self::rmw::*;
    use self::sh::*;
    use self::shift::*;
    use self::stack::*;
    use self::transfer::*;
    use self::unofficial::*;
    [
        // 0x00-0x0F
        brk, ora_indx, nop_imm, slo_indx, nop_zp, ora_zp, asl_zp, slo_zp, php, ora_imm, asl_a,
        anc_imm, nop_abs, ora_abs, asl_abs, slo_abs, // 0x10-0x1F
        bpl, ora_indy, nop_imm, slo_indy, nop_zpx, ora_zpx, asl_zpx, slo_zpx, clc, ora_absy, nop,
        slo_absy, nop_absx, ora_absx, asl_absx, slo_absx, // 0x20-0x2F
        jsr, and_indx, nop_imm, rla_indx, bit_zp, and_zp, rol_zp, rla_zp, plp, and_imm, rol_a,
        anc_imm, bit_abs, and_abs, rol_abs, rla_abs, // 0x30-0x3F
        bmi, and_indy, nop_imm, rla_indy, nop_zpx, and_zpx, rol_zpx, rla_zpx, sec, and_absy, nop,
        rla_absy, nop_absx, and_absx, rol_absx, rla_absx, // 0x40-0x4F
        rti, eor_indx, nop_imm, sre_indx, nop_zp, eor_zp, lsr_zp, sre_zp, pha, eor_imm, lsr_a,
        asr_imm, jmp_abs, eor_abs, lsr_abs, sre_abs, // 0x50-0x5F
        bvc, eor_indy, nop_imm, sre_indy, nop_zpx, eor_zpx, lsr_zpx, sre_zpx, cli, eor_absy, nop,
        sre_absy, nop_absx, eor_absx, lsr_absx, sre_absx, // 0x60-0x6F
        rts, adc_indx, nop_imm, rra_indx, nop_zp, adc_zp, ror_zp, rra_zp, pla, adc_imm, ror_a,
        arr_imm, jmp_ind, adc_abs, ror_abs, rra_abs, // 0x70-0x7F
        bvs, adc_indy, nop_imm, rra_indy, nop_zpx, adc_zpx, ror_zpx, rra_zpx, sei, adc_absy, nop,
        rra_absy, nop_absx, adc_absx, ror_absx, rra_absx, // 0x80-0x8F
        nop_imm, sta_indx, nop_imm, sax_indx, sty_zp, sta_zp, stx_zp, sax_zp, dey, nop_imm, txa,
        ane_imm, sty_abs, sta_abs, stx_abs, sax_abs, // 0x90-0x9F
        bcc, sta_indy, nop_imm, sha_indy, sty_zpx, sta_zpx, stx_zpy, sax_zpy, tya, sta_absy, txs,
        shs_absy, shy_absx, sta_absx, shx_absy, sha_absy, // 0xA0-0xAF
        ldy_imm, lda_indx, ldx_imm, lax_indx, ldy_zp, lda_zp, ldx_zp, lax_zp, tay, lda_imm, tax,
        lxa_imm, ldy_abs, lda_abs, ldx_abs, lax_abs, // 0xB0-0xBF
        bcs, lda_indy, nop_imm, lax_indy, ldy_zpx, lda_zpx, ldx_zpy, lax_zpy, clv, lda_absy, tsx,
        lae_absy, ldy_absx, lda_absx, ldx_absy, lax_absy, // 0xC0-0xCF
        cpy_imm, cmp_indx, nop_imm, dcp_indx, cpy_zp, cmp_zp, dec_zp, dcp_zp, iny, cmp_imm, dex,
        axs_imm, cpy_abs, cmp_abs, dec_abs, dcp_abs, // 0xD0-0xDF
        bne, cmp_indy, nop_imm, dcp_indy, nop_zpx, cmp_zpx, dec_zpx, dcp_zpx, cld, cmp_absy, nop,
        dcp_absy, nop_absx, cmp_absx, dec_absx, dcp_absx, // 0xE0-0xEF
        cpx_imm, sbc_indx, nop_imm, isc_indx, cpx_zp, sbc_zp, inc_zp, isc_zp, inx, sbc_imm, nop,
        sbc_imm, cpx_abs, sbc_abs, inc_abs, isc_abs, // 0xF0-0xFF
        beq, sbc_indy, nop_imm, isc_indy, nop_zpx, sbc_zpx, inc_zpx, isc_zpx, sed, sbc_absy, nop,
        isc_absy, nop_absx, sbc_absx, inc_absx, isc_absx,
    ]
};

mod arithmetic;
mod branch;
mod flag;
mod jump;
mod logic;
mod nop;
mod rmw;
mod sh;
mod shift;
mod stack;
mod transfer;
mod unofficial;
