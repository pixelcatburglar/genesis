//! machoNet, the build-1000 client's packet protocol (script/net/machoNet.py).
//!
//! Layers, bottom up:
//!   * framing  - NetClient.dll AsyncPacketSocket: u32 LE length + payload
//!   * pickle   - each payload is a blue.marshal stream, zlib-compressed when the
//!                first byte is not '~' (MachoTransport.Read)
//!   * packet   - a `macho.<Kind>` instance whose state is
//!                (command, source, destination, userID, body, oob)
//!   * GPCS     - per-channel payload wrapping (see `gpcs`)

pub mod framing;
pub mod gpcs;
pub mod packet;

pub use framing::FrameCodec;
pub use packet::{Address, Command, Packet};

use marshal::Value;

/// `MachoLoads`: decompress if needed, then unmarshal.
pub fn loads(frame: &[u8]) -> Result<Value, Error> {
    if frame.first() == Some(&marshal::HEADER) {
        return Ok(marshal::decode(frame)?);
    }
    use std::io::Read;
    let mut raw = Vec::new();
    flate2::read::ZlibDecoder::new(frame)
        .read_to_end(&mut raw)
        .map_err(|_| Error::Zlib)?;
    Ok(marshal::decode(&raw)?)
}

/// `MachoDumps` with the client's defaults: packetGuard and packetCRC32 are 0.
/// Compression is optional for the receiver, so the server never compresses.
pub fn dumps(v: &Value) -> Vec<u8> {
    marshal::encode(v)
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Marshal(#[from] marshal::Error),
    #[error("frame is neither a marshal stream nor zlib data")]
    Zlib,
    #[error("malformed packet: {0}")]
    Malformed(&'static str),
}

/// The low-level version check (`LowLevelVersionCheck`).
///
/// The listening side pitches `(170472, machoVersion, userCount, version,
/// build, codename)` and the connecting side answers with the same shape. The
/// client only checks the length, the birthday 170472 and `machoVersion`;
/// version/build/codename are echoed back in mismatch errors.
pub mod handshake {
    use marshal::Value;

    /// "A low-level-version challenge should be a tuple that starts with my birthday."
    pub const BIRTHDAY: i32 = 170472;
    /// `40 + [0,1000][packetGuard] + [0,10000][packetCRC32]`, both options off.
    pub const MACHO_VERSION: i32 = 40;

    /// boot.ini: version=10.0, build=1000, codename=Gemini.
    pub fn challenge(user_count: i32) -> Value {
        Value::tuple([
            Value::Int(BIRTHDAY),
            Value::Int(MACHO_VERSION),
            Value::Int(user_count),
            Value::Float(10.0),
            Value::Int(1000),
            Value::str("Gemini"),
        ])
    }

    /// Validates the client's reply exactly as the client validates ours.
    pub fn check(reply: &Value) -> Result<(), String> {
        let t = reply.as_seq().ok_or("version reply is not a tuple")?;
        if t.len() != 6 {
            return Err(format!("version reply has {} elements, not 6", t.len()));
        }
        if t[0].as_int() != Some(BIRTHDAY as i64) {
            return Err("version reply does not start with the birthday".into());
        }
        if t[1].as_int() != Some(MACHO_VERSION as i64) {
            return Err(format!(
                "protocol version mismatch: client {:?}, server {}",
                t[1], MACHO_VERSION
            ));
        }
        Ok(())
    }
}
