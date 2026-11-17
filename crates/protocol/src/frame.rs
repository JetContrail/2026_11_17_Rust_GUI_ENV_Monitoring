//! Card-reader frame format.
//!
//! Real reader protocols (SS2269VHC-Lite, SS2279VHC-RS-FS, ...) are not public; adapters
//! translate them into this format.
//!
//! ```text
//! STX(0x02) | LEN | TYPE | READER (u32, BE) | DATA | BCC | ETX(0x03)
//! ```
//!
//! `LEN` counts `TYPE`, `READER` and `DATA`. `BCC` is the XOR of `LEN` through the last
//! `DATA` byte.

use envmon_core::model::{CardId, ReaderId};

pub const STX: u8 = 0x02;
pub const ETX: u8 = 0x03;

/// `TYPE` + `READER`.
const HEADER_LEN: usize = 5;
/// Bytes outside `LEN`: `STX`, `LEN`, `BCC`, `ETX`.
const ENVELOPE_LEN: usize = 4;
/// Undecoded bytes kept by [`FrameDecoder`] before old data is discarded.
const MAX_BUFFER: usize = 4 * 1024;

mod kind {
    pub const CARD_READ: u8 = 0x10;
    pub const TAMPER: u8 = 0x20;
    pub const HEARTBEAT: u8 = 0x30;
    pub const FEEDBACK: u8 = 0x80;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReaderFrame {
    /// Reader → host: a card was presented.
    CardRead {
        reader: ReaderId,
        card: CardId,
    },
    /// Reader → host: the reader housing was opened.
    Tamper {
        reader: ReaderId,
    },
    Heartbeat {
        reader: ReaderId,
    },
    /// Host → reader: LED / buzzer result of the access decision.
    Feedback {
        reader: ReaderId,
        granted: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    #[error("frame does not start with STX")]
    MissingStx,
    #[error("frame is incomplete")]
    Incomplete,
    #[error("invalid frame length {0}")]
    BadLength(u8),
    #[error("checksum mismatch (expected {expected:#04x}, got {actual:#04x})")]
    Checksum { expected: u8, actual: u8 },
    #[error("frame does not end with ETX")]
    MissingEtx,
    #[error("unknown frame type {0:#04x}")]
    UnknownType(u8),
    #[error("invalid payload for frame type {0:#04x}")]
    BadPayload(u8),
    #[error("trailing bytes after the frame")]
    Trailing,
}

impl ReaderFrame {
    pub fn reader(&self) -> ReaderId {
        match self {
            Self::CardRead { reader, .. }
            | Self::Tamper { reader }
            | Self::Heartbeat { reader }
            | Self::Feedback { reader, .. } => *reader,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let (kind, data): (u8, &[u8]) = match self {
            Self::CardRead { card, .. } => (kind::CARD_READ, card.as_bytes()),
            Self::Tamper { .. } => (kind::TAMPER, &[]),
            Self::Heartbeat { .. } => (kind::HEARTBEAT, &[]),
            Self::Feedback { granted, .. } => (kind::FEEDBACK, if *granted { &[1] } else { &[0] }),
        };
        // Card IDs are at most 16 bytes, so LEN always fits in a byte.
        let len = u8::try_from(HEADER_LEN + data.len()).unwrap_or(u8::MAX);
        let mut out = Vec::with_capacity(usize::from(len) + ENVELOPE_LEN);
        out.extend_from_slice(&[STX, len, kind]);
        out.extend_from_slice(&self.reader().0.to_be_bytes());
        out.extend_from_slice(data);
        out.push(bcc(&out[1..]));
        out.push(ETX);
        out
    }

    /// Parses exactly one frame.
    pub fn parse(bytes: &[u8]) -> Result<Self, FrameError> {
        let (frame, used) = parse_prefix(bytes)?;
        if used == bytes.len() {
            Ok(frame)
        } else {
            Err(FrameError::Trailing)
        }
    }
}

fn bcc(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0, |acc, b| acc ^ b)
}

/// Parses the frame at the start of `b` and returns it with the number of bytes used.
fn parse_prefix(b: &[u8]) -> Result<(ReaderFrame, usize), FrameError> {
    match b.first() {
        None => return Err(FrameError::Incomplete),
        Some(&STX) => {}
        Some(_) => return Err(FrameError::MissingStx),
    }
    let len = *b.get(1).ok_or(FrameError::Incomplete)?;
    let body_len = usize::from(len);
    if body_len < HEADER_LEN {
        return Err(FrameError::BadLength(len));
    }
    let total = body_len + ENVELOPE_LEN;
    if b.len() < total {
        return Err(FrameError::Incomplete);
    }
    let body = &b[2..2 + body_len];
    let expected = bcc(&b[1..2 + body_len]);
    let actual = b[2 + body_len];
    if expected != actual {
        return Err(FrameError::Checksum { expected, actual });
    }
    if b[total - 1] != ETX {
        return Err(FrameError::MissingEtx);
    }
    let kind = body[0];
    let reader = ReaderId(u32::from_be_bytes([body[1], body[2], body[3], body[4]]));
    let data = &body[HEADER_LEN..];
    let frame = match (kind, data) {
        (kind::CARD_READ, card) => ReaderFrame::CardRead {
            reader,
            card: CardId::from_bytes(card).map_err(|_| FrameError::BadPayload(kind))?,
        },
        (kind::TAMPER, []) => ReaderFrame::Tamper { reader },
        (kind::HEARTBEAT, []) => ReaderFrame::Heartbeat { reader },
        (kind::FEEDBACK, [g @ (0 | 1)]) => ReaderFrame::Feedback {
            reader,
            granted: *g == 1,
        },
        (kind::TAMPER | kind::HEARTBEAT | kind::FEEDBACK, _) => {
            return Err(FrameError::BadPayload(kind));
        }
        (other, _) => return Err(FrameError::UnknownType(other)),
    };
    Ok((frame, total))
}

/// Incremental decoder for a byte stream from a reader (TCP segments may split or join
/// frames). Garbage before `STX` is skipped; a corrupt frame is reported once and the decoder
/// resynchronises on the next `STX`.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buf: Vec<u8>,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
        if self.buf.len() > MAX_BUFFER {
            let excess = self.buf.len() - MAX_BUFFER;
            self.buf.drain(..excess);
        }
    }

    /// Returns the next frame or error, or `None` when more bytes are needed.
    pub fn next_frame(&mut self) -> Option<Result<ReaderFrame, FrameError>> {
        let Some(start) = self.buf.iter().position(|&b| b == STX) else {
            self.buf.clear();
            return None;
        };
        self.buf.drain(..start);
        match parse_prefix(&self.buf) {
            Ok((frame, used)) => {
                self.buf.drain(..used);
                Some(Ok(frame))
            }
            Err(FrameError::Incomplete) => None,
            Err(e) => {
                self.buf.drain(..1);
                Some(Err(e))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card_frame() -> ReaderFrame {
        ReaderFrame::CardRead {
            reader: ReaderId(0x0102_0304),
            card: "0123456789ABCDEF".parse().unwrap(),
        }
    }

    #[test]
    fn encodes_documented_layout() {
        let bytes = card_frame().encode();
        assert_eq!(
            bytes,
            [
                0x02, 13, 0x10, 0x01, 0x02, 0x03, 0x04, 0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD,
                0xEF, 0x19, 0x03
            ]
        );
        assert_eq!(bcc(&bytes[1..15]), bytes[15]);
    }

    #[test]
    fn round_trips_every_frame_type() {
        let r = ReaderId(7);
        for frame in [
            card_frame(),
            ReaderFrame::Tamper { reader: r },
            ReaderFrame::Heartbeat { reader: r },
            ReaderFrame::Feedback {
                reader: r,
                granted: true,
            },
            ReaderFrame::Feedback {
                reader: r,
                granted: false,
            },
        ] {
            assert_eq!(ReaderFrame::parse(&frame.encode()), Ok(frame));
        }
    }

    #[test]
    fn rejects_malformed_frames() {
        let good = card_frame().encode();
        let mut bad_bcc = good.clone();
        bad_bcc[8] ^= 0xFF;
        assert!(matches!(
            ReaderFrame::parse(&bad_bcc),
            Err(FrameError::Checksum { .. })
        ));

        let mut no_etx = good.clone();
        *no_etx.last_mut().unwrap() = 0x00;
        assert_eq!(ReaderFrame::parse(&no_etx), Err(FrameError::MissingEtx));

        assert_eq!(ReaderFrame::parse(&good[..10]), Err(FrameError::Incomplete));
        assert_eq!(ReaderFrame::parse(&good[1..]), Err(FrameError::MissingStx));
        assert_eq!(
            ReaderFrame::parse(b"{\"card\":1}"),
            Err(FrameError::MissingStx)
        );

        let mut trailing = good.clone();
        trailing.push(0);
        assert_eq!(ReaderFrame::parse(&trailing), Err(FrameError::Trailing));

        assert_eq!(
            ReaderFrame::parse(&[STX, 2, 0x10, 0, 0x12, ETX]),
            Err(FrameError::BadLength(2))
        );
    }

    fn raw(kind: u8, data: &[u8]) -> Vec<u8> {
        let mut out = vec![
            STX,
            u8::try_from(HEADER_LEN + data.len()).unwrap(),
            kind,
            0,
            0,
            0,
            1,
        ];
        out.extend_from_slice(data);
        out.push(bcc(&out[1..]));
        out.push(ETX);
        out
    }

    #[test]
    fn rejects_unknown_types_and_bad_payloads() {
        assert_eq!(
            ReaderFrame::parse(&raw(0x55, &[])),
            Err(FrameError::UnknownType(0x55))
        );
        assert_eq!(
            ReaderFrame::parse(&raw(0x10, &[])),
            Err(FrameError::BadPayload(0x10))
        );
        assert_eq!(
            ReaderFrame::parse(&raw(0x10, &[0; 17])),
            Err(FrameError::BadPayload(0x10))
        );
        assert_eq!(
            ReaderFrame::parse(&raw(0x80, &[2])),
            Err(FrameError::BadPayload(0x80))
        );
        assert_eq!(
            ReaderFrame::parse(&raw(0x20, &[1])),
            Err(FrameError::BadPayload(0x20))
        );
    }

    #[test]
    fn decoder_handles_split_joined_and_corrupt_input() {
        let a = card_frame();
        let b = ReaderFrame::Heartbeat {
            reader: ReaderId(9),
        };
        let mut corrupt = a.encode();
        corrupt[10] ^= 1;

        let mut stream = vec![0xFF, 0x00];
        stream.extend(a.encode());
        stream.extend(corrupt);
        stream.extend(b.encode());

        let mut dec = FrameDecoder::new();
        let mut out = Vec::new();
        for chunk in stream.chunks(3) {
            dec.push(chunk);
            while let Some(r) = dec.next_frame() {
                out.push(r);
            }
        }
        assert_eq!(out.first(), Some(&Ok(a)));
        assert!(matches!(out.get(1), Some(Err(FrameError::Checksum { .. }))));
        assert_eq!(out.last(), Some(&Ok(b)));
        assert_eq!(out.iter().filter(|r| r.is_ok()).count(), 2);
    }
}
