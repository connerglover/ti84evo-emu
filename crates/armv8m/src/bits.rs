//! Pseudocode helpers named after the Arm ARM (DDI0553) shared pseudocode.

use crate::regs::Xpsr;

/// Shift and rotate kinds (`SRType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SRType {
    Lsl,
    Lsr,
    Asr,
    Ror,
    Rrx,
}

/// `AddWithCarry(x, y, carry_in)`: returns `(result, carry_out, overflow)`.
pub fn add_with_carry(x: u32, y: u32, carry_in: bool) -> (u32, bool, bool) {
    let unsigned_sum = u64::from(x) + u64::from(y) + u64::from(carry_in);
    let signed_sum = i64::from(x as i32) + i64::from(y as i32) + i64::from(carry_in);
    let result = unsigned_sum as u32;
    (
        result,
        u64::from(result) != unsigned_sum,
        i64::from(result as i32) != signed_sum,
    )
}

/// `Shift_C(value, type, amount, carry_in)`: returns `(result, carry_out)`.
pub fn shift_c(v: u32, t: SRType, amount: u32, carry_in: bool) -> (u32, bool) {
    if t == SRType::Rrx {
        return ((u32::from(carry_in) << 31) | (v >> 1), v & 1 != 0);
    }
    if amount == 0 {
        return (v, carry_in);
    }
    match t {
        SRType::Lsl => match amount {
            1..=31 => (v << amount, (v >> (32 - amount)) & 1 != 0),
            32 => (0, v & 1 != 0),
            _ => (0, false),
        },
        SRType::Lsr => match amount {
            1..=31 => (v >> amount, (v >> (amount - 1)) & 1 != 0),
            32 => (0, v >> 31 != 0),
            _ => (0, false),
        },
        SRType::Asr => match amount {
            1..=31 => (((v as i32) >> amount) as u32, (v >> (amount - 1)) & 1 != 0),
            _ => (((v as i32) >> 31) as u32, v >> 31 != 0),
        },
        SRType::Ror => {
            let r = v.rotate_right(amount % 32);
            (r, r >> 31 != 0)
        }
        SRType::Rrx => unreachable!("handled above"),
    }
}

/// `ThumbExpandImm_C(imm12, carry_in)`: returns `(imm32, carry_out)`.
pub fn thumb_expand_imm_c(imm12: u32, carry_in: bool) -> (u32, bool) {
    let imm8 = imm12 & 0xFF;
    if (imm12 >> 10) & 3 == 0 {
        let v = match (imm12 >> 8) & 3 {
            0 => imm8,
            1 => (imm8 << 16) | imm8,
            2 => (imm8 << 24) | (imm8 << 8),
            _ => imm8 * 0x0101_0101,
        };
        (v, carry_in)
    } else {
        let unrotated = 0x80 | (imm12 & 0x7F);
        let r = unrotated.rotate_right(imm12 >> 7);
        (r, r >> 31 != 0)
    }
}

/// `DecodeImmShift(type, imm5)`: returns `(shift_type, shift_amount)`.
pub fn decode_imm_shift(ty: u32, imm5: u32) -> (SRType, u32) {
    match ty & 3 {
        0 => (SRType::Lsl, imm5),
        1 => (SRType::Lsr, if imm5 == 0 { 32 } else { imm5 }),
        2 => (SRType::Asr, if imm5 == 0 { 32 } else { imm5 }),
        _ if imm5 == 0 => (SRType::Rrx, 1),
        _ => (SRType::Ror, imm5),
    }
}

/// `ConditionPassed()` for an explicit 4-bit condition field.
pub fn condition_holds(cond: u8, x: &Xpsr) -> bool {
    let r = match cond >> 1 {
        0 => x.z,
        1 => x.c,
        2 => x.n,
        3 => x.v,
        4 => x.c && !x.z,
        5 => x.n == x.v,
        6 => x.n == x.v && !x.z,
        _ => return true,
    };
    if cond & 1 != 0 { !r } else { r }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn awc_flags() {
        assert_eq!(
            add_with_carry(0x7FFF_FFFF, 1, false),
            (0x8000_0000, false, true)
        );
        assert_eq!(add_with_carry(0xFFFF_FFFF, 1, false), (0, true, false));
        assert_eq!(add_with_carry(0, !1, true), (0xFFFF_FFFF, false, false));
    }

    #[test]
    fn shifts() {
        assert_eq!(shift_c(0x8000_0001, SRType::Lsl, 1, false), (2, true));
        assert_eq!(shift_c(1, SRType::Lsl, 33, true), (0, false));
        assert_eq!(shift_c(0x8000_0000, SRType::Lsr, 32, false), (0, true));
        assert_eq!(
            shift_c(0x8000_0000, SRType::Asr, 40, false),
            (u32::MAX, true)
        );
        assert_eq!(shift_c(0x1, SRType::Ror, 1, false), (0x8000_0000, true));
        assert_eq!(shift_c(3, SRType::Rrx, 1, true), (0x8000_0001, true));
        assert_eq!(shift_c(7, SRType::Lsl, 0, true), (7, true));
    }

    #[test]
    fn expand_imm() {
        assert_eq!(thumb_expand_imm_c(0x0AB, false), (0xAB, false));
        assert_eq!(thumb_expand_imm_c(0x1AB, true), (0x00AB_00AB, true));
        assert_eq!(thumb_expand_imm_c(0x2AB, false), (0xAB00_AB00, false));
        assert_eq!(thumb_expand_imm_c(0x3AB, false), (0xABAB_ABAB, false));
        assert_eq!(thumb_expand_imm_c(0x4AB, false), (0x5580_0000, false));
        assert_eq!(thumb_expand_imm_c(0xFFF, false), (0x1FE, false));
        assert_eq!(thumb_expand_imm_c(0x400, false), (0x8000_0000, true));
    }

    #[test]
    fn imm_shift_decode() {
        assert_eq!(decode_imm_shift(1, 0), (SRType::Lsr, 32));
        assert_eq!(decode_imm_shift(3, 0), (SRType::Rrx, 1));
        assert_eq!(decode_imm_shift(3, 4), (SRType::Ror, 4));
    }

    #[test]
    fn conditions() {
        let x = Xpsr {
            z: true,
            ..Default::default()
        };
        assert!(condition_holds(0, &x));
        assert!(!condition_holds(1, &x));
        assert!(condition_holds(0xE, &x));
    }
}
