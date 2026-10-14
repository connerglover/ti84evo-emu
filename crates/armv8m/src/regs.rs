//! Architectural register model (ARMv8-M with Security Extension).

use serde::{Deserialize, Serialize};

/// Security state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Sec {
    #[default]
    Secure,
    NonSecure,
}

/// A register that exists once per security state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Banked<T> {
    pub s: T,
    pub ns: T,
}

impl<T> Banked<T> {
    pub fn get(&self, sec: Sec) -> &T {
        match sec {
            Sec::Secure => &self.s,
            Sec::NonSecure => &self.ns,
        }
    }

    pub fn get_mut(&mut self, sec: Sec) -> &mut T {
        match sec {
            Sec::Secure => &mut self.s,
            Sec::NonSecure => &mut self.ns,
        }
    }
}

/// Combined program status register, unpacked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Xpsr {
    pub n: bool,
    pub z: bool,
    pub c: bool,
    pub v: bool,
    pub q: bool,
    pub ge: u8,
    pub it: u8,
    pub t: bool,
    pub exception: u16,
}

impl Xpsr {
    pub fn to_u32(&self) -> u32 {
        let it = u32::from(self.it);
        (u32::from(self.n) << 31)
            | (u32::from(self.z) << 30)
            | (u32::from(self.c) << 29)
            | (u32::from(self.v) << 28)
            | (u32::from(self.q) << 27)
            | ((it & 0x3) << 25)
            | (u32::from(self.t) << 24)
            | ((u32::from(self.ge) & 0xF) << 16)
            | ((it >> 2) << 10)
            | (u32::from(self.exception) & 0x1FF)
    }

    pub fn from_u32(v: u32) -> Self {
        let it = (((v >> 10) & 0x3F) << 2) | ((v >> 25) & 0x3);
        Self {
            n: v >> 31 & 1 != 0,
            z: v >> 30 & 1 != 0,
            c: v >> 29 & 1 != 0,
            v: v >> 28 & 1 != 0,
            q: v >> 27 & 1 != 0,
            ge: ((v >> 16) & 0xF) as u8,
            it: it as u8,
            t: v >> 24 & 1 != 0,
            exception: (v & 0x1FF) as u16,
        }
    }
}

/// CPU register file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registers {
    pub r: [u32; 13],
    pub lr: u32,
    pub pc: u32,
    pub xpsr: Xpsr,
    pub msp: Banked<u32>,
    pub psp: Banked<u32>,
    pub msplim: Banked<u32>,
    pub psplim: Banked<u32>,
    pub control: Banked<u32>,
    pub primask: Banked<bool>,
    pub basepri: Banked<u8>,
    pub faultmask: Banked<bool>,
    pub sec: Sec,
}

impl Registers {
    /// True when executing an exception handler (IPSR != 0).
    pub fn handler_mode(&self) -> bool {
        self.xpsr.exception != 0
    }

    fn uses_psp(&self) -> bool {
        !self.handler_mode() && self.control.get(self.sec) & 0b10 != 0
    }

    /// Currently selected stack pointer.
    pub fn sp(&self) -> u32 {
        if self.uses_psp() {
            *self.psp.get(self.sec)
        } else {
            *self.msp.get(self.sec)
        }
    }

    pub fn set_sp(&mut self, v: u32) {
        let sec = self.sec;
        if self.uses_psp() {
            *self.psp.get_mut(sec) = v;
        } else {
            *self.msp.get_mut(sec) = v;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xpsr_roundtrip() {
        let v = 0xF80F_FC00 | 0x0100_0000 | 0x2B;
        assert_eq!(Xpsr::from_u32(v).to_u32(), v);
        // IT bits split across [26:25] and [15:10]
        let v = 0x0600_0000 | 0x0000_FC00;
        let x = Xpsr::from_u32(v);
        assert_eq!(x.it, 0xFF);
        assert_eq!(x.to_u32(), v);
        let x = Xpsr::from_u32(0x0200_0000);
        assert_eq!(x.it, 0b01);
    }

    #[test]
    fn default_is_secure() {
        assert_eq!(Registers::default().sec, Sec::Secure);
    }

    #[test]
    fn sp_selects_banked_psp() {
        let mut r = Registers {
            sec: Sec::NonSecure,
            ..Default::default()
        };
        r.control.ns = 0b10;
        r.msp.ns = 0x1000;
        r.psp.ns = 0x2000;
        assert!(!r.handler_mode());
        assert_eq!(r.sp(), 0x2000);
        r.set_sp(0x3000);
        assert_eq!(r.psp.ns, 0x3000);
        assert_eq!(r.msp.ns, 0x1000);
        r.xpsr.exception = 3;
        assert!(r.handler_mode());
        assert_eq!(r.sp(), 0x1000);
    }
}
