//! The zones of light of Northrend, which Wow.exe 12340 holds in place of a table: 11 entries of 48
//! bytes at 0xADEF58, each its map, −1 and its light of `Light.dbc`; their outlines, texts of SVG
//! paths among its constants, named by the 11 addresses at 0xADEE48, in the units of an image. As
//! the client starts (0x77ED40), it reads only their commands `M` and `L`, adds to each point the
//! two offsets at 0xA3E8AC and 0xA3E8A8, and turns it into the world, 100/3 yards a unit.

use uniwow_api::formats::{TILE, ZoneLightRecord};

/// The count of the zones, the address of their table and the size of an entry in it, and the
/// address of the addresses of their paths.
const ZONES: u32 = 11;
const TABLE: u32 = 0x00AD_EF58;
const ENTRY: u32 = 48;
const PATHS: u32 = 0x00AD_EE48;
/// The offsets of a point of a path, across the image then down it.
const ACROSS: u32 = 0x00A3_E8AC;
const DOWN: u32 = 0x00A3_E8A8;
/// The middle of the world, in yards, and the yards of a unit of the image.
const MIDDLE: f32 = 32.0 * TILE;
const UNIT: f32 = TILE / 16.0;

/// An executable of 32 bits: its bytes, where it is loaded, and its sections, each where it starts
/// from the load, how many bytes the file holds of it and where.
struct Image<'a> {
    bytes: &'a [u8],
    base: u32,
    sections: Vec<[u32; 3]>,
}

impl<'a> Image<'a> {
    fn parse(bytes: &'a [u8]) -> Result<Self, String> {
        let u16_at = |at: usize| Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?));
        let u32_at = |at: usize| Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?));
        let header = u32_at(0x3C)
            .filter(|_| bytes.starts_with(b"MZ"))
            .map(|at| at as usize)
            .filter(|at| bytes.get(*at..*at + 4) == Some(b"PE\0\0"))
            .ok_or("not an executable of Windows")?;
        let optional = header + 24;
        let (Some(count), Some(size), Some(0x10B), Some(base)) = (
            u16_at(header + 6),
            u16_at(header + 20),
            u16_at(optional),
            u32_at(optional + 28),
        ) else {
            return Err("not an executable of 32 bits".to_owned());
        };
        let sections = (0..usize::from(count))
            .map(|section| {
                let at = optional + usize::from(size) + 40 * section;
                Some([u32_at(at + 12)?, u32_at(at + 16)?, u32_at(at + 20)?])
            })
            .collect::<Option<Vec<_>>>()
            .ok_or("its sections cut short")?;
        Ok(Self { bytes, base, sections })
    }

    /// The bytes the file holds from `address`, when loaded, to the end of its section.
    fn from(&self, address: u32) -> Option<&'a [u8]> {
        let relative = address.checked_sub(self.base)?;
        self.sections.iter().find_map(|&[start, size, at]| {
            let into = relative.checked_sub(start).filter(|into| *into < size)?;
            self.bytes.get(at as usize + into as usize..at as usize + size as usize)
        })
    }

    fn u32(&self, address: u32) -> Option<u32> {
        Some(u32::from_le_bytes(self.from(address)?.get(..4)?.try_into().ok()?))
    }

    fn f32(&self, address: u32) -> Option<f32> {
        self.u32(address).map(f32::from_bits)
    }

    /// The text at `address`, to its null within its section.
    fn text(&self, address: u32) -> Option<&'a [u8]> {
        let text = self.from(address)?;
        Some(&text[..text.iter().position(|byte| *byte == 0)?])
    }
}

/// The number `text` starts with, as `atof` reads it, without exponent.
fn number(text: &[u8]) -> Option<f32> {
    let text = text.trim_ascii_start();
    let end = text
        .iter()
        .position(|byte| !(byte.is_ascii_digit() || matches!(byte, b'+' | b'-' | b'.')))
        .unwrap_or(text.len());
    std::str::from_utf8(&text[..end]).ok()?.parse().ok()
}

/// The points of `path` before its `z`, those of its commands `M` and `L`, each across and down the
/// image, two places past its letter.
fn points(path: &[u8]) -> Result<Vec<[f32; 2]>, String> {
    let end = path
        .iter()
        .position(|byte| *byte == b'z')
        .ok_or("a path that does not close")?;
    let path = &path[..end];
    let mut points = Vec::new();
    for (at, byte) in path.iter().enumerate() {
        if !matches!(byte, b'M' | b'L') {
            continue;
        }
        let pair = path.get(at + 2..).unwrap_or_default();
        let comma = path[at..].iter().position(|byte| *byte == b',');
        let down = comma.and_then(|comma| number(&path[at + comma + 1..]));
        let (Some(across), Some(down)) = (number(pair), down) else {
            return Err(format!("a point of a path unread at {at}"));
        };
        points.push([across, down]);
    }
    if points.len() < 3 {
        return Err(format!("a path of {} points", points.len()));
    }
    Ok(points)
}

/// The zones of light the executable `bytes` holds, in its order; refused when it is not Wow.exe
/// 12340.
pub fn read(bytes: &[u8]) -> Result<Vec<ZoneLightRecord>, String> {
    let image = Image::parse(bytes)?;
    let other = |what: String| format!("not the Wow.exe of 3.3.5a 12340: {what}");
    let (Some(across), Some(down)) = (image.f32(ACROSS), image.f32(DOWN)) else {
        return Err(other("no offsets of its zones of light".to_owned()));
    };
    (0..ZONES)
        .map(|zone| {
            let entry = TABLE + zone * ENTRY;
            let (Some(map), Some(u32::MAX), Some(light)) =
                (image.u32(entry), image.u32(entry + 4), image.u32(entry + 8))
            else {
                return Err(other(format!("no zone of light {zone}")));
            };
            let path = image
                .u32(PATHS + 4 * zone)
                .and_then(|address| image.text(address))
                .ok_or_else(|| other(format!("no path of the zone of light {zone}")))?;
            let points = points(path).map_err(|why| other(format!("the zone of light {zone}: {why}")))?;
            Ok(ZoneLightRecord {
                map,
                light,
                points: points
                    .into_iter()
                    .map(|point| [MIDDLE - (point[1] + down) * UNIT, MIDDLE - (point[0] + across) * UNIT])
                    .collect(),
            })
        })
        .collect()
}
