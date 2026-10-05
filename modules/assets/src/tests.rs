//! Tests on archives the tests write themselves, never on files of the client, which are Blizzard's;
//! and, when `UNIWOW_CLIENT` names the folder of a client, on its own archives.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use uniwow_api::miniz_oxide;

use crate::chain::{self, Chain, Source};
use crate::mpq::{
    self, Archive, ENCRYPTION_TABLE, Entry, FILE_COMPRESS, FILE_DELETE_MARKER, FILE_ENCRYPTED, FILE_EXISTS,
    FILE_SECTOR_CRC, FILE_SINGLE_UNIT, KEPT_BUFFER, PACKED, hash_string, hash_type,
};

/// Copied from wow-mpq (crypto/encryption.rs): encrypts a block, as an archive's tables are.
fn encrypt_block(data: &mut [u32], mut key: u32) {
    let mut seed: u32 = 0xEEEE_EEEE;
    for value in data.iter_mut() {
        seed = seed.wrapping_add(ENCRYPTION_TABLE[0x400 + (key & 0xFF) as usize]);
        let plain = *value;
        *value = plain ^ key.wrapping_add(seed);
        key = (!key << 0x15).wrapping_add(0x1111_1111) | (key >> 0x0B);
        seed = plain.wrapping_add(seed).wrapping_add(seed << 5).wrapping_add(3);
    }
}

/// How a file is written into a test archive.
#[derive(Clone, Copy)]
pub(crate) enum Stored {
    Plain,
    OneUnit,
    Sectors { crc: bool },
    Deleted,
}

/// A file of a test archive: its name, its bytes, how it is stored, and extra flags.
pub(crate) struct TestFile<'a> {
    name: &'a str,
    data: &'a [u8],
    stored: Stored,
    flags: u32,
}

pub(crate) fn file<'a>(name: &'a str, data: &'a [u8], stored: Stored) -> TestFile<'a> {
    TestFile {
        name,
        data,
        stored,
        flags: 0,
    }
}

/// A sector, or a file in one unit, packed: zlib behind its byte, unless that is no shorter.
fn pack(data: &[u8]) -> Vec<u8> {
    let mut packed = vec![0x02];
    packed.extend(miniz_oxide::deflate::compress_to_vec_zlib(data, 6));
    if packed.len() >= data.len() {
        data.to_vec()
    } else {
        packed
    }
}

/// Writes an archive of format 1 with sectors of 512 << `shift` bytes, its list included.
pub(crate) fn write_archive(path: &Path, shift: u16, files: &[TestFile]) {
    let sector = 512usize << shift;
    let names: Vec<&str> = files.iter().map(|file| file.name).collect();
    let listfile = names.join("\r\n");
    let mut all: Vec<TestFile> = files
        .iter()
        .map(|f| TestFile {
            name: f.name,
            data: f.data,
            stored: f.stored,
            flags: f.flags,
        })
        .collect();
    all.push(file("(listfile)", listfile.as_bytes(), Stored::Plain));
    let mut body = vec![0u8; 32];
    let mut blocks: Vec<[u32; 4]> = Vec::new();
    for file in &all {
        let offset = body.len() as u32;
        let (packed, flags) = match file.stored {
            Stored::Plain => (file.data.to_vec(), FILE_EXISTS),
            Stored::OneUnit => (pack(file.data), FILE_EXISTS | FILE_COMPRESS | FILE_SINGLE_UNIT),
            Stored::Sectors { crc } => {
                let sectors: Vec<Vec<u8>> = file.data.chunks(sector).map(pack).collect();
                let entries = sectors.len() + 1 + usize::from(crc);
                let mut table = Vec::new();
                let mut at = entries * 4;
                for sector in &sectors {
                    table.push(at as u32);
                    at += sector.len();
                }
                table.push(at as u32);
                if crc {
                    table.push((at + sectors.len() * 4) as u32);
                }
                let mut packed: Vec<u8> = table.iter().flat_map(|n| n.to_le_bytes()).collect();
                sectors.iter().for_each(|sector| packed.extend(sector));
                if crc {
                    packed.extend(vec![0u8; sectors.len() * 4]);
                }
                (
                    packed,
                    FILE_EXISTS | FILE_COMPRESS | if crc { FILE_SECTOR_CRC } else { 0 },
                )
            }
            Stored::Deleted => (Vec::new(), FILE_EXISTS | FILE_DELETE_MARKER),
        };
        let size = if matches!(file.stored, Stored::Deleted) {
            0
        } else {
            file.data.len() as u32
        };
        body.extend(&packed);
        blocks.push([offset, packed.len() as u32, size, flags | file.flags]);
    }
    let hash_count = (all.len() * 2).next_power_of_two().max(4);
    let mut hash = vec![[0xFFFF_FFFFu32; 4]; hash_count];
    for (block, file) in all.iter().enumerate() {
        let mut slot = hash_string(file.name, hash_type::TABLE_OFFSET) as usize & (hash_count - 1);
        while hash[slot][3] != 0xFFFF_FFFF {
            slot = (slot + 1) & (hash_count - 1);
        }
        hash[slot] = [
            hash_string(file.name, hash_type::NAME_A),
            hash_string(file.name, hash_type::NAME_B),
            0,
            block as u32,
        ];
    }
    let mut table = |entries: &[[u32; 4]], name: &str| {
        let mut numbers: Vec<u32> = entries.iter().flatten().copied().collect();
        encrypt_block(&mut numbers, hash_string(name, hash_type::FILE_KEY));
        let at = body.len() as u32;
        body.extend(numbers.iter().flat_map(|n| n.to_le_bytes()));
        at
    };
    let hash_at = table(&hash, "(hash table)");
    let block_at = table(&blocks, "(block table)");
    let header: [u32; 8] = [
        u32::from_le_bytes(*b"MPQ\x1a"),
        32,
        body.len() as u32,
        u32::from(shift) << 16,
        hash_at,
        block_at,
        hash_count as u32,
        blocks.len() as u32,
    ];
    body[..32].copy_from_slice(&header.iter().flat_map(|n| n.to_le_bytes()).collect::<Vec<u8>>());
    std::fs::write(path, body).unwrap();
}

/// A folder of its own for a test, empty.
pub(crate) fn scratch(name: &str) -> PathBuf {
    let folder = std::env::temp_dir()
        .join("uniwow-assets-tests")
        .join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).unwrap();
    folder
}

/// Bytes that do not compress, then bytes that do.
fn sample(length: usize) -> Vec<u8> {
    let mut seed = 0x1234_5678u32;
    (0..length)
        .map(|at| {
            if at < length / 3 {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed as u8
            } else {
                (at % 7) as u8
            }
        })
        .collect()
}

fn read(archive: &Archive, name: &str) -> Vec<u8> {
    match archive.find(name) {
        Some(Entry::File(index)) => archive.read(index).unwrap(),
        other => panic!("{name}: {other:?}"),
    }
}

#[test]
fn the_hash_of_names_is_that_of_the_mpq_format() {
    assert_eq!(hash_string("(listfile)", hash_type::TABLE_OFFSET), 0x5F3D_E859);
    assert_eq!(hash_string("(hash table)", hash_type::FILE_KEY), 0xC3AF_3770);
    assert_eq!(hash_string("(block table)", hash_type::FILE_KEY), 0xEC83_B3A3);
    assert_eq!(hash_string("path\\to\\file", hash_type::TABLE_OFFSET), 0x534C_C8EE);
    assert_eq!(
        hash_string("Path/To/File", hash_type::TABLE_OFFSET),
        hash_string("path\\to\\file", hash_type::TABLE_OFFSET),
        "case and slashes ignored"
    );
}

#[test]
fn files_stored_compressed_in_one_unit_or_in_sectors_read_back_whole() {
    let folder = scratch("stored");
    let path = folder.join("test.mpq");
    let (long, exact) = (sample(10_000), sample(4096 * 3));
    write_archive(
        &path,
        3,
        &[
            file("plain.bin", &long, Stored::Plain),
            file("Unit\\One.bin", &long, Stored::OneUnit),
            file("sectors.bin", &long, Stored::Sectors { crc: false }),
            file("crc.bin", &long, Stored::Sectors { crc: true }),
            file("exact.bin", &exact, Stored::Sectors { crc: true }),
            file("empty.bin", &[], Stored::Plain),
        ],
    );
    let archive = Archive::open(&path).unwrap();
    for name in ["plain.bin", "unit/one.bin", "sectors.bin", "crc.bin"] {
        assert_eq!(read(&archive, name), long, "{name}");
    }
    assert_eq!(read(&archive, "exact.bin"), exact);
    assert!(read(&archive, "empty.bin").is_empty());
    assert!(archive.find("absent.bin").is_none());
    let mut listed = archive.listed().unwrap();
    listed.sort();
    assert_eq!(listed.len(), 6);
    let _ = std::fs::remove_dir_all(folder);
}

#[test]
fn a_file_larger_than_a_thread_keeps_is_read_in_an_allocation_of_its_own() {
    let folder = scratch("large");
    let path = folder.join("test.mpq");
    // Bytes that do not compress, more than a thread's buffer holds.
    let mut seed = 0x9E37_79B9u32;
    let large: Vec<u8> = (0..KEPT_BUFFER + 300_000)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as u8
        })
        .collect();
    let small = sample(5000);
    write_archive(
        &path,
        3,
        &[
            file("large.bin", &large, Stored::Sectors { crc: true }),
            file("small.bin", &small, Stored::Sectors { crc: false }),
        ],
    );
    let archive = Archive::open(&path).unwrap();
    assert_eq!(read(&archive, "small.bin"), small);
    assert_eq!(read(&archive, "large.bin"), large);
    PACKED.with_borrow(|packed| assert!(packed.capacity() <= KEPT_BUFFER, "{} kept", packed.capacity()));
    let _ = std::fs::remove_dir_all(folder);
}

#[test]
fn what_the_archives_of_3_3_5a_do_not_hold_is_refused_by_name() {
    let folder = scratch("refused");
    let data = sample(2000);
    let path = folder.join("test.mpq");
    write_archive(
        &path,
        3,
        &[TestFile {
            name: "secret.bin",
            data: &data,
            stored: Stored::Plain,
            flags: FILE_ENCRYPTED,
        }],
    );
    let archive = Archive::open(&path).unwrap();
    let Some(Entry::File(index)) = archive.find("secret.bin") else {
        panic!()
    };
    assert!(archive.read(index).unwrap_err().contains("encrypted"));
    // A sector compressed with another method than zlib.
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[12..14].copy_from_slice(&2u16.to_le_bytes());
    std::fs::write(folder.join("v3.mpq"), &bytes).unwrap();
    assert!(
        Archive::open(&folder.join("v3.mpq"))
            .err()
            .unwrap()
            .contains("format 3")
    );
    std::fs::write(folder.join("text.mpq"), b"not an archive at all, really").unwrap();
    assert!(
        Archive::open(&folder.join("text.mpq"))
            .err()
            .unwrap()
            .contains("not an MPQ")
    );
    let mut wrong = vec![0x08];
    wrong.extend(miniz_oxide::deflate::compress_to_vec_zlib(&data, 6));
    let one = folder.join("one.mpq");
    write_archive(&one, 3, &[file("one.bin", &data, Stored::OneUnit)]);
    let mut bytes = std::fs::read(&one).unwrap();
    // The method byte of the file in one unit, right after the header.
    bytes[32] = 0x08;
    std::fs::write(&one, &bytes).unwrap();
    let archive = Archive::open(&one).unwrap();
    let Some(Entry::File(index)) = archive.find("one.bin") else {
        panic!()
    };
    assert!(archive.read(index).unwrap_err().contains("0x08"));
    let _ = std::fs::remove_dir_all(folder);
}

#[test]
fn a_patch_deleting_a_file_hides_the_one_of_the_archives_read_after_it() {
    let folder = scratch("deleted");
    let (old, new) = (b"old".to_vec(), b"new".to_vec());
    write_archive(
        &folder.join("patch.mpq"),
        3,
        &[
            file("a.txt", b"", Stored::Deleted),
            file("Dir\\b.txt", &new, Stored::OneUnit),
        ],
    );
    write_archive(
        &folder.join("base.mpq"),
        3,
        &[
            file("a.txt", &old, Stored::Plain),
            file("dir\\b.txt", &old, Stored::Plain),
            file("dir\\c.txt", &old, Stored::Plain),
        ],
    );
    let sources = ["patch.mpq", "base.mpq"]
        .iter()
        .map(|name| Source::open(&folder.join(name)).unwrap())
        .collect();
    let chain = Chain::new(sources);
    assert_eq!(chain.read("a.txt").unwrap(), None, "deleted by the patch");
    assert!(!chain.exists("a.txt"));
    assert_eq!(chain.read("DIR/B.TXT").unwrap(), Some(new), "the patch's");
    assert_eq!(chain.read("dir\\c.txt").unwrap(), Some(old));
    let mut under = chain.files_under("dir");
    under.sort();
    assert_eq!(under, vec!["Dir\\b.txt".to_owned(), "dir\\c.txt".to_owned()]);
    assert!(chain.files_under("").iter().all(|name| name != "a.txt"));
    let _ = std::fs::remove_dir_all(folder);
}

#[test]
fn patches_of_any_name_and_folders_mounted_as_archives_come_in_the_client_s_order() {
    let client = scratch("order");
    let data = client.join("Data");
    let locale = data.join("enUS");
    std::fs::create_dir_all(&locale).unwrap();
    for name in [
        "common.MPQ",
        "common-2.MPQ",
        "lichking.MPQ",
        "expansion.MPQ",
        "patch.MPQ",
        "patch-2.MPQ",
        "patch-z.MPQ",
        "Patch-Mod.MPQ.disabled",
    ] {
        write_archive(
            &data.join(name),
            3,
            &[file("which.txt", name.as_bytes(), Stored::Plain)],
        );
    }
    for name in ["locale-enUS.MPQ", "patch-enUS-3.MPQ", "patch-enUS.MPQ", "base-enUS.MPQ"] {
        write_archive(
            &locale.join(name),
            3,
            &[file("which.txt", name.as_bytes(), Stored::Plain)],
        );
    }
    let loose = data.join("Patch-Mod.MPQ").join("World");
    std::fs::create_dir_all(&loose).unwrap();
    std::fs::write(loose.join("Loose.txt"), b"loose").unwrap();
    assert_eq!(chain::locale(&client).unwrap(), "enUS");
    let order: Vec<String> = chain::order(&client, "enUS")
        .iter()
        .map(|path| {
            path.strip_prefix(&client)
                .unwrap()
                .to_string_lossy()
                .to_ascii_lowercase()
        })
        .collect();
    assert_eq!(
        order,
        [
            "data\\patch-z.mpq",
            "data\\patch-mod.mpq",
            "data\\patch-2.mpq",
            "data\\enus\\patch-enus-3.mpq",
            "data\\patch.mpq",
            "data\\enus\\patch-enus.mpq",
            "data\\expansion.mpq",
            "data\\lichking.mpq",
            "data\\common.mpq",
            "data\\common-2.mpq",
            "data\\enus\\locale-enus.mpq",
        ],
        "the order of Wow.exe 12340; base-enUS.MPQ, which the game does not read, left out"
    );
    let sources = chain::order(&client, "enUS")
        .iter()
        .map(|path| Source::open(path).unwrap())
        .collect();
    let chain = Chain::new(sources);
    assert_eq!(chain.read("which.txt").unwrap(), Some(b"patch-z.MPQ".to_vec()));
    assert_eq!(
        chain.read("world/loose.txt").unwrap(),
        Some(b"loose".to_vec()),
        "a folder mounted"
    );
    std::fs::create_dir_all(client.join("WTF")).unwrap();
    std::fs::create_dir_all(data.join("frFR")).unwrap();
    std::fs::write(
        client.join("WTF").join("Config.wtf"),
        "SET gxApi \"d3d9\"\r\nSET locale \"frFR\"\r\n",
    )
    .unwrap();
    assert_eq!(chain::locale(&client).unwrap(), "frFR", "as Config.wtf sets it");
    let _ = std::fs::remove_dir_all(client);
}

#[test]
fn many_threads_read_one_archive_at_once() {
    let folder = scratch("threads");
    let path = folder.join("test.mpq");
    let data: Vec<Vec<u8>> = (0..32).map(|n| sample(3000 + n * 997)).collect();
    let names: Vec<String> = (0..32).map(|n| format!("file{n}.bin")).collect();
    let files: Vec<TestFile> = names
        .iter()
        .zip(&data)
        .map(|(name, data)| file(name, data, Stored::Sectors { crc: true }))
        .collect();
    write_archive(&path, 3, &files);
    let archive = Archive::open(&path).unwrap();
    std::thread::scope(|scope| {
        for _ in 0..16 {
            scope.spawn(|| {
                for _ in 0..20 {
                    for (name, expected) in names.iter().zip(&data) {
                        assert_eq!(&read(&archive, name), expected);
                    }
                }
            });
        }
    });
    let _ = std::fs::remove_dir_all(folder);
}

/// The client named by `UNIWOW_CLIENT`, its archives open, or none.
fn client() -> Option<Chain> {
    let Ok(folder) = std::env::var("UNIWOW_CLIENT") else {
        eprintln!("skipped: UNIWOW_CLIENT names no client folder");
        return None;
    };
    let folder = PathBuf::from(folder);
    let locale = chain::locale(&folder).unwrap();
    let sources = chain::order(&folder, &locale)
        .iter()
        .map(|path| Source::open(path).unwrap_or_else(|e| panic!("{}: {e}", path.display())))
        .collect();
    Some(Chain::new(sources))
}

#[test]
fn the_client_s_archives_read_as_the_client_reads_them() {
    let Some(chain) = client() else { return };
    let tables = chain.files_under("DBFilesClient");
    assert!(tables.len() > 200, "{} tables", tables.len());
    for name in tables.iter().filter(|name| name.to_ascii_lowercase().ends_with(".dbc")) {
        let bytes = chain.read(name).unwrap().unwrap();
        let field = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        assert_eq!(&bytes[..4], b"WDBC", "{name}");
        assert_eq!(20 + field(4) * field(12) + field(16), bytes.len(), "{name}");
    }
    let all = chain.files_under("");
    let step = (all.len() / 3000).max(1);
    for name in all.iter().step_by(step) {
        assert!(chain.read(name).unwrap().is_some(), "{name}");
    }
    eprintln!(
        "{} archives, {} files listed",
        chain.sources().len(),
        chain.listed_count()
    );
}

/// Run on demand, a few minutes: `cargo test -p uniwow-module-assets -- --ignored --nocapture`.
#[test]
#[ignore = "reads every file of the client named by UNIWOW_CLIENT"]
fn every_file_the_client_lists_is_read_and_none_is_refused() {
    let Some(chain) = client() else { return };
    let all = chain.files_under("");
    let next = AtomicUsize::new(0);
    let refused = std::sync::Mutex::new(std::collections::BTreeMap::<String, Vec<String>>::new());
    let (bytes, sounds) = (AtomicUsize::new(0), AtomicUsize::new(0));
    let started = Instant::now();
    std::thread::scope(|scope| {
        for _ in 0..16 {
            scope.spawn(|| {
                while let Some(name) = all.get(next.fetch_add(1, Ordering::Relaxed)) {
                    match chain.read(name) {
                        Ok(Some(data)) => {
                            bytes.fetch_add(data.len(), Ordering::Relaxed);
                            let lower = name.to_ascii_lowercase();
                            if [".wav", ".mp3", ".ogg"].iter().any(|kind| lower.ends_with(kind)) {
                                sounds.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        Ok(None) => refused
                            .lock()
                            .unwrap()
                            .entry("listed but absent".to_owned())
                            .or_default()
                            .push(name.clone()),
                        Err(error) => {
                            let cause = [
                                "encrypted",
                                "implode",
                                "compression",
                                "incremental",
                                "table of sectors",
                                "cannot be read",
                                "zlib",
                            ]
                            .into_iter()
                            .find(|cause| error.contains(cause))
                            .unwrap_or("other");
                            refused.lock().unwrap().entry(cause.to_owned()).or_default().push(error);
                        }
                    }
                }
            });
        }
    });
    let refused = refused.into_inner().unwrap();
    eprintln!(
        "{} files read, {} sounds among them, {:.1} GB, in {:.0} s; refused: {:?}",
        all.len(),
        sounds.load(Ordering::Relaxed),
        bytes.load(Ordering::Relaxed) as f64 / 1e9,
        started.elapsed().as_secs_f64(),
        refused
            .iter()
            .map(|(cause, files)| (cause.as_str(), files.len(), files.first()))
            .collect::<Vec<_>>()
    );
    assert!(refused.is_empty());
}

#[test]
fn reading_the_client_s_archives_from_several_threads_at_once() {
    let Some(chain) = client() else { return };
    let kinds = [".blp", ".m2", ".adt", ".wmo", ".skin"];
    let all: Vec<String> = chain
        .files_under("")
        .into_iter()
        .filter(|name| kinds.iter().any(|kind| name.to_ascii_lowercase().ends_with(kind)))
        .collect();
    let threads = [1usize, 2, 4, 8, 16, 32];
    let per_run = 1500;
    let step = (all.len() / (per_run * threads.len() * 2)).max(1);
    let picked: Vec<&String> = all.iter().step_by(step).collect();
    for (round, label) in ["files never read", "files cached"].iter().enumerate() {
        for (number, &count) in threads.iter().enumerate() {
            // The first round reads files of their own for each run; the second, those of the first.
            let run: Vec<&String> = picked
                .iter()
                .skip(number)
                .step_by(threads.len())
                .take(per_run)
                .copied()
                .collect();
            let next = AtomicUsize::new(0);
            let bytes = AtomicUsize::new(0);
            let started = Instant::now();
            std::thread::scope(|scope| {
                for _ in 0..count {
                    scope.spawn(|| {
                        while let Some(name) = run.get(next.fetch_add(1, Ordering::Relaxed)) {
                            let read = chain.read(name).unwrap().map_or(0, |data| data.len());
                            bytes.fetch_add(read, Ordering::Relaxed);
                        }
                    });
                }
            });
            let seconds = started.elapsed().as_secs_f64();
            eprintln!(
                "{label}, {count:>2} threads: {:>7.1} MB/s",
                bytes.load(Ordering::Relaxed) as f64 / 1e6 / seconds
            );
            let _ = round;
        }
    }
    let _ = mpq::FILE_EXISTS;
}
