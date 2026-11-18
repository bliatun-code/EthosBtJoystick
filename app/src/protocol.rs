//! FrSky Bluetooth trainer frame decoder.
//!
//! The radio (Model -> Trainer -> Bluetooth -> Slave) sends its channels as
//! notifications on GATT characteristic 0xFFF6 of service 0xFFF0. The byte
//! stream is framed like this (same format as OpenTX/EdgeTX `bluetooth.cpp`):
//!
//! ```text
//!   0x7E  <body, byte-stuffed>  0x7E
//!   body = 0x80, 3 bytes per channel pair ..., XOR checksum
//! ```
//!
//! * `0x7D` escapes the next byte, which is XORed with `0x20`.
//! * The XOR of the whole (unstuffed) body is zero.
//! * Each 3-byte group packs two 12 bit channel values (pulse width in µs,
//!   1500 = centre, roughly 988..2012).
//! * 8 channels -> 14 byte body, 16 channels -> 26 byte body.

pub const MAX_CHANNELS: usize = 32;

const START_STOP: u8 = 0x7E;
const BYTE_STUFF: u8 = 0x7D;
const STUFF_MASK: u8 = 0x20;
const TRAINER_FRAME: u8 = 0x80;
const MAX_BODY: usize = 2 + 3 * (MAX_CHANNELS / 2);

/// One decoded trainer frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame {
    /// Pulse widths in microseconds. Only the first `count` entries are valid.
    pub channels: [u16; MAX_CHANNELS],
    pub count: usize,
}

impl Frame {
    pub fn channels(&self) -> &[u16] {
        &self.channels[..self.count]
    }
}

/// Streaming decoder. Feed it the notification payloads in order; frames may be
/// split across notifications or several frames may arrive in one.
#[derive(Default)]
pub struct FrameDecoder {
    body: Vec<u8>,
    in_frame: bool,
    escape: bool,
    overflow: bool,
    /// Number of frames that were dropped (bad checksum, bad length ...).
    pub bad_frames: u64,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, data: &[u8], mut on_frame: impl FnMut(&Frame)) {
        for &byte in data {
            if byte == START_STOP {
                if self.in_frame && !self.body.is_empty() {
                    match (!self.overflow).then(|| parse_body(&self.body, self.escape)).flatten() {
                        Some(frame) => on_frame(&frame),
                        None => self.bad_frames += 1,
                    }
                }
                self.body.clear();
                self.in_frame = true;
                self.escape = false;
                self.overflow = false;
                continue;
            }

            if !self.in_frame {
                continue;
            }

            let byte = if self.escape {
                self.escape = false;
                byte ^ STUFF_MASK
            } else if byte == BYTE_STUFF {
                self.escape = true;
                continue;
            } else {
                byte
            };

            if self.body.len() < MAX_BODY {
                self.body.push(byte);
            } else {
                self.overflow = true;
            }
        }
    }
}

/// `pending_escape` is true when the frame ended right after a `0x7D`.
fn parse_body(body: &[u8], pending_escape: bool) -> Option<Frame> {
    if body.len() < 4 || body[0] != TRAINER_FRAME {
        return None;
    }
    let xor = body.iter().fold(0u8, |acc, b| acc ^ b);

    // Regular frame: 0x80, data, checksum.
    let data = if !pending_escape && (body.len() - 2) % 3 == 0 && xor == 0 {
        &body[1..body.len() - 1]
    }
    // Tolerate senders that do not byte-stuff the checksum itself (OpenTX does
    // not): a checksum of 0x7E then looks like the end of the frame, and a
    // checksum of 0x7D looks like a dangling escape. In both cases the body we
    // collected is "0x80, data" and its XOR equals the missing checksum.
    else if (body.len() - 1) % 3 == 0
        && ((!pending_escape && xor == START_STOP) || (pending_escape && xor == BYTE_STUFF))
    {
        &body[1..]
    } else {
        return None;
    };

    let pairs = data.len() / 3;
    if pairs == 0 || pairs * 2 > MAX_CHANNELS {
        return None;
    }

    let mut frame = Frame { channels: [0; MAX_CHANNELS], count: pairs * 2 };
    for (pair, d) in data.chunks_exact(3).enumerate() {
        let (b0, b1, b2) = (d[0] as u16, d[1] as u16, d[2] as u16);
        frame.channels[pair * 2] = b0 | ((b1 & 0xF0) << 4);
        frame.channels[pair * 2 + 1] = ((b1 & 0x0F) << 4) | ((b2 & 0xF0) >> 4) | ((b2 & 0x0F) << 8);
    }
    Some(frame)
}

/// Encodes channels into a trainer frame (used by the demo mode and the tests).
/// `channels.len()` must be even.
pub fn encode_frame(channels: &[u16]) -> Vec<u8> {
    assert!(channels.len() % 2 == 0 && channels.len() <= MAX_CHANNELS);

    let mut body = vec![TRAINER_FRAME];
    for pair in channels.chunks_exact(2) {
        let (v1, v2) = (pair[0] & 0x0FFF, pair[1] & 0x0FFF);
        body.push((v1 & 0x00FF) as u8);
        body.push((((v1 & 0x0F00) >> 4) | ((v2 & 0x00F0) >> 4)) as u8);
        body.push((((v2 & 0x000F) << 4) | ((v2 & 0x0F00) >> 8)) as u8);
    }
    let xor = body.iter().fold(0u8, |acc, b| acc ^ b);
    body.push(xor);

    let mut out = vec![START_STOP];
    for byte in body {
        if byte == START_STOP || byte == BYTE_STUFF {
            out.push(BYTE_STUFF);
            out.push(byte ^ STUFF_MASK);
        } else {
            out.push(byte);
        }
    }
    out.push(START_STOP);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_all(dec: &mut FrameDecoder, data: &[u8]) -> Vec<Frame> {
        let mut frames = Vec::new();
        dec.push(data, |f| frames.push(*f));
        frames
    }

    #[test]
    fn roundtrip_8_channels() {
        let ch = [988, 1500, 2012, 1234, 1777, 1000, 2000, 1501];
        let mut dec = FrameDecoder::new();
        let frames = decode_all(&mut dec, &encode_frame(&ch));
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].channels(), &ch);
        assert_eq!(dec.bad_frames, 0);
    }

    #[test]
    fn roundtrip_16_channels() {
        let ch: Vec<u16> = (0..16).map(|i| 988 + i * 64).collect();
        let mut dec = FrameDecoder::new();
        let frames = decode_all(&mut dec, &encode_frame(&ch));
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].channels(), &ch[..]);
    }

    #[test]
    fn opentx_reference_frame() {
        // Hand-packed exactly like OpenTX Bluetooth::sendTrainer for
        // CH1 = 1500 (0x5DC), CH2 = 1000 (0x3E8), remaining pairs identical.
        // byte0 = 0xDC, byte1 = 0x50 | 0x0E = 0x5E, byte2 = 0x80 | 0x03 = 0x83
        let mut body = vec![0x80u8];
        for _ in 0..4 {
            body.extend_from_slice(&[0xDC, 0x5E, 0x83]);
        }
        let xor = body.iter().fold(0u8, |a, b| a ^ b);
        let mut raw = vec![0x7E];
        raw.extend_from_slice(&body);
        raw.push(xor);
        raw.push(0x7E);

        let mut dec = FrameDecoder::new();
        let frames = decode_all(&mut dec, &raw);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].channels(), &[1500, 1000, 1500, 1000, 1500, 1000, 1500, 1000]);
    }

    #[test]
    fn byte_stuffing_and_fragmentation() {
        // 0x7E and 0x7D as low bytes force stuffing.
        let ch = [0x57E, 0x47D, 1500, 1500, 0x67D, 0x57E, 1500, 1500];
        let raw = encode_frame(&ch);
        assert!(raw.len() > 16, "expected stuffed bytes in the frame");

        // feed one byte at a time, twice in a row
        let mut dec = FrameDecoder::new();
        let mut frames = Vec::new();
        for _ in 0..2 {
            for b in &raw {
                dec.push(&[*b], |f| frames.push(*f));
            }
        }
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].channels(), &ch);
        assert_eq!(frames[1].channels(), &ch);
        assert_eq!(dec.bad_frames, 0);
    }

    #[test]
    fn several_frames_in_one_notification_and_shared_delimiter() {
        let a = encode_frame(&[1000, 1100, 1200, 1300, 1400, 1500, 1600, 1700]);
        let b = encode_frame(&[2000, 1900, 1800, 1700, 1600, 1500, 1400, 1300]);
        let mut dec = FrameDecoder::new();

        let mut joined = a.clone();
        joined.extend_from_slice(&b);
        assert_eq!(decode_all(&mut dec, &joined).len(), 2);

        // only one 0x7E between the frames
        let mut shared = a.clone();
        shared.extend_from_slice(&b[1..]);
        assert_eq!(decode_all(&mut dec, &shared).len(), 2);
        assert_eq!(dec.bad_frames, 0);
    }

    #[test]
    fn bad_checksum_is_rejected_and_decoder_recovers() {
        let good = encode_frame(&[1500; 8]);
        let mut bad = good.clone();
        bad[3] ^= 0x01;

        let mut dec = FrameDecoder::new();
        assert!(decode_all(&mut dec, &bad).is_empty());
        assert_eq!(dec.bad_frames, 1);
        assert_eq!(decode_all(&mut dec, &good).len(), 1);
    }

    #[test]
    fn garbage_before_first_frame_is_ignored() {
        let mut raw = vec![0x00, 0x41, 0x54, 0x2B, 0x0D, 0x0A];
        raw.extend_from_slice(&encode_frame(&[1500; 8]));
        let mut dec = FrameDecoder::new();
        assert_eq!(decode_all(&mut dec, &raw).len(), 1);
        assert_eq!(dec.bad_frames, 0);
    }

    #[test]
    fn unstuffed_checksum_quirk() {
        // Find channel values whose checksum is 0x7E / 0x7D and send the
        // checksum raw (like OpenTX does).
        for target in [0x7Eu8, 0x7D] {
            let mut found = false;
            for v in 988u16..2012 {
                let ch = [v, 1500, 1500, 1500, 1500, 1500, 1500, 1500];
                let mut body = vec![0x80u8];
                for pair in ch.chunks_exact(2) {
                    let (v1, v2) = (pair[0], pair[1]);
                    body.push((v1 & 0xFF) as u8);
                    body.push((((v1 & 0x0F00) >> 4) | ((v2 & 0x00F0) >> 4)) as u8);
                    body.push((((v2 & 0x000F) << 4) | ((v2 & 0x0F00) >> 8)) as u8);
                }
                let xor = body.iter().fold(0u8, |a, b| a ^ b);
                if xor != target || body.iter().any(|b| *b == 0x7E || *b == 0x7D) {
                    continue;
                }
                let mut raw = vec![0x7E];
                raw.extend_from_slice(&body);
                raw.push(xor); // not stuffed
                raw.push(0x7E);

                let mut dec = FrameDecoder::new();
                let frames = decode_all(&mut dec, &raw);
                assert_eq!(frames.len(), 1, "checksum {target:#x}");
                assert_eq!(frames[0].channels(), &ch);
                found = true;
                break;
            }
            assert!(found, "no test vector for checksum {target:#x}");
        }
    }

    #[test]
    fn oversized_frame_is_dropped() {
        let mut raw = vec![0x7E];
        raw.extend(std::iter::repeat(0x11).take(200));
        raw.push(0x7E);
        let mut dec = FrameDecoder::new();
        assert!(decode_all(&mut dec, &raw).is_empty());
        assert_eq!(dec.bad_frames, 1);
    }
}
