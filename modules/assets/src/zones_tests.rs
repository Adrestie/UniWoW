//! Tests of the zones of light of Wow.exe, read from executables the tests write and, when the
//! folder of a client is given, from its own.

use std::path::PathBuf;
use std::sync::Arc;

use uniwow_api::formats::Formats;

use crate::tests::scratch;
use crate::zones::read;
use crate::{Client, Files, FilesState};

/// Where the two sections of the executables written start in memory, then in the file.
const CONSTANTS: u32 = 0x00A3_B000;
const DATA: u32 = 0x00AD_E000;
const SIZE: u32 = 0x4000;

/// A Wow.exe as 12340 holds its zones of light: their entries (map, mark, light) and the paths
/// they name, the offsets of the client; `magic` the kind of its optional header (0x10B, 32 bits).
fn exe(zones: &[(u32, u32, u32, &str)], magic: u16) -> Vec<u8> {
    let mut bytes = vec![0u8; 0x400 + 2 * SIZE as usize];
    let mut put = |at: usize, data: &[u8]| bytes[at..at + data.len()].copy_from_slice(data);
    put(0, b"MZ");
    put(0x3C, &0x40u32.to_le_bytes());
    put(0x40, b"PE\0\0");
    put(0x46, &2u16.to_le_bytes());
    put(0x54, &0xE0u16.to_le_bytes());
    put(0x58, &magic.to_le_bytes());
    put(0x58 + 28, &0x0040_0000u32.to_le_bytes());
    for (section, (start, at)) in [(CONSTANTS, 0x400u32), (DATA, 0x400 + SIZE)].into_iter().enumerate() {
        let header = 0x58 + 0xE0 + 40 * section;
        put(header + 12, &(start - 0x0040_0000).to_le_bytes());
        put(header + 16, &SIZE.to_le_bytes());
        put(header + 20, &at.to_le_bytes());
    }
    let file = |address: u32| match address >= DATA {
        true => (0x400 + SIZE + address - DATA) as usize,
        false => (0x400 + address - CONSTANTS) as usize,
    };
    put(file(0x00A3_E8AC), &(-1.662_337_5f32).to_le_bytes());
    put(file(0x00A3_E8A8), &(-145.7316f32).to_le_bytes());
    for (zone, (map, mark, light, path)) in zones.iter().enumerate() {
        let entry = 0x00AD_EF58 + 48 * zone as u32;
        put(file(entry), &map.to_le_bytes());
        put(file(entry + 4), &mark.to_le_bytes());
        put(file(entry + 8), &light.to_le_bytes());
        let text = CONSTANTS + 0x100 + 0x300 * zone as u32;
        put(file(0x00AD_EE48 + 4 * zone as u32), &text.to_le_bytes());
        put(file(text), path.as_bytes());
    }
    bytes
}

/// The lights of the zones of Northrend, in the client's order.
const LIGHTS: [u32; 11] = [914, 825, 959, 862, 1847, 1703, 1796, 1777, 1792, 1589, 1740];

/// Eleven zones as the client holds them, of paths the tests make: the first of three points, the
/// second with a curve, the others a triangle.
fn zones() -> Vec<(u32, u32, u32, &'static str)> {
    let paths = [
        "L 400,500 L 380.25,520 L 390,540.5 z",
        "M -10,20 L 30.5,-40 C 1,2 3,4 5,6 L 50,60 z",
    ];
    LIGHTS
        .iter()
        .enumerate()
        .map(|(zone, light)| {
            (
                571,
                u32::MAX,
                *light,
                *paths.get(zone).unwrap_or(&"L 0,0 L 9,0 L 0,9 z"),
            )
        })
        .collect()
}

fn close(a: [f32; 2], b: [f32; 2]) -> bool {
    (a[0] - b[0]).abs() < 0.01 && (a[1] - b[1]).abs() < 0.01
}

#[test]
fn the_zones_of_light_are_read_from_the_paths_wow_exe_holds() {
    let read = read(&exe(&zones(), 0x10B)).unwrap();
    assert_eq!(read.iter().map(|zone| zone.light).collect::<Vec<_>>(), LIGHTS);
    assert!(read.iter().all(|zone| zone.map == 571));
    // In the world, as the client turns them: north from the second number, west from the first,
    // each plus its offset, 100/3 yards a unit from the middle of the world.
    let world = |across: f32, down: f32| {
        let unit = 100.0 / 3.0;
        let middle = 32.0 * 1600.0 / 3.0;
        [
            middle - (down - 145.7316) * unit,
            middle - (across - 1.662_337_5) * unit,
        ]
    };
    let first = &read[0].points;
    assert!(close(first[0], [5_257.72, 3_788.744_6]), "{first:?}");
    assert_eq!(first.len(), 3);
    for (point, (across, down)) in first.iter().zip([(400.0, 500.0), (380.25, 520.0), (390.0, 540.5)]) {
        assert!(close(*point, world(across, down)), "{point:?}");
    }
    // Its commands M and L alone, a curve left out with its points.
    let curved = &read[1].points;
    assert_eq!(curved.len(), 3, "{curved:?}");
    for (point, (across, down)) in curved.iter().zip([(-10.0, 20.0), (30.5, -40.0), (50.0, 60.0)]) {
        assert!(close(*point, world(across, down)), "{point:?}");
    }
}

#[test]
fn an_executable_other_than_wow_exe_12340_gives_no_zones() {
    let refused = |bytes: Vec<u8>| read(&bytes).unwrap_err();
    assert!(refused(b"not an executable".to_vec()).contains("not an executable of Windows"));
    for mark in [0, 0x40] {
        let mut unmarked = exe(&zones(), 0x10B);
        unmarked[mark] = b'X';
        assert!(refused(unmarked).contains("not an executable of Windows"), "{mark}");
    }
    assert!(refused(exe(&zones(), 0x20B)).contains("not an executable of 32 bits"));
    let mut shorter = exe(&zones(), 0x10B);
    shorter.truncate(0x150);
    assert!(refused(shorter).contains("its sections cut short"));
    // Every zone holding its entry, its mark and a path that closes on three points at least.
    let mut changed = zones();
    changed[4].1 = 0;
    assert!(refused(exe(&changed, 0x10B)).contains("no zone of light 4"));
    let mut fewer = zones();
    fewer.pop();
    assert!(refused(exe(&fewer, 0x10B)).contains("no zone of light 10"));
    // The address of the last path out of the executable.
    let mut lost = exe(&zones(), 0x10B);
    let at = (0x400 + SIZE + 0x00AD_EE48 + 40 - DATA) as usize;
    lost[at..at + 4].copy_from_slice(&0u32.to_le_bytes());
    assert!(refused(lost).contains("no path of the zone of light 10"));
    // Its path not ended within its section.
    let mut endless = exe(&zones(), 0x10B);
    endless[at..at + 4].copy_from_slice(&(CONSTANTS + SIZE - 0x100).to_le_bytes());
    endless[0x400 + SIZE as usize - 0x100..0x400 + SIZE as usize].fill(b'L');
    assert!(refused(endless).contains("no path of the zone of light 10"));
    for (path, why) in [
        ("L 0,0 L 9,0 L 0,9", "the zone of light 2: a path that does not close"),
        ("L 0,0 L 9,0 z", "the zone of light 2: a path of 2 points"),
        (
            "L 0,0 L 9,0 L x,9 z",
            "the zone of light 2: a point of a path unread at 12",
        ),
        (
            "L 0,0 L 9,0 L 0 9 z",
            "the zone of light 2: a point of a path unread at 12",
        ),
    ] {
        let mut changed = zones();
        changed[2].3 = path;
        let refused = refused(exe(&changed, 0x10B));
        assert!(refused.starts_with("not the Wow.exe of 3.3.5a 12340"), "{refused}");
        assert!(refused.ends_with(why), "{refused}");
    }
}

#[test]
fn the_zones_of_light_are_those_of_the_wow_exe_of_the_client_s_folder() {
    let folder = scratch("zones");
    let files = Files::default();
    files.set(FilesState::Ready(Arc::new(Client::open(Vec::new(), &folder, "enUS").0)));
    assert_eq!(files.zone_lights().unwrap_err(), "no Wow.exe in the client's folder");
    // Its name in any case; read once.
    std::fs::write(folder.join("WOW.EXE"), exe(&zones(), 0x10B)).unwrap();
    let files = Files::default();
    files.set(FilesState::Ready(Arc::new(Client::open(Vec::new(), &folder, "enUS").0)));
    assert_eq!(files.zone_lights().unwrap().len(), 11);
    std::fs::write(folder.join("WOW.EXE"), b"not an executable").unwrap();
    assert_eq!(files.zone_lights().unwrap().len(), 11, "read once");
    let files = Files::default();
    files.set(FilesState::Ready(Arc::new(Client::open(Vec::new(), &folder, "enUS").0)));
    let refused = files.zone_lights().unwrap_err();
    assert!(refused.ends_with("WOW.EXE: not an executable of Windows"), "{refused}");
    let _ = std::fs::remove_dir_all(folder);
}

#[test]
fn the_zones_of_light_of_the_client_are_those_wotlk_classic_stores() {
    let Ok(folder) = std::env::var("UNIWOW_CLIENT") else {
        eprintln!("skipped: UNIWOW_CLIENT names no client folder");
        return;
    };
    let (client, _) = Client::open(Vec::new(), &PathBuf::from(folder), "enUS");
    let zones = client.zone_lights().unwrap();
    assert_eq!(zones.iter().map(|zone| zone.light).collect::<Vec<_>>(), LIGHTS);
    assert!(zones.iter().all(|zone| zone.map == 571));
    // As many points as WotLK Classic stores, the curve of the fifth left out.
    assert_eq!(
        zones.iter().map(|zone| zone.points.len()).collect::<Vec<_>>(),
        [27, 58, 54, 31, 35, 61, 31, 35, 37, 52, 41]
    );
    // Its first point of the Borean Tundra and of the Howling Fjord, as it stores them.
    assert!(
        close(zones[0].points[0], [4_215.874_5, 3_269.265_4]),
        "{:?}",
        zones[0].points[0]
    );
    assert!(
        close(zones[3].points[0], [3_082.776_6, -6_049.62]),
        "{:?}",
        zones[3].points[0]
    );
}
