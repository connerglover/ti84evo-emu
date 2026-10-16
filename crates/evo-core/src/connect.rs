//! Native TI Connect transport: actual USB CDC tokens and Kermit resource transfers.
//! Packet framing follows https://www.kermitproject.org/kpackets.html; Evo's URI
//! upload/download convention is observed in the unchanged official client.
use crate::{CPU_HZ, Machine, StopReason, UsbError};
use std::{collections::VecDeque, ops::ControlFlow};

const INIT: &[u8] = b"~% @-#Y1~.!5M";
const MAX_RESOURCE: usize = 64 * 1024 * 1024;
const QUANTUM: u64 = CPU_HZ / 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Connecting,
    Sending,
    Receiving,
    Finishing,
}
#[derive(Clone, Debug)]
pub struct Progress {
    pub phase: Phase,
    pub transferred: usize,
    pub total: Option<usize>,
    /// Packet type, sequence and byte count only; never payload contents.
    pub detail: Option<String>,
}
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error("USB: {0}")]
    Usb(#[from] UsbError),
    #[error("calculator stopped: {0:?}")]
    GuestStopped(StopReason),
    #[error("calculator did not respond in time")]
    Timeout,
    #[error("invalid transfer: {0}")]
    Protocol(String),
    #[error("calculator rejected the transfer: {0}")]
    Remote(String),
    #[error("transfer cancelled")]
    Cancelled,
}
type Result<T> = std::result::Result<T, ConnectError>;

/// A single native connection. Dropping detaches and lets the guest observe it.
/// The caller must run a normally booted machine; this never provisions or patches it.
pub struct Connect<'a> {
    machine: &'a mut Machine,
    progress: &'a mut dyn FnMut(Progress) -> ControlFlow<()>,
    input: u8,
    output: u8,
    packet_size: usize,
    rx: VecDeque<u8>,
    phase: Phase,
    transferred: usize,
    total: Option<usize>,
    closed: bool,
}
impl Drop for Connect<'_> {
    fn drop(&mut self) {
        if self.closed {
            return;
        }
        self.machine.usb_attach(false);
        // Physical cable transitions need guest time, including after cancellation.
        let _ = self.machine.run(CPU_HZ / 4);
    }
}
impl<'a> Connect<'a> {
    pub fn new(
        machine: &'a mut Machine,
        progress: &'a mut dyn FnMut(Progress) -> ControlFlow<()>,
    ) -> Result<Self> {
        let mut this = Self {
            machine,
            progress,
            input: 0,
            output: 0,
            packet_size: 64,
            rx: VecDeque::new(),
            phase: Phase::Connecting,
            transferred: 0,
            total: None,
            closed: false,
        };
        this.machine.usb_attach(true);
        this.run(CPU_HZ / 4)?;
        this.machine.usb_bus_reset()?;
        this.run(CPU_HZ / 100)?;
        let device = this.control([0x80, 6, 0, 1, 0, 0, 18, 0], &[])?;
        if device.len() != 18 || device[7] != 64 || device[8..12] != [0x51, 4, 0x18, 0xe0] {
            return Err(protocol("unexpected Evo USB device descriptor"));
        }
        this.control([0, 5, 1, 0, 0, 0, 0, 0], &[])?;
        let config = this.control([0x80, 6, 0, 2, 0, 0, 255, 0], &[])?;
        let mut class = 0;
        let mut cdc = None;
        let mut pos = 0;
        while pos + 2 <= config.len() {
            let n = config[pos] as usize;
            if n < 2 || pos + n > config.len() {
                return Err(protocol("invalid configuration descriptor"));
            }
            let d = &config[pos..pos + n];
            if d[1] == 4 && n >= 9 {
                class = d[5];
                if class == 2 {
                    cdc = Some(d[2]);
                }
            }
            if d[1] == 5 && n >= 7 && class == 10 && d[3] & 3 == 2 {
                if u16::from_le_bytes([d[4], d[5]]) != 64 {
                    return Err(protocol("unsupported CDC packet size"));
                }
                if d[2] & 0x80 != 0 {
                    this.input = d[2] & 15;
                } else {
                    this.output = d[2] & 15;
                }
            }
            pos += n;
        }
        let cdc = cdc.ok_or_else(|| protocol("CDC control interface missing"))?;
        if this.input == 0 || this.output == 0 || config.len() < 9 {
            return Err(protocol("CDC data interface missing"));
        }
        this.control([0, 9, config[5], 0, 0, 0, 0, 0], &[])?;
        this.control([0x21, 0x20, 0, 0, cdc, 0, 7, 0], &[0, 0xc2, 1, 0, 0, 0, 8])?;
        this.control([0x21, 0x22, 3, 0, cdc, 0, 0, 0], &[])?;
        Ok(this)
    }
    fn run(&mut self, cycles: u64) -> Result<()> {
        if (self.progress)(Progress {
            phase: self.phase,
            transferred: self.transferred,
            total: self.total,
            detail: None,
        })
        .is_break()
        {
            return Err(ConnectError::Cancelled);
        }
        match self.machine.run(cycles) {
            StopReason::CycleLimit | StopReason::Idle => Ok(()),
            s => Err(ConnectError::GuestStopped(s)),
        }
    }
    /// Advance actual guest work (for example asynchronous exam-mode completion).
    pub fn advance(&mut self, cycles: u64) -> Result<()> {
        let mut remaining = cycles;
        while remaining > 0 {
            let n = remaining.min(CPU_HZ / 100);
            self.run(n)?;
            remaining -= n;
        }
        Ok(())
    }
    /// Detach and report any guest fault while processing the cable transition.
    /// Dropping without calling this still detaches, including on errors.
    pub fn finish(mut self) -> Result<()> {
        self.closed = true;
        self.machine.usb_attach(false);
        match self.machine.run(CPU_HZ / 4) {
            StopReason::CycleLimit | StopReason::Idle => Ok(()),
            stop => Err(ConnectError::GuestStopped(stop)),
        }
    }
    fn usb_in(&mut self, ep: u8) -> Result<Vec<u8>> {
        for _ in 0..5000 {
            match self.machine.usb_in(ep) {
                Ok(b) => return Ok(b),
                Err(UsbError::Nak) => self.run(QUANTUM)?,
                Err(e) => return Err(e.into()),
            }
        }
        Err(ConnectError::Timeout)
    }
    fn usb_out(&mut self, ep: u8, b: &[u8]) -> Result<()> {
        for _ in 0..5000 {
            match self.machine.usb_out(ep, b) {
                Ok(()) => return Ok(()),
                Err(UsbError::Nak) => self.run(QUANTUM)?,
                Err(e) => return Err(e.into()),
            }
        }
        Err(ConnectError::Timeout)
    }
    fn control(&mut self, setup: [u8; 8], bytes: &[u8]) -> Result<Vec<u8>> {
        self.machine.usb_setup(setup)?;
        self.run(QUANTUM)?;
        let mut result = Vec::new();
        if setup[0] & 0x80 != 0 {
            let n = u16::from_le_bytes([setup[6], setup[7]]) as usize;
            while result.len() < n {
                let p = self.usb_in(0)?;
                let short = p.len() < 64;
                result.extend(p);
                if short {
                    break;
                }
            }
            self.usb_out(0, &[])?;
        } else {
            for p in bytes.chunks(64) {
                self.usb_out(0, p)?;
                self.run(QUANTUM)?;
            }
            if !self.usb_in(0)?.is_empty() {
                return Err(protocol("nonempty control status"));
            }
        }
        self.run(QUANTUM)?;
        Ok(result)
    }
    fn send(&mut self, seq: u8, kind: u8, data: &[u8]) -> Result<()> {
        self.trace(format!(
            "TX {} sequence {seq}, {} bytes",
            char::from(kind),
            data.len()
        ))?;
        let frame = frame(seq, kind, data)?;
        for p in frame.chunks(self.packet_size) {
            self.usb_out(self.output, p)?;
        }
        Ok(())
    }
    fn receive(&mut self) -> Result<Packet> {
        let mut elapsed = 0;
        loop {
            if let Some(packet) = parse(&mut self.rx)? {
                self.trace(format!(
                    "RX {} sequence {}, {} bytes",
                    char::from(packet.kind),
                    packet.seq,
                    packet.data.len()
                ))?;
                return Ok(packet);
            }
            if elapsed >= CPU_HZ * 5 {
                return Err(ConnectError::Timeout);
            }
            match self.machine.usb_in(self.input) {
                Ok(p) => self.rx.extend(p),
                Err(UsbError::Nak) => {
                    self.run(QUANTUM)?;
                    elapsed += QUANTUM;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
    fn trace(&mut self, detail: String) -> Result<()> {
        if (self.progress)(Progress {
            phase: self.phase,
            transferred: self.transferred,
            total: self.total,
            detail: Some(detail),
        })
        .is_break()
        {
            Err(ConnectError::Cancelled)
        } else {
            Ok(())
        }
    }
    fn exchange(&mut self, seq: u8, kind: u8, data: &[u8]) -> Result<Vec<u8>> {
        for _ in 0..3 {
            self.send(seq, kind, data)?;
            loop {
                let p = match self.receive() {
                    Ok(packet) => packet,
                    Err(ConnectError::Timeout) => break,
                    Err(error) => return Err(error),
                };
                if p.kind == b'E' {
                    return Err(remote(&p.data));
                }
                if p.seq != seq {
                    continue;
                }
                if p.kind == b'Y' {
                    return Ok(p.data);
                }
                if p.kind == b'N' {
                    break;
                }
                return Err(protocol("unexpected acknowledgment"));
            }
        }
        Err(protocol("acknowledgment retry limit reached"))
    }
    /// Upload bytes to an Evo resource URI (including variable and update resources).
    pub fn put(&mut self, uri: &str, bytes: &[u8]) -> Result<()> {
        if uri.len() > 2048 || bytes.len() > MAX_RESOURCE {
            return Err(protocol("resource exceeds transfer limit"));
        }
        let filename = quote(uri.as_bytes());
        if filename.len() > 2036 {
            return Err(protocol(
                "quoted resource URI exceeds negotiated packet size",
            ));
        }
        self.phase = Phase::Sending;
        self.transferred = 0;
        self.total = Some(bytes.len());
        let init = self.exchange(0, b'S', INIT)?;
        if init.get(7) != Some(&b'1') {
            return Err(protocol("guest did not select Kermit checksum 1"));
        }
        self.exchange(1, b'F', &filename)?;
        let n = bytes.len().to_string();
        let mut attrs = b"\"\"B81".to_vec();
        attrs.push(32 + n.len() as u8);
        attrs.extend(n.as_bytes());
        attrs.extend(b"@ ");
        self.exchange(2, b'A', &attrs)?;
        let mut seq = 3;
        for chunk in bytes.chunks(900) {
            self.exchange(seq, b'D', &quote(chunk))?;
            seq = (seq + 1) & 63;
            self.transferred += chunk.len();
            self.run(1)?;
        }
        self.phase = Phase::Finishing;
        self.exchange(seq, b'Z', &[])?;
        seq = (seq + 1) & 63;
        self.exchange(seq, b'B', &[])?;
        Ok(())
    }
    /// Download raw CBOR/binary resource bytes using Evo's request-upload convention.
    pub fn get(&mut self, uri: &str) -> Result<Vec<u8>> {
        self.put(&format!("hh01/get/{uri}"), b"h")?;
        self.phase = Phase::Receiving;
        self.transferred = 0;
        self.total = None;
        let mut result = Vec::new();
        let mut expected = 0u8;
        let mut last_ack = Vec::new();
        loop {
            let p = self.receive()?;
            if p.kind == b'E' {
                return Err(remote(&p.data));
            }
            if p.seq == expected.wrapping_sub(1) & 63 {
                self.send(p.seq, b'Y', &last_ack)?;
                continue;
            }
            if p.seq != expected {
                self.send(expected, b'N', &[])?;
                continue;
            }
            let ack = match p.kind {
                b'S' => INIT.to_vec(),
                b'F' => p.data.clone(),
                b'A' => {
                    self.total = attribute_size(&p.data);
                    b"Y".to_vec()
                }
                b'D' => {
                    let decoded = unquote(&p.data)?;
                    if result.len() + decoded.len() > MAX_RESOURCE {
                        return Err(protocol("download exceeds limit"));
                    }
                    result.extend(decoded);
                    self.transferred = result.len();
                    Vec::new()
                }
                b'Z' | b'B' => Vec::new(),
                _ => return Err(protocol("unexpected download packet")),
            };
            self.send(p.seq, b'Y', &ack)?;
            last_ack = ack;
            expected = (expected + 1) & 63;
            self.run(1)?;
            if p.kind == b'B' {
                // Let the guest consume the final ACK before starting another session.
                self.run(CPU_HZ / 100)?;
                return Ok(result);
            }
        }
    }
}
fn protocol(s: &str) -> ConnectError {
    ConnectError::Protocol(s.into())
}
fn remote(data: &[u8]) -> ConnectError {
    ConnectError::Remote(String::from_utf8_lossy(data).into_owned())
}
fn checksum(bytes: &[u8]) -> u8 {
    let s: usize = bytes.iter().map(|b| *b as usize).sum();
    32 + ((s + ((s & 192) >> 6)) & 63) as u8
}
fn frame(seq: u8, kind: u8, data: &[u8]) -> Result<Vec<u8>> {
    if seq >= 64 || data.len() > 8191 {
        return Err(protocol("packet too large"));
    }
    let mut out = vec![1];
    if data.len() + 3 <= 94 {
        out.extend([32 + (data.len() + 3) as u8, 32 + seq, kind]);
    } else {
        let n = data.len() + 1;
        out.extend([32, 32 + seq, kind, 32 + (n / 95) as u8, 32 + (n % 95) as u8]);
        out.push(checksum(&out[1..]));
    }
    out.extend(data);
    out.push(checksum(&out[1..]));
    out.push(13);
    Ok(out)
}
struct Packet {
    seq: u8,
    kind: u8,
    data: Vec<u8>,
}
fn parse(rx: &mut VecDeque<u8>) -> Result<Option<Packet>> {
    while rx.front().is_some_and(|b| *b != 1) {
        rx.pop_front();
    }
    if rx.len() < 4 {
        return Ok(None);
    }
    let count = rx[1]
        .checked_sub(32)
        .ok_or_else(|| protocol("invalid packet length"))? as usize;
    let (start, total) = if count == 0 {
        if rx.len() < 7 {
            return Ok(None);
        }
        if !(32..=126).contains(&rx[4]) || !(32..=126).contains(&rx[5]) {
            return Err(protocol("invalid extended length"));
        }
        let n = (rx[4] - 32) as usize * 95 + (rx[5] - 32) as usize;
        if n == 0 || n > 8192 {
            return Err(protocol("invalid extended packet size"));
        }
        let h: Vec<_> = rx.iter().skip(1).take(5).copied().collect();
        if checksum(&h) != rx[6] {
            return Err(protocol("header checksum mismatch"));
        }
        (7, n + 8)
    } else {
        if !(3..=94).contains(&count) {
            return Err(protocol("short packet length"));
        }
        (4, count + 3)
    };
    if rx.len() < total {
        return Ok(None);
    }
    let data: Vec<_> = rx.drain(..total).collect();
    if data[total - 1] != 13 || checksum(&data[1..total - 2]) != data[total - 2] {
        return Err(protocol("packet checksum mismatch"));
    }
    let seq = data[2]
        .checked_sub(32)
        .filter(|s| *s < 64)
        .ok_or_else(|| protocol("invalid packet sequence"))?;
    Ok(Some(Packet {
        seq,
        kind: data[3],
        data: data[start..total - 2].to_vec(),
    }))
}
fn quote(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for &b in bytes {
        let low = b & 0x7f;
        if low < 32 || low == 127 {
            out.extend([b'#', b ^ 64]);
        } else if b == b'#' || b == b'~' {
            out.extend([b'#', b]);
        } else {
            out.push(b);
        }
    }
    out
}
fn unquote(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < bytes.len() {
        let mut count = 1;
        if bytes[pos] == b'~' {
            pos += 1;
            count = bytes
                .get(pos)
                .and_then(|b| b.checked_sub(32))
                .filter(|n| *n > 0)
                .ok_or_else(|| protocol("invalid repeat prefix"))? as usize;
            pos += 1;
        }
        let mut b = *bytes
            .get(pos)
            .ok_or_else(|| protocol("truncated quoted data"))?;
        pos += 1;
        if b == b'#' {
            b = *bytes
                .get(pos)
                .ok_or_else(|| protocol("truncated control quote"))?;
            pos += 1;
            if (63..=95).contains(&(b & 0x7f)) {
                b ^= 64;
            }
        }
        out.extend(std::iter::repeat_n(b, count));
    }
    Ok(out)
}
fn attribute_size(bytes: &[u8]) -> Option<usize> {
    let mut p = 0;
    while p + 2 <= bytes.len() {
        let tag = bytes[p];
        let n = bytes[p + 1].checked_sub(32)? as usize;
        p += 2;
        let value = bytes.get(p..p + n)?;
        if tag == b'1' {
            return std::str::from_utf8(value).ok()?.parse().ok();
        }
        p += n;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_send_init_checksum_matches_observed_wire_packet() {
        assert_eq!(
            frame(0, b'S', b"~' @-#Y1~.\"5M").unwrap(),
            b"\x010 S~' @-#Y1~.\"5M5\r"
        );
    }

    #[test]
    fn split_extended_packets_and_all_binary_values_round_trip() {
        let binary: Vec<_> = (0..4096).map(|n| n as u8).collect();
        let encoded = quote(&binary);
        let wire = frame(63, b'D', &encoded).unwrap();
        let mut rx = VecDeque::new();
        for &b in &wire[..wire.len() - 1] {
            rx.push_back(b);
            assert!(parse(&mut rx).unwrap().is_none());
        }
        rx.push_back(13);
        let parsed = parse(&mut rx).unwrap().unwrap();
        assert_eq!(parsed.seq, 63);
        assert_eq!(unquote(&parsed.data).unwrap(), binary);
        assert!(rx.is_empty());
    }

    #[test]
    fn compressed_quotes_and_malformed_packets_are_checked() {
        assert_eq!(
            unquote(b"~%#@##~##~").unwrap(),
            [0, 0, 0, 0, 0, b'#', b'~', b'~', b'~']
        );
        assert!(unquote(b"#").is_err());
        assert!(unquote(b"~ ").is_err());
        let mut wire = frame(1, b'D', b"payload").unwrap();
        wire[4] ^= 1;
        assert!(parse(&mut wire.into()).is_err());
        assert!(frame(64, b'D', b"").is_err());
        assert!(frame(0, b'D', &[0; 8192]).is_err());
    }

    #[test]
    fn coalesced_acknowledgment_and_send_init_remain_separate() {
        let mut wire = frame(5, b'Y', b"").unwrap();
        wire.extend(frame(0, b'S', INIT).unwrap());
        let mut rx = wire.into();
        assert_eq!(parse(&mut rx).unwrap().unwrap().kind, b'Y');
        assert_eq!(parse(&mut rx).unwrap().unwrap().kind, b'S');
    }
}
