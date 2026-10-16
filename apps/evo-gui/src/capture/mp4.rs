//! MP4 (H.264) recording: the screen scaled ×2, one sample per distinct screen, each lasting
//! as many 1/60 s ticks as it stayed on screen.

use std::fs::File;
use std::io::BufWriter;
use std::path::PathBuf;

use ::mp4::{AvcConfig, MediaConfig, Mp4Config, Mp4Sample, Mp4Writer, TrackConfig, TrackType};
use openh264::OpenH264API;
use openh264::Timestamp;
use openh264::encoder::{BitRate, Encoder, EncoderConfig, FrameRate, FrameType};
use openh264::formats::{RgbSliceU8, YUVBuffer};

use super::CaptureError;
use crate::backend::Frame;

/// Ticks per second: the track timescale, so a sample's duration is its tick count.
const TICKS_PER_SECOND: u32 = 60;
/// H.264 NAL unit types the MP4 carries elsewhere (SPS, PPS in `avcC`) or not at all (AUD).
const NAL_SPS: u8 = 7;
const NAL_PPS: u8 = 8;
const NAL_AUD: u8 = 9;

/// The screen at twice its size as RGB888 (alpha dropped), nearest neighbour. The size is
/// rounded down to even, as H.264's 4:2:0 needs; doubling already makes it even. An invalid
/// frame gives `(0, 0, [])`.
pub fn scale2x_rgb(frame: &Frame) -> (u32, u32, Vec<u8>) {
    if !frame.is_valid() {
        return (0, 0, Vec::new());
    }
    let (w, h) = (frame.width as usize, frame.height as usize);
    let (w2, h2) = ((w * 2) & !1, (h * 2) & !1);
    let mut rgb = Vec::with_capacity(w2 * h2 * 3);
    for y in 0..h2 {
        let row = &frame.rgba[(y / 2) * w * 4..][..w * 4];
        for x in 0..w2 {
            rgb.extend_from_slice(&row[(x / 2) * 4..][..3]);
        }
    }
    (w2 as u32, h2 as u32, rgb)
}

/// The encoded picture waiting to be written: its duration grows while the screen stays.
struct Pending {
    avcc: Vec<u8>,
    sync: bool,
    ticks: u64,
}

/// The encoder and the file, created at the first frame.
struct Stream {
    encoder: Encoder,
    writer: Mp4Writer<BufWriter<File>>,
    pending: Pending,
    /// Ticks written so far: the next sample's start time.
    written: u64,
}

/// Records distinct frames into an H.264 MP4.
///
/// The file is created at the first frame and finished (its `moov` index written) at
/// [`finish`](Self::finish). A write error makes the file unusable: from then on every push
/// fails, and `finish` removes the file and reports the error.
pub struct Mp4Recorder {
    path: PathBuf,
    size: Option<(u32, u32)>,
    last: Vec<u8>,
    /// Ticks pushed so far: the encoder's clock.
    ticks: u64,
    stream: Option<Stream>,
    /// The write error that broke the file, if any.
    failed: Option<String>,
}

impl Mp4Recorder {
    /// A recorder that writes `path`.
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            size: None,
            last: Vec::new(),
            ticks: 0,
            stream: None,
            failed: None,
        }
    }

    /// Adds `frame`, shown for `ticks` 1/60 s ticks. A frame identical to the last one adds
    /// its ticks to that one. An invalid frame, one of another size than the first, or one
    /// the encoder refuses is refused with [`CaptureError::Encode`] and leaves the recording
    /// as it was; a write error ([`CaptureError::Io`]) ends it.
    pub fn push(&mut self, frame: &Frame, ticks: u32) -> Result<(), CaptureError> {
        if let Some(e) = &self.failed {
            return Err(CaptureError::Io(e.clone()));
        }
        if !frame.is_valid() {
            return Err(CaptureError::Encode("invalid frame".to_owned()));
        }
        let size = (frame.width, frame.height);
        if let Some(first) = self.size
            && first != size
        {
            return Err(CaptureError::Encode(format!(
                "frame size changed from {}×{} to {}×{}",
                first.0, first.1, size.0, size.1
            )));
        }
        let ticks = u64::from(ticks);
        if let Some(stream) = self.stream.as_mut()
            && self.last == frame.rgba
        {
            stream.pending.ticks = stream.pending.ticks.saturating_add(ticks);
            self.ticks = self.ticks.saturating_add(ticks);
            return Ok(());
        }
        let at = Timestamp::from_millis(self.ticks.saturating_mul(1000) / 60);
        match self.stream.as_mut() {
            None => self.start(frame, at, ticks)?,
            Some(stream) => {
                let nals = encode(&mut stream.encoder, frame, at)?;
                let avcc = avcc(&nals.slices);
                if avcc.is_empty() {
                    // Nothing new to show: the picture on screen lasts longer.
                    stream.pending.ticks = stream.pending.ticks.saturating_add(ticks);
                } else {
                    let next = Pending {
                        avcc,
                        sync: nals.sync,
                        ticks,
                    };
                    let done = std::mem::replace(&mut stream.pending, next);
                    if let Err(e) = write_sample(stream, done) {
                        return Err(self.fail(e));
                    }
                }
            }
        }
        self.size = Some(size);
        self.last.clear();
        self.last.extend_from_slice(&frame.rgba);
        self.ticks = self.ticks.saturating_add(ticks);
        Ok(())
    }

    /// Encodes the first frame, then creates the file with the track its SPS/PPS describe.
    fn start(&mut self, frame: &Frame, at: Timestamp, ticks: u64) -> Result<(), CaptureError> {
        let config = EncoderConfig::new()
            .skip_frames(false)
            .bitrate(BitRate::from_bps(2_000_000))
            .max_frame_rate(FrameRate::from_hz(60.0));
        let mut encoder = Encoder::with_api_config(OpenH264API::from_source(), config)
            .map_err(|e| CaptureError::Encode(e.to_string()))?;
        // The first picture after initialisation is an IDR; should the encoder ever differ, it
        // is asked for one (it ignores the request before its first picture).
        let mut nals = encode(&mut encoder, frame, at)?;
        if !nals.sync {
            encoder.force_intra_frame();
            nals = encode(&mut encoder, frame, at)?;
        }
        // `avcC` copies the profile and level out of the SPS's first bytes.
        let (Some(sps), Some(pps)) = (nals.sps.filter(|s| s.len() >= 4), nals.pps) else {
            return Err(CaptureError::Encode(
                "the encoder gave no sequence header".to_owned(),
            ));
        };
        let avcc = avcc(&nals.slices);
        if avcc.is_empty() {
            return Err(CaptureError::Encode(
                "the encoder gave no picture".to_owned(),
            ));
        }
        let too_big = || CaptureError::Encode("frame too large".to_owned());
        let width = u16::try_from(frame.width.saturating_mul(2)).map_err(|_| too_big())?;
        let height = u16::try_from(frame.height.saturating_mul(2)).map_err(|_| too_big())?;
        let writer = match open_writer(&self.path, width, height, sps, pps) {
            Ok(w) => w,
            Err(e) => return Err(self.fail(e)),
        };
        self.stream = Some(Stream {
            encoder,
            writer,
            pending: Pending {
                avcc,
                sync: nals.sync,
                ticks,
            },
            written: 0,
        });
        Ok(())
    }

    /// Marks the file broken by `e` and removes it.
    fn fail(&mut self, e: CaptureError) -> CaptureError {
        self.stream = None;
        let _ = std::fs::remove_file(&self.path);
        let msg = e.to_string();
        self.failed = Some(msg.clone());
        CaptureError::Io(msg)
    }

    /// Writes the last sample and the index, and returns the path; `None` (and no file) when
    /// no frame was pushed. On an error the partial file is removed.
    pub fn finish(mut self) -> Result<Option<PathBuf>, CaptureError> {
        if let Some(e) = self.failed.take() {
            let _ = std::fs::remove_file(&self.path);
            return Err(CaptureError::Io(e));
        }
        let Some(stream) = self.stream.take() else {
            return Ok(None);
        };
        match end(stream) {
            Ok(()) => Ok(Some(self.path)),
            Err(e) => {
                let _ = std::fs::remove_file(&self.path);
                Err(e)
            }
        }
    }
}

/// The NAL units of one encoded picture, without start codes.
struct Nals {
    sps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
    /// Everything but SPS, PPS and AUD.
    slices: Vec<Vec<u8>>,
    sync: bool,
}

/// Encodes `frame` at twice its size.
fn encode(encoder: &mut Encoder, frame: &Frame, at: Timestamp) -> Result<Nals, CaptureError> {
    let (w, h, rgb) = scale2x_rgb(frame);
    let yuv = YUVBuffer::from_rgb8_source(RgbSliceU8::new(&rgb, (w as usize, h as usize)));
    let bits = encoder
        .encode_at(&yuv, at)
        .map_err(|e| CaptureError::Encode(e.to_string()))?;
    let mut nals = Nals {
        sps: None,
        pps: None,
        slices: Vec::new(),
        sync: matches!(bits.frame_type(), FrameType::IDR | FrameType::I),
    };
    for l in 0..bits.num_layers() {
        let Some(layer) = bits.layer(l) else { continue };
        for n in 0..layer.nal_count() {
            let Some(nal) = layer.nal_unit(n).map(strip_start_code) else {
                continue;
            };
            let Some(&header) = nal.first() else { continue };
            match header & 0x1F {
                NAL_SPS => nals.sps = Some(nal.to_vec()),
                NAL_PPS => nals.pps = Some(nal.to_vec()),
                NAL_AUD => {}
                _ => nals.slices.push(nal.to_vec()),
            }
        }
    }
    Ok(nals)
}

/// A NAL unit without its Annex B start code (`00 00 01` or `00 00 00 01`).
fn strip_start_code(nal: &[u8]) -> &[u8] {
    let zeros = nal.iter().take_while(|&&b| b == 0).count();
    if zeros >= 2 && nal.get(zeros) == Some(&1) {
        &nal[zeros + 1..]
    } else {
        nal
    }
}

/// NAL units as one AVCC sample: each prefixed by its 4-byte big-endian length.
fn avcc(nals: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nals.iter().map(|n| n.len() + 4).sum());
    for nal in nals {
        let len = u32::try_from(nal.len()).unwrap_or(u32::MAX);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(nal);
    }
    out
}

/// Creates the file and starts the MP4 with its one video track.
fn open_writer(
    path: &std::path::Path,
    width: u16,
    height: u16,
    sps: Vec<u8>,
    pps: Vec<u8>,
) -> Result<Mp4Writer<BufWriter<File>>, CaptureError> {
    let fourcc = |s: &str| s.parse().map_err(mp4_error);
    let config = Mp4Config {
        major_brand: fourcc("isom")?,
        minor_version: 512,
        compatible_brands: vec![
            fourcc("isom")?,
            fourcc("iso2")?,
            fourcc("avc1")?,
            fourcc("mp41")?,
        ],
        timescale: 1000,
    };
    let file = BufWriter::new(File::create(path)?);
    let mut writer = Mp4Writer::write_start(file, &config).map_err(mp4_error)?;
    let track = TrackConfig {
        track_type: TrackType::Video,
        timescale: TICKS_PER_SECOND,
        language: "und".into(),
        media_conf: MediaConfig::AvcConfig(AvcConfig {
            width,
            height,
            seq_param_set: sps,
            pic_param_set: pps,
        }),
    };
    writer.add_track(&track).map_err(mp4_error)?;
    Ok(writer)
}

/// Writes `sample` to the track.
fn write_sample(stream: &mut Stream, sample: Pending) -> Result<(), CaptureError> {
    let duration = u32::try_from(sample.ticks).unwrap_or(u32::MAX);
    let s = Mp4Sample {
        start_time: stream.written,
        duration,
        rendering_offset: 0,
        is_sync: sample.sync,
        bytes: sample.avcc.into(),
    };
    stream.writer.write_sample(1, &s).map_err(mp4_error)?;
    stream.written = stream.written.saturating_add(u64::from(duration));
    Ok(())
}

/// Writes the pending sample and the index, then flushes the file.
fn end(mut stream: Stream) -> Result<(), CaptureError> {
    let last = std::mem::replace(
        &mut stream.pending,
        Pending {
            avcc: Vec::new(),
            sync: false,
            ticks: 0,
        },
    );
    write_sample(&mut stream, last)?;
    stream.writer.write_end().map_err(mp4_error)?;
    let file = stream
        .writer
        .into_writer()
        .into_inner()
        .map_err(|e| CaptureError::Io(e.error().to_string()))?;
    drop(file);
    Ok(())
}

fn mp4_error(e: ::mp4::Error) -> CaptureError {
    match e {
        ::mp4::Error::IoError(e) => CaptureError::Io(e.to_string()),
        e => CaptureError::Encode(e.to_string()),
    }
}
