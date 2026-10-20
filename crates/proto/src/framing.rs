//! NetClient.dll packet framing.
//!
//! The read path (NetClient.dll 0x10002101..0x10002155) loads the first dword
//! of the receive buffer raw, i.e. a little-endian u32 length, and accepts the
//! message once `buffered - 4 >= length`. Lengths above 1,000,000
//! (`cmp esi, 0xF4240` @ 0x1000213D) reset the socket silently, so the encoder
//! refuses to produce them.

use bytes::{Buf, BufMut, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

pub const MAX_FRAME: usize = 1_000_000;

#[derive(Debug, Default)]
pub struct FrameCodec;

impl Decoder for FrameCodec {
    type Item = BytesMut;
    type Error = std::io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<BytesMut>, Self::Error> {
        if src.len() < 4 {
            return Ok(None);
        }
        let len = u32::from_le_bytes(src[..4].try_into().unwrap()) as usize;
        if len > MAX_FRAME {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("frame of {len} bytes exceeds the client's {MAX_FRAME}-byte limit"),
            ));
        }
        if src.len() < 4 + len {
            src.reserve(4 + len - src.len());
            return Ok(None);
        }
        src.advance(4);
        Ok(Some(src.split_to(len)))
    }
}

impl Encoder<Vec<u8>> for FrameCodec {
    type Error = std::io::Error;

    fn encode(&mut self, item: Vec<u8>, dst: &mut BytesMut) -> Result<(), Self::Error> {
        if item.len() > MAX_FRAME {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "refusing a {}-byte frame: the client drops anything over {MAX_FRAME}",
                    item.len()
                ),
            ));
        }
        dst.reserve(4 + item.len());
        dst.put_u32_le(item.len() as u32);
        dst.extend_from_slice(&item);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_roundtrip_and_split() {
        let mut codec = FrameCodec;
        let mut buf = BytesMut::new();
        codec.encode(b"hello".to_vec(), &mut buf).unwrap();
        assert_eq!(&buf[..], b"\x05\x00\x00\x00hello");
        let mut partial = BytesMut::from(&buf[..6]);
        assert!(codec.decode(&mut partial).unwrap().is_none());
        partial.extend_from_slice(&buf[6..]);
        assert_eq!(&codec.decode(&mut partial).unwrap().unwrap()[..], b"hello");
    }

    #[test]
    fn rejects_oversized_frames() {
        let mut buf = BytesMut::from(&(MAX_FRAME as u32 + 1).to_le_bytes()[..]);
        assert!(FrameCodec.decode(&mut buf).is_err());
        assert!(FrameCodec
            .encode(vec![0; MAX_FRAME + 1], &mut BytesMut::new())
            .is_err());
    }
}
