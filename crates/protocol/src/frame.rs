//! Length-prefixed framing: `[kind: u8][len: u32 BE][payload]`.

use std::io;

use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::message::PaneId;

/// Largest accepted payload.
pub const MAX_FRAME_LEN: usize = 64 << 20;

const KIND_CONTROL: u8 = 0;
const KIND_DATA: u8 = 1;

/// One unit on the wire.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    /// A JSON-encoded control message.
    Control(Vec<u8>),
    /// Raw terminal bytes for a pane (output from the daemon, input from the GUI).
    Data {
        /// Pane the bytes belong to.
        pane: PaneId,
        /// The bytes.
        bytes: Vec<u8>,
    },
}

impl Frame {
    /// Encode a control message.
    pub fn control(message: &impl Serialize) -> Frame {
        Frame::Control(serde_json::to_vec(message).expect("control messages always serialize"))
    }

    /// Decode a control frame as `T`; `None` for data frames.
    pub fn parse<T: DeserializeOwned>(&self) -> Option<serde_json::Result<T>> {
        match self {
            Frame::Control(json) => Some(serde_json::from_slice(json)),
            Frame::Data { .. } => None,
        }
    }

    /// Bytes this frame occupies on the wire.
    pub fn encoded_len(&self) -> usize {
        5 + match self {
            Frame::Control(json) => json.len(),
            Frame::Data { bytes, .. } => 8 + bytes.len(),
        }
    }
}

/// Read one frame; `Ok(None)` on a clean end of stream before a frame starts.
pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> io::Result<Option<Frame>> {
    let kind = match reader.read_u8().await {
        Ok(kind) => kind,
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err),
    };
    let len = reader.read_u32().await? as usize;
    if len > MAX_FRAME_LEN {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("frame of {len} bytes exceeds limit")));
    }
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload).await?;
    match kind {
        KIND_CONTROL => Ok(Some(Frame::Control(payload))),
        KIND_DATA if len >= 8 => {
            let pane = u64::from_be_bytes(payload[..8].try_into().expect("8 bytes"));
            Ok(Some(Frame::Data { pane, bytes: payload.split_off(8) }))
        }
        _ => Err(io::Error::new(io::ErrorKind::InvalidData, format!("bad frame kind {kind} / length {len}"))),
    }
}

/// Write one frame and flush.
pub async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, frame: &Frame) -> io::Result<()> {
    match frame {
        Frame::Control(json) => {
            writer.write_u8(KIND_CONTROL).await?;
            writer.write_u32(json.len() as u32).await?;
            writer.write_all(json).await?;
        }
        Frame::Data { pane, bytes } => {
            writer.write_u8(KIND_DATA).await?;
            writer.write_u32((8 + bytes.len()) as u32).await?;
            writer.write_u64(*pane).await?;
            writer.write_all(bytes).await?;
        }
    }
    writer.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::ClientMessage;

    #[tokio::test]
    async fn frames_round_trip_through_a_byte_stream() {
        let frames = vec![
            Frame::control(&ClientMessage::ListPanes),
            Frame::Data { pane: 42, bytes: b"\x1b[31mhi".to_vec() },
            Frame::Data { pane: 1, bytes: Vec::new() },
        ];
        let mut wire = Vec::new();
        for frame in &frames {
            write_frame(&mut wire, frame).await.unwrap();
        }
        let mut reader = wire.as_slice();
        for frame in &frames {
            assert_eq!(read_frame(&mut reader).await.unwrap().as_ref(), Some(frame));
        }
        assert_eq!(read_frame(&mut reader).await.unwrap(), None);
    }

    #[tokio::test]
    async fn control_frames_parse_back() {
        let frame = Frame::control(&ClientMessage::Focus { pane: 5 });
        assert_eq!(frame.parse::<ClientMessage>().unwrap().unwrap(), ClientMessage::Focus { pane: 5 });
        assert!(Frame::Data { pane: 1, bytes: vec![] }.parse::<ClientMessage>().is_none());
    }

    #[tokio::test]
    async fn oversized_and_unknown_frames_are_errors() {
        let mut too_big = vec![0u8];
        too_big.extend_from_slice(&((MAX_FRAME_LEN as u32) + 1).to_be_bytes());
        assert!(read_frame(&mut too_big.as_slice()).await.is_err());
        let unknown = [9u8, 0, 0, 0, 0];
        assert!(read_frame(&mut unknown.as_slice()).await.is_err());
        let truncated = [0u8, 0, 0, 0, 5, b'{'];
        assert!(read_frame(&mut truncated.as_slice()).await.is_err());
    }
}
