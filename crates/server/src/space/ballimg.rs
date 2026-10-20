//! Writer for destiny balls as blue object images (marshal tag 0x18).
//!
//! Ballpark.SetState / AddBalls take a list of real destiny ball objects: the
//! native SetState (destiny.dll FUN_10021200) reads the C++ fields directly.
//! They travel as blue object images ("Blue\0" v7, tools/blue_image.py), which
//! BeROT loads through the class factory named by the CLSID. The client's
//! destiny.dll has a factory for ClientBall (v1, 0x288 bytes) but none for
//! plain Ball, so every ball is a destiny.ClientBall.
//!
//! The reader (blue.dll FUN_1002c6c0) resolves each record BY NAME in the live
//! class, requires the VARTYPE to match and the member to carry the persist
//! flag 0x10. Members and types below are destiny's own member table
//! (docs/destiny-ball-members.json, FUN_10009f50); yaw/pitch/roll, the bubble
//! ids and `ballpark` are not persistable and must not be written.

/// Ball modes (destiny DSTBALL_* enum).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Mode {
    Goto = 0,
    Follow = 1,
    Stop = 2,
    Warp = 3,
    Orbit = 4,
    Missile = 5,
    Mushroom = 6,
    Boid = 7,
    Troll = 8,
    Miniball = 9,
    Field = 10,
    Rigid = 11,
}

#[derive(Debug, Clone)]
pub struct Ball {
    pub id: i64,
    pub mass: f32,
    pub radius: f32,
    pub max_velocity: f32,
    pub is_free: bool,
    pub is_global: bool,
    pub is_massive: bool,
    pub is_interactive: bool,
    pub pos: [f64; 3],
    pub vel: [f64; 3],
    pub agility: f32,
    pub speed_fraction: f32,
    pub mode: Mode,
    pub goto: [f64; 3],
    pub follow_id: i64,
    pub follow_range: f64,
    pub owner_id: i64,
    pub effect_stamp: i32,
}

impl Ball {
    /// A static celestial: not free, not interactive (stations and gates are
    /// interactive so they can be docked at / jumped through).
    pub fn fixed(id: i64, radius: f64, pos: [f64; 3], interactive: bool, global: bool) -> Ball {
        Ball {
            id,
            mass: 1.0e12,
            radius: radius as f32,
            max_velocity: 0.0,
            is_free: false,
            is_global: global,
            is_massive: true,
            is_interactive: interactive,
            pos,
            vel: [0.0; 3],
            agility: 1.0,
            speed_fraction: 0.0,
            mode: Mode::Rigid,
            goto: pos,
            follow_id: 0,
            follow_range: 0.0,
            owner_id: 1,
            effect_stamp: -1,
        }
    }
}

/// VARTYPEs: 1 int32, 2 float32, 3 float64, 4 bool.
const MEMBERS: [(&str, u8); 24] = [
    ("id", 1),
    ("mass", 2),
    ("radius", 2),
    ("maxVelocity", 2),
    ("isFree", 4),
    ("isGlobal", 4),
    ("isMassive", 4),
    ("isInteractive", 4),
    ("x", 3),
    ("y", 3),
    ("z", 3),
    ("vx", 3),
    ("vy", 3),
    ("vz", 3),
    ("Agility", 2),
    ("speedFraction", 2),
    ("mode", 1),
    ("gotoX", 3),
    ("gotoY", 3),
    ("gotoZ", 3),
    ("followId", 1),
    ("followRange", 3),
    ("ownerId", 1),
    ("effectStamp", 1),
];

const MODULE: &str = "destiny";
const CLASS: &str = "ClientBall";
const CLASS_VERSION: u16 = 1;
/// Ends an object whose class carries Python state, with none attached.
const MARKER_NO_STATE: u8 = 0xD3;

fn put_u16(b: &mut Vec<u8>, v: u16) {
    b.extend_from_slice(&v.to_le_bytes());
}

fn put_u32(b: &mut Vec<u8>, v: u32) {
    b.extend_from_slice(&v.to_le_bytes());
}

fn padded(s: &str, n: usize) -> Vec<u8> {
    let mut v = s.as_bytes().to_vec();
    v.resize(n, 0);
    v
}

/// One ball as a complete blue object image.
pub fn image(ball: &Ball, filetime: u64) -> Vec<u8> {
    // Strings: class name, module, then member names.
    let mut strings: Vec<&str> = vec![CLASS, MODULE];
    strings.extend(MEMBERS.iter().map(|(n, _)| *n));

    // Object: classIdx 0, then {gap, value} records for every member in table
    // order (gap 0 = the next member), 0xff, marker.
    let mut obj = vec![0u8];
    let mut records = 0u32;
    let mut rec = |obj: &mut Vec<u8>, bytes: &[u8]| {
        obj.push(0);
        obj.extend_from_slice(bytes);
        records += 1;
    };
    let f32b = |v: f32| v.to_le_bytes().to_vec();
    let f64b = |v: f64| v.to_le_bytes().to_vec();
    let i32b = |v: i64| (v as i32).to_le_bytes().to_vec();
    rec(&mut obj, &i32b(ball.id));
    rec(&mut obj, &f32b(ball.mass));
    rec(&mut obj, &f32b(ball.radius));
    rec(&mut obj, &f32b(ball.max_velocity));
    rec(&mut obj, &[ball.is_free as u8]);
    rec(&mut obj, &[ball.is_global as u8]);
    rec(&mut obj, &[ball.is_massive as u8]);
    rec(&mut obj, &[ball.is_interactive as u8]);
    for v in ball.pos.iter().chain(&ball.vel) {
        rec(&mut obj, &f64b(*v));
    }
    rec(&mut obj, &f32b(ball.agility));
    rec(&mut obj, &f32b(ball.speed_fraction));
    rec(&mut obj, &i32b(ball.mode as i64));
    for v in &ball.goto {
        rec(&mut obj, &f64b(*v));
    }
    rec(&mut obj, &i32b(ball.follow_id));
    rec(&mut obj, &f64b(ball.follow_range));
    rec(&mut obj, &i32b(ball.owner_id));
    rec(&mut obj, &i32b(ball.effect_stamp as i64));
    obj.push(0xFF);
    obj.push(MARKER_NO_STATE);

    // Body: root reference (inline, share slot 0) followed by the object.
    let mut body = vec![0x05];
    put_u16(&mut body, 0);
    body.extend_from_slice(&obj);

    let mut classes = Vec::new();
    put_u16(&mut classes, 0); // class name: strings[0]
    put_u16(&mut classes, 1); // module: strings[1]
    put_u32(&mut classes, CLASS_VERSION as u32);
    put_u32(&mut classes, MEMBERS.len() as u32);
    for (i, (_, vt)) in MEMBERS.iter().enumerate() {
        put_u16(&mut classes, (i + 2) as u16);
        classes.push(*vt);
    }

    let mut tail = classes;
    put_u32(&mut tail, obj.len() as u32 + 2); // share-length table: length + 2
    let mut string_bytes = 0u32;
    for s in &strings {
        put_u32(&mut tail, s.len() as u32);
        tail.extend_from_slice(s.as_bytes());
        string_bytes += s.len() as u32;
    }

    let mut out = Vec::with_capacity(0x64 + body.len() + tail.len());
    out.extend_from_slice(b"Blue\0");
    put_u32(&mut out, 7);
    out.extend_from_slice(&padded(MODULE, 10));
    out.extend_from_slice(&padded(CLASS, 24));
    put_u16(&mut out, CLASS_VERSION);
    let size_at = out.len();
    put_u32(&mut out, 0); // total size, patched below
    out.extend_from_slice(&filetime.to_le_bytes());
    put_u32(&mut out, 1); // objects
    out.extend_from_slice(&[0u8; 11]);
    put_u32(&mut out, body.len() as u32);
    put_u32(&mut out, strings.len() as u32);
    put_u32(&mut out, string_bytes);
    put_u32(&mut out, 1); // classes
    put_u32(&mut out, 1); // objects
    put_u32(&mut out, records);
    put_u32(&mut out, 0);
    debug_assert_eq!(out.len(), 0x64);
    out.extend_from_slice(&body);
    out.extend_from_slice(&tail);
    let size = out.len() as u32;
    out[size_at..size_at + 4].copy_from_slice(&size.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_layout_is_consistent() {
        let b = Ball::fixed(40000001, 5.0e8, [1.0, 2.0, 3.0], false, true);
        let img = image(&b, 0x01c2eee551e3ca20);
        assert_eq!(&img[..5], b"Blue\0");
        let u32_at = |o: usize| u32::from_le_bytes(img[o..o + 4].try_into().unwrap());
        assert_eq!(u32_at(0x2d) as usize, img.len());
        let body_len = u32_at(0x48) as usize;
        // The body ends exactly where the class table starts: FF D3 tail.
        assert_eq!(&img[0x64 + body_len - 2..0x64 + body_len], &[0xFF, 0xD3]);
        // Share-length entry = object length (from the classIdx byte) + 2.
        let class_table = 12 + MEMBERS.len() * 3;
        let share = u32_at(0x64 + body_len + class_table) as usize;
        assert_eq!(share - 2, body_len - 3);
        assert_eq!(u32_at(0x5c) as usize, MEMBERS.len());
    }
}

#[cfg(test)]
mod dump {
    /// Writes a sample image for tools/blue_image.py to parse (cross-check).
    #[test]
    fn dump_sample_image() {
        let mut b = super::Ball::fixed(60000141, 15000.0, [1.5e10, -2.0e8, 3.25e9], true, false);
        b.owner_id = 1000001;
        let img = super::image(&b, 0x01c2eee551e3ca20);
        std::fs::write(std::env::temp_dir().join("genesis_ball.blue"), img).unwrap();
    }
}
