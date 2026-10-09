//! Tests of the tables on files the tests write themselves, never on files of the client: DB2 of
//! each version WarcraftXL reads, and DBC; and, when `UNIWOW_CLIENT` names the folder of a client,
//! its own tables.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use uniwow_api::formats::Formats;
use uniwow_api::vfs::Vfs;

use crate::chain::{self, Chain, Source};
use crate::db2::{BITPACKED, COMMON, FileIds, NONE, PALLET, PathTable, SIGNED};
use crate::dbc::Tables;
use crate::tests::{Stored, file, scratch, write_archive};
use crate::{Client, Files, FilesState};

fn put(out: &mut Vec<u8>, values: &[u32]) {
    for value in values {
        out.extend(value.to_le_bytes());
    }
}

/// How a column is stored, as a table writes it.
#[derive(Clone, Copy)]
struct Storage {
    offset_bits: u16,
    size_bits: u16,
    additional: u32,
    compression: u32,
}

fn storage(offset_bits: u16, size_bits: u16, compression: u32) -> Storage {
    Storage {
        offset_bits,
        size_bits,
        additional: 0,
        compression,
    }
}

/// The fields of a table, 32 bits each, by their place in the record.
fn put_fields(out: &mut Vec<u8>, columns: &[Storage]) {
    for column in columns {
        out.extend(0i16.to_le_bytes());
        out.extend((column.offset_bits / 8).to_le_bytes());
    }
}

fn put_storage(out: &mut Vec<u8>, columns: &[Storage]) {
    for column in columns {
        out.extend(column.offset_bits.to_le_bytes());
        out.extend(column.size_bits.to_le_bytes());
        put(out, &[column.additional, column.compression, 0, 0, 0]);
    }
}

/// The strings of a table: an empty one first, each then once.
struct Strings {
    bytes: Vec<u8>,
    at: HashMap<String, u32>,
}

impl Strings {
    fn new() -> Self {
        Self {
            bytes: vec![0],
            at: HashMap::new(),
        }
    }

    fn add(&mut self, text: &str) -> u32 {
        if text.is_empty() {
            return 0;
        }
        if let Some(at) = self.at.get(text) {
            return *at;
        }
        let at = self.bytes.len() as u32;
        self.bytes.extend(text.as_bytes());
        self.bytes.push(0);
        self.at.insert(text.to_owned(), at);
        at
    }
}

/// How a test table keeps its ids.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Ids {
    Plain,
    Bitpacked,
    Signed,
    Pallet,
    Common,
    List,
    Zeros,
}

/// A WDC1 as DB2Gen writes it: its records, each an id and the offset of its path from the start
/// of `strings`, or only the offset with the ids in a list; then its copies.
fn wdc1_raw(records: &[(u32, u32)], strings: &[u8], listed: bool, copies: &[(u32, u32)]) -> Vec<u8> {
    let columns = if listed {
        vec![storage(0, 32, NONE)]
    } else {
        vec![storage(0, 32, NONE), storage(32, 32, NONE)]
    };
    let record_size = columns.len() as u32 * 4;
    let min_id = records.iter().map(|row| row.0).min().unwrap_or(0);
    let max_id = records.iter().map(|row| row.0).max().unwrap_or(0);
    let mut out = vec![0u8; 84];
    put_fields(&mut out, &columns);
    for (id, offset) in records {
        if !listed {
            put(&mut out, &[*id]);
        }
        put(&mut out, &[*offset]);
    }
    out.extend(strings);
    if listed {
        put(&mut out, &records.iter().map(|row| row.0).collect::<Vec<u32>>());
    }
    for (copy, copied) in copies {
        put(&mut out, &[*copy, *copied]);
    }
    put_storage(&mut out, &columns);
    let mut header = b"WDC1".to_vec();
    let count = columns.len() as u32;
    put(
        &mut header,
        &[
            records.len() as u32,
            count,
            record_size,
            strings.len() as u32,
            0,
            0,
            min_id,
            max_id,
            0,
            copies.len() as u32 * 8,
        ],
    );
    header.extend(0u16.to_le_bytes());
    header.extend(0u16.to_le_bytes());
    let ids_size = if listed { records.len() as u32 * 4 } else { 0 };
    put(&mut header, &[count, record_size, 0, 0, ids_size, count * 24, 0, 0, 0]);
    out[..84].copy_from_slice(&header);
    out
}

/// A WDC1 as DB2Gen writes it, of `rows` and `copies`, its ids plain or listed.
fn wdc1(rows: &[(u32, &str)], ids: Ids, copies: &[(u32, u32)]) -> Vec<u8> {
    assert!(matches!(ids, Ids::Plain | Ids::List));
    let mut strings = Strings::new();
    let records: Vec<(u32, u32)> = rows.iter().map(|(id, path)| (*id, strings.add(path))).collect();
    wdc1_raw(&records, &strings.bytes, ids == Ids::List, copies)
}

/// A DB2 of WDC2 or WDC3: its sections, how it keeps its ids, a section encrypted with a key the
/// client lacks, the copies of its last section.
struct Wdc<'a> {
    magic: &'a [u8; 4],
    sections: Vec<Vec<(u32, &'a str)>>,
    ids: Ids,
    encrypted: Option<usize>,
    copies: Vec<(u32, u32)>,
}

fn wdc<'a>(magic: &'a [u8; 4], sections: &[&[(u32, &'a str)]]) -> Wdc<'a> {
    Wdc {
        magic,
        sections: sections.iter().map(|rows| rows.to_vec()).collect(),
        ids: Ids::Plain,
        encrypted: None,
        copies: Vec::new(),
    }
}

impl Wdc<'_> {
    fn write(&self) -> Vec<u8> {
        let version2 = matches!(self.magic, b"WDC2" | b"1SLC");
        let all: Vec<(u32, &str)> = self.sections.concat();
        let id_bits = if self.ids == Ids::Pallet { 3 } else { 20 };
        // A packed id 3 bits into the second column, which shifts its bits.
        let packed = |compression| storage(35, id_bits, compression);
        let columns = match self.ids {
            Ids::Plain => vec![storage(0, 32, NONE), storage(32, 32, NONE)],
            Ids::List | Ids::Zeros => vec![storage(0, 32, NONE)],
            Ids::Bitpacked => vec![storage(0, 32, NONE), packed(BITPACKED)],
            Ids::Signed => vec![storage(0, 32, NONE), packed(SIGNED)],
            Ids::Pallet => vec![
                storage(0, 32, NONE),
                Storage {
                    additional: all.len() as u32 * 4,
                    ..packed(PALLET)
                },
            ],
            Ids::Common => vec![
                storage(0, 32, NONE),
                Storage {
                    additional: 8,
                    ..storage(32, 0, COMMON)
                },
            ],
        };
        // The path is the column after the plain id, the first otherwise.
        let (id_index, path_byte) = match self.ids {
            Ids::Plain => (0u16, 4),
            Ids::List | Ids::Zeros => (0, 0),
            _ => (1, 0),
        };
        let record_size = columns.len() * 4;
        let min_id = all.iter().map(|row| row.0).min().unwrap_or(0);
        let max_id = all.iter().map(|row| row.0).max().unwrap_or(0);
        let mut strings: Vec<Strings> = self
            .sections
            .iter()
            .map(|rows| {
                let mut strings = Strings::new();
                for (_, path) in rows {
                    strings.add(path);
                }
                strings
            })
            .collect();
        let mut out = self.magic.to_vec();
        let header_at = out.len();
        out.extend([0u8; 68]);
        let headers_at = out.len();
        let header_size = if version2 { 36 } else { 40 };
        out.extend(vec![0u8; header_size * self.sections.len()]);
        put_fields(&mut out, &columns);
        put_storage(&mut out, &columns);
        let mut pallet_size = 0;
        let mut common_size = 0;
        if self.ids == Ids::Pallet {
            put(&mut out, &all.iter().map(|row| row.0).collect::<Vec<u32>>());
            pallet_size = all.len() as u32 * 4;
        }
        if self.ids == Ids::Common {
            put(&mut out, &[all[0].0, 7]);
            common_size = 8;
        }
        let mut headers = Vec::new();
        let mut outside = 0;
        let mut base = 0;
        for (place, rows) in self.sections.iter().enumerate() {
            let file_offset = out.len();
            let encrypted = self.encrypted == Some(place);
            for (index, (id, path)) in rows.iter().enumerate() {
                let mut record = vec![0u8; record_size];
                // The offset of a path among the strings of all the sections, end to end.
                let local = strings[place].add(path) as usize;
                let offset = if local == 0 { 0 } else { base + local };
                record[path_byte..path_byte + 4].copy_from_slice(&(offset as u32).to_le_bytes());
                let packed = match self.ids {
                    Ids::Plain => {
                        record[..4].copy_from_slice(&id.to_le_bytes());
                        None
                    }
                    Ids::Bitpacked | Ids::Signed => Some(id & 0xF_FFFF),
                    Ids::Pallet => Some((outside + index) as u32),
                    _ => None,
                };
                if let Some(value) = packed {
                    record[4..8].copy_from_slice(&(value << 3).to_le_bytes());
                }
                if encrypted {
                    record.fill(0);
                }
                out.extend(record);
            }
            out.extend(&strings[place].bytes);
            let ids: Vec<u32> = match self.ids {
                Ids::List => rows.iter().map(|row| row.0).collect(),
                Ids::Zeros => vec![0; rows.len()],
                _ => Vec::new(),
            };
            put(&mut out, &ids);
            let copies = if place + 1 == self.sections.len() {
                self.copies.clone()
            } else {
                Vec::new()
            };
            for (copy, copied) in &copies {
                put(&mut out, &[*copy, *copied]);
            }
            let key: u64 = if encrypted { 0x1234_5678_9ABC_DEF0 } else { 0 };
            headers.extend(key.to_le_bytes());
            let strings_size = strings[place].bytes.len();
            put(
                &mut headers,
                &[file_offset as u32, rows.len() as u32, strings_size as u32],
            );
            if version2 {
                put(&mut headers, &[copies.len() as u32 * 8, 0, ids.len() as u32 * 4, 0]);
            } else {
                let records_end = file_offset + rows.len() * record_size;
                put(
                    &mut headers,
                    &[records_end as u32, ids.len() as u32 * 4, 0, 0, copies.len() as u32],
                );
            }
            outside += rows.len();
            base += strings_size;
        }
        let mut header = Vec::new();
        let count = columns.len() as u32;
        put(
            &mut header,
            &[
                all.len() as u32,
                count,
                record_size as u32,
                base as u32,
                0,
                0,
                min_id,
                max_id,
                0,
            ],
        );
        header.extend(0u16.to_le_bytes());
        header.extend(id_index.to_le_bytes());
        put(
            &mut header,
            &[
                count,
                record_size as u32,
                0,
                count * 24,
                common_size,
                pallet_size,
                self.sections.len() as u32,
            ],
        );
        out[header_at..header_at + 68].copy_from_slice(&header);
        out[headers_at..headers_at + headers.len()].copy_from_slice(&headers);
        out
    }
}

/// Whether `table` names each of `rows` by its id, an empty path naming nothing, and nothing else.
fn names(table: &PathTable, rows: &[(u32, &str)]) {
    for (id, path) in rows {
        let expected = (!path.is_empty()).then(|| path.to_string());
        assert_eq!(table.path(*id), expected, "the file {id}");
    }
    let named = rows.iter().filter(|row| !row.1.is_empty()).count();
    assert_eq!(table.len(), named);
}

fn parse(bytes: Vec<u8>) -> PathTable {
    PathTable::parse(bytes).unwrap()
}

fn refused(bytes: Vec<u8>) -> String {
    PathTable::parse(bytes).err().expect("refused")
}

const ROWS: [(u32, &str); 5] = [
    (10, "world\\a.blp"),
    (3, "world\\b.blp"),
    (7, ""),
    (12, "world\\a.blp"),
    (5_000_000, "character\\human\\male\\humanmale.blp"),
];

#[test]
fn a_wdc1_as_db2gen_writes_it_names_each_file_by_its_id() {
    let table = parse(wdc1(&ROWS, Ids::Plain, &[]));
    names(&table, &ROWS);
    assert_eq!(table.path(11), None);
    names(&parse(wdc1(&ROWS, Ids::List, &[])), &ROWS);
}

#[test]
fn an_id_named_twice_keeps_its_last_row_copies_included_as_warcraftxl_does() {
    let twice = parse(wdc1(&[(3, "first.blp"), (3, "second.blp")], Ids::Plain, &[]));
    assert_eq!(twice.path(3).as_deref(), Some("second.blp"));
    let emptied = parse(wdc1(&[(3, "first.blp"), (3, "")], Ids::Plain, &[]));
    assert_eq!(emptied.path(3), None, "an empty last row names nothing");
    assert_eq!(emptied.len(), 0);
    let copied = parse(wdc1(&ROWS, Ids::Plain, &[(20, 3), (21, 7), (22, 999), (10, 3)]));
    assert_eq!(copied.path(20).as_deref(), Some("world\\b.blp"));
    assert_eq!(copied.path(21), None, "a copy of an empty path names nothing");
    assert_eq!(copied.path(22), None, "a copy of a row missing names nothing");
    assert_eq!(
        copied.path(10).as_deref(),
        Some("world\\b.blp"),
        "a copy comes after the rows"
    );
}

#[test]
fn an_offset_counts_from_the_start_of_the_strings_as_warcraftxl_reads_it() {
    let first = parse(wdc1_raw(&[(5, 0), (6, 2)], b"a.blp\0", false, &[]));
    assert_eq!(first.path(5).as_deref(), Some("a.blp"), "0 is the first string");
    assert_eq!(first.path(6).as_deref(), Some("blp"), "an offset inside a string");
    let outside = parse(wdc1_raw(&[(5, 999)], b"\0a.blp\0", false, &[]));
    assert_eq!(outside.path(5), None, "an offset out of the strings names nothing");
    let first_part: &[(u32, &str)] = &ROWS[..3];
    let second: &[(u32, &str)] = &[(40, "world\\c.m2"), (41, "world\\c.m2"), (42, "")];
    let all = [first_part, second].concat();
    for magic in [b"WDC2", b"1SLC", b"WDC3"] {
        let table = PathTable::parse(wdc(magic, &[first_part, second]).write())
            .unwrap_or_else(|e| panic!("{}: {e}", String::from_utf8_lossy(magic)));
        names(&table, &all);
    }
}

#[test]
fn ids_packed_signed_in_a_pallet_or_listed_read_as_warcraftxl_reads_them() {
    let rows: &[(u32, &str)] = &[(70_000, "a.blp"), (3, "b.blp"), (900_000, "c.blp")];
    for ids in [Ids::Bitpacked, Ids::Pallet, Ids::List] {
        let table = Wdc {
            ids,
            ..wdc(b"WDC3", &[rows])
        };
        names(&parse(table.write()), rows);
    }
    let signed_rows: &[(u32, &str)] = &[(5, "a.blp"), (0x8_0001, "b.blp")];
    let signed = Wdc {
        ids: Ids::Signed,
        ..wdc(b"WDC3", &[signed_rows])
    };
    names(&parse(signed.write()), &[(5, "a.blp"), (0xFFF8_0001, "b.blp")]);
    let zeros = Wdc {
        ids: Ids::Zeros,
        ..wdc(b"WDC3", &[rows])
    };
    let zeros = parse(zeros.write());
    assert_eq!(
        zeros.path(0).as_deref(),
        Some("c.blp"),
        "a list of zeros gives the id 0 to each"
    );
    assert_eq!(zeros.len(), 1);
    let common = Wdc {
        ids: Ids::Common,
        ..wdc(b"WDC3", &[rows])
    };
    let reason = refused(common.write());
    assert!(reason.contains("common data"), "{reason}");
}

#[test]
fn a_section_encrypted_with_a_key_the_client_lacks_names_nothing() {
    let first: &[(u32, &str)] = &[(1, "a.blp"), (2, "b.blp")];
    let second: &[(u32, &str)] = &[(3, "c.blp")];
    for ids in [Ids::Plain, Ids::List] {
        let table = Wdc {
            encrypted: Some(0),
            ids,
            ..wdc(b"WDC3", &[first, second])
        };
        names(&parse(table.write()), second);
    }
}

#[test]
fn what_warcraftxl_does_not_read_is_refused_and_a_file_damaged_never_panics() {
    let mut magic = wdc(b"WDC3", &[&ROWS[..]]).write();
    for name in [b"WDC4", b"WDC5"] {
        magic[..4].copy_from_slice(name);
        let reason = refused(magic.clone());
        assert!(reason.starts_with(std::str::from_utf8(name).unwrap()), "{reason}");
    }
    assert!(PathTable::parse(b"WDBC\0\0\0\0".to_vec()).is_err());
    // The flag of records of variable size, then the size of the relations, in WDC1 and WDC3.
    for (mut bytes, flags, relations) in [
        (wdc1(&ROWS, Ids::Plain, &[]), 44, 80),
        (wdc(b"WDC3", &[&ROWS[..]]).write(), 40, 100),
    ] {
        let mut sparse = bytes.clone();
        sparse[flags] = 1;
        assert!(refused(sparse).contains("variable size"));
        bytes[relations] = 12;
        assert!(refused(bytes).contains("relations"));
    }
    // A list of one id after the strings, the last bytes of the file, beside the column of ids.
    let mut listed = wdc(b"WDC3", &[&ROWS[..]]).write();
    listed.extend(10u32.to_le_bytes());
    listed[72 + 8 + 4 * 4] = 4;
    assert!(refused(listed).contains("2 columns and a list of ids"));
    // Its list of ids said empty, a table of one column has no ids.
    let mut unlisted = Wdc {
        ids: Ids::List,
        ..wdc(b"WDC3", &[&ROWS[..]])
    }
    .write();
    unlisted[72 + 8 + 4 * 4] = 0;
    assert!(refused(unlisted).contains("1 columns without a list of ids"));
    let mut pallet = Wdc {
        ids: Ids::Pallet,
        ..wdc(b"WDC3", &[&ROWS[..]])
    }
    .write();
    // The size of the pallets in the header, 4 bytes more than they hold.
    let size = u32::from_le_bytes(pallet[64..68].try_into().unwrap());
    pallet[64..68].copy_from_slice(&(size + 4).to_le_bytes());
    assert!(refused(pallet).contains("pallets"));
    let samples = [
        wdc1(&ROWS, Ids::Plain, &[(20, 3)]),
        wdc(b"WDC2", &[&ROWS[..]]).write(),
        Wdc {
            ids: Ids::Pallet,
            ..wdc(b"WDC3", &[&ROWS[..3], &ROWS[3..]])
        }
        .write(),
        Wdc {
            ids: Ids::List,
            ..wdc(b"WDC3", &[&ROWS[..2], &ROWS[2..]])
        }
        .write(),
    ];
    for sample in samples {
        for len in 0..sample.len() {
            assert!(PathTable::parse(sample[..len].to_vec()).is_err(), "cut at {len}");
        }
        for at in 0..sample.len() {
            for value in [0x00, 0x7F, 0xFF] {
                let mut damaged = sample.clone();
                damaged[at] = value;
                if let Ok(table) = PathTable::parse(damaged) {
                    for (id, _) in ROWS {
                        let _ = table.path(id);
                    }
                }
            }
        }
    }
}

#[test]
fn the_tables_of_paths_are_read_loose_first_then_from_the_archives_textures_before_models() {
    let folder = scratch("file-ids");
    let textures = wdc1(&[(1, "a.blp"), (5, "e.blp"), (6, "")], Ids::Plain, &[]);
    let model_rows: &[(u32, &str)] = &[(1, "m.m2"), (2, "n.m2"), (6, "o.m2"), (0, "zero.m2")];
    let models = wdc(b"WDC3", &[model_rows]).write();
    let archive = folder.join("patch.mpq");
    write_archive(
        &archive,
        0,
        &[
            file("DBFilesClient\\TextureFilePath.db2", b"WDC9", Stored::Plain),
            file("DBFilesClient\\ModelFilePath.db2", &models, Stored::Plain),
        ],
    );
    std::fs::create_dir_all(folder.join("DBFilesClient")).unwrap();
    let loose = folder.join("DBFilesClient").join("TextureFilePath.db2");
    std::fs::write(&loose, &textures).unwrap();
    let chain = Chain::new(vec![Source::open(&archive).unwrap()]);

    let (ids, refused) = FileIds::load(&folder, &chain);
    assert!(refused.is_empty(), "{refused:?}");
    assert_eq!(ids.path_of(1).as_deref(), Some("a.blp"), "the textures first");
    assert_eq!(ids.path_of(2).as_deref(), Some("n.m2"));
    assert_eq!(ids.path_of(5).as_deref(), Some("e.blp"));
    assert_eq!(
        ids.path_of(6).as_deref(),
        Some("o.m2"),
        "an empty texture leaves the model"
    );
    assert_eq!(ids.path_of(0), None, "0 names no file");
    assert_eq!(ids.path_of(3), None);
    assert_eq!(ids.len(), 6);

    std::fs::write(&loose, b"").unwrap();
    let (ids, refused) = FileIds::load(&folder, &chain);
    assert_eq!(
        refused.len(),
        1,
        "an empty loose table leaves that of the archives: {refused:?}"
    );
    assert!(refused[0].starts_with("TextureFilePath.db2: "), "{refused:?}");
    assert_eq!(ids.path_of(1).as_deref(), Some("m.m2"));

    let (ids, refused) = FileIds::load(&folder, &Chain::new(Vec::new()));
    assert!(refused.is_empty() && ids.len() == 0, "no table, nothing refused");
    let _ = std::fs::remove_dir_all(folder);
}

/// A cell of a test DBC.
#[derive(Clone, Copy)]
enum Cell<'a> {
    Int(u32),
    Float(f32),
    Text(&'a str),
}

/// A DBC of `columns` columns, the cells of each row by their column, the others zero.
fn dbc(columns: usize, rows: &[&[(usize, Cell)]]) -> Vec<u8> {
    let mut strings = Strings::new();
    let mut records = Vec::new();
    for row in rows {
        let mut values = vec![0u32; columns];
        for (column, cell) in *row {
            values[*column] = match cell {
                Cell::Int(value) => *value,
                Cell::Float(value) => value.to_bits(),
                Cell::Text(text) => strings.add(text),
            };
        }
        put(&mut records, &values);
    }
    let mut out = b"WDBC".to_vec();
    put(
        &mut out,
        &[
            rows.len() as u32,
            columns as u32,
            columns as u32 * 4,
            strings.bytes.len() as u32,
        ],
    );
    out.extend(records);
    out.extend(&strings.bytes);
    out
}

/// A folder holding an archive, `common.mpq`, of `tables` in `DBFilesClient`, and its chain.
fn tables_chain(name: &str, tables: &[(&str, Vec<u8>)]) -> (PathBuf, Chain) {
    let folder = scratch(name);
    let archive = folder.join("common.mpq");
    let names: Vec<String> = tables
        .iter()
        .map(|(table, _)| format!("DBFilesClient\\{table}"))
        .collect();
    let files: Vec<_> = names
        .iter()
        .zip(tables)
        .map(|(name, (_, bytes))| file(name, bytes, Stored::Plain))
        .collect();
    write_archive(&archive, 0, &files);
    let chain = Chain::new(vec![Source::open(&archive).unwrap()]);
    (folder, chain)
}

fn sample_tables() -> Vec<(&'static str, Vec<u8>)> {
    use Cell::*;
    vec![
        (
            "Map.dbc",
            dbc(
                66,
                &[
                    &[
                        (0, Int(1)),
                        (1, Text("Kalimdor")),
                        (5, Text("Kalimdor")),
                        (7, Text("Kalimdor (fr)")),
                        (22, Int(14)),
                    ],
                    &[
                        (0, Int(0)),
                        (1, Text("Azeroth")),
                        (5, Text("Eastern Kingdoms")),
                        (7, Text("Royaumes de l'Est")),
                    ],
                    &[
                        (0, Int(33)),
                        (1, Text("Shadowfang")),
                        (2, Int(1)),
                        (7, Text("Ombrecroc")),
                        (22, Int(209)),
                    ],
                ],
            ),
        ),
        (
            "AreaTable.dbc",
            dbc(
                36,
                &[
                    &[
                        (0, Int(1)),
                        (4, Int(0x40)),
                        (11, Text("Dun Morogh")),
                        (13, Text("Dun Morogh (fr)")),
                    ],
                    &[(0, Int(131)), (2, Int(1)), (13, Text("Kharanos"))],
                ],
            ),
        ),
        (
            "CreatureDisplayInfo.dbc",
            dbc(
                16,
                &[&[
                    (0, Int(49)),
                    (1, Int(50)),
                    (3, Int(7)),
                    (4, Float(1.5)),
                    (5, Int(128)),
                    (6, Text("WolfSkinGrey")),
                    (8, Text("WolfSkinBlack")),
                    (14, Int(0x21)),
                ]],
            ),
        ),
        (
            "CreatureDisplayInfoExtra.dbc",
            dbc(
                21,
                &[
                    &[
                        (0, Int(9)),
                        (1, Int(3)),
                        (2, Int(1)),
                        (5, Int(4)),
                        (7, Int(2)),
                        (18, Int(77)),
                    ],
                    &[
                        (0, Int(7)),
                        (1, Int(1)),
                        (3, Int(2)),
                        (4, Int(3)),
                        (5, Int(5)),
                        (6, Int(6)),
                        (7, Int(1)),
                        (8, Int(11)),
                        (19, Int(0x10)),
                        (20, Text("HumanGuard.blp")),
                    ],
                ],
            ),
        ),
        (
            "CharHairGeosets.dbc",
            dbc(
                6,
                &[
                    &[(0, Int(2)), (1, Int(1)), (3, Int(5)), (4, Int(3))],
                    &[(0, Int(1)), (1, Int(1)), (3, Int(0)), (5, Int(1))],
                ],
            ),
        ),
        (
            "CharacterFacialHairStyles.dbc",
            dbc(
                8,
                &[
                    &[(0, Int(1)), (2, Int(1)), (3, Int(1)), (4, Int(2)), (7, Int(3))],
                    &[(0, Int(1)), (2, Int(0))],
                ],
            ),
        ),
        (
            "GameObjectDisplayInfo.dbc",
            dbc(19, &[&[(0, Int(31)), (1, Text("World\\Generic\\Chest.mdx"))]]),
        ),
        (
            "CharSections.dbc",
            dbc(
                10,
                &[
                    &[
                        (0, Int(2)),
                        (1, Int(1)),
                        (3, Int(3)),
                        (4, Text("Hair02_04.blp")),
                        (8, Int(2)),
                        (9, Int(4)),
                    ],
                    &[(0, Int(1)), (1, Int(1)), (3, Int(0)), (5, Text("Skin.blp"))],
                ],
            ),
        ),
        (
            "CreatureModelData.dbc",
            dbc(
                28,
                &[&[
                    (0, Int(50)),
                    (1, Int(1)),
                    (2, Text("Creature\\Wolf\\Wolf.mdx")),
                    (4, Float(0.75)),
                ]],
            ),
        ),
    ]
}

#[test]
fn the_tables_of_3_3_5a_read_their_rows_in_the_client_s_locale() {
    let (folder, chain) = tables_chain("dbc", &sample_tables());
    let tables = Tables::new("frFR");
    let maps = tables.maps(&chain).unwrap();
    let ids: Vec<u32> = maps.iter().map(|map| map.id).collect();
    assert_eq!(ids, [0, 1, 33], "by increasing id");
    assert_eq!(maps[0].directory, "Azeroth");
    assert_eq!(maps[0].name, "Royaumes de l'Est");
    assert_eq!((maps[1].area, maps[2].instance_type), (14, 1));
    assert!(Arc::ptr_eq(&maps, &tables.maps(&chain).unwrap()), "read once");
    let areas = tables.areas(&chain).unwrap();
    assert_eq!((areas[0].name.as_str(), areas[0].flags), ("Dun Morogh (fr)", 0x40));
    assert_eq!(
        (areas[1].map, areas[1].parent, areas[1].name.as_str()),
        (0, 1, "Kharanos")
    );
    let display = &tables.creature_displays(&chain).unwrap()[0];
    assert_eq!(
        (display.id, display.model, display.extra, display.scale),
        (49, 50, 7, 1.5)
    );
    assert_eq!((display.alpha, display.geosets), (128, 0x21));
    let looks = tables.creature_looks(&chain).unwrap();
    assert_eq!(
        looks.iter().map(|look| look.id).collect::<Vec<_>>(),
        [7, 9],
        "by increasing id"
    );
    let look = &looks[0];
    assert_eq!(
        (
            look.race,
            look.sex,
            look.skin,
            look.face,
            look.hair_style,
            look.hair_colour,
            look.facial_hair
        ),
        (1, 0, 2, 3, 5, 6, 1)
    );
    assert_eq!((look.items[0], look.items[10], look.flags), (11, 0, 0x10));
    assert_eq!(look.baked, "HumanGuard.blp");
    assert_eq!(looks[1].items[10], 77, "its cape");
    let hairs = tables.hair_geosets(&chain).unwrap();
    assert_eq!(
        hairs
            .iter()
            .map(|hair| (hair.variation, hair.geoset, hair.scalp))
            .collect::<Vec<_>>(),
        [(0, 0, true), (5, 3, false)],
        "by race, sex and variation"
    );
    let facials = tables.facial_hairs(&chain).unwrap();
    assert_eq!(
        facials
            .iter()
            .map(|facial| (facial.variation, facial.geosets))
            .collect::<Vec<_>>(),
        [(0, [0; 5]), (1, [1, 2, 0, 0, 3])]
    );
    let object = &tables.game_object_displays(&chain).unwrap()[0];
    assert_eq!((object.id, object.path.as_str()), (31, "World\\Generic\\Chest.mdx"));
    let sections = tables.char_sections(&chain).unwrap();
    assert_eq!(
        sections
            .iter()
            .map(|section| (
                section.section,
                section.variation,
                section.colour,
                section.textures.clone()
            ))
            .collect::<Vec<_>>(),
        [
            (0, 0, 0, [String::new(), "Skin.blp".to_owned(), String::new()]),
            (3, 2, 4, ["Hair02_04.blp".to_owned(), String::new(), String::new()]),
        ],
        "by race, sex, section, variation and colour"
    );
    assert_eq!(
        display.textures,
        ["WolfSkinGrey".to_owned(), String::new(), "WolfSkinBlack".to_owned()]
    );
    let model = &tables.creature_models(&chain).unwrap()[0];
    assert_eq!(
        (model.id, model.flags, model.path.as_str(), model.scale),
        (50, 1, "Creature\\Wolf\\Wolf.mdx", 0.75)
    );

    let english = Tables::new("enGB");
    assert_eq!(
        english.maps(&chain).unwrap()[0].name,
        "Eastern Kingdoms",
        "enGB reads the first"
    );
    assert_eq!(
        english.maps(&chain).unwrap()[2].name,
        "",
        "a text missing in the locale stays empty"
    );
    let _ = std::fs::remove_dir_all(folder);
}

#[test]
fn a_table_of_another_layout_or_damaged_is_refused_with_its_name() {
    use Cell::*;
    let mut out_of_strings = dbc(28, &[&[(0, Int(1)), (2, Text("a.mdx"))]]);
    out_of_strings[20 + 8..20 + 12].copy_from_slice(&999u32.to_le_bytes());
    let (folder, chain) = tables_chain(
        "dbc-refused",
        &[
            ("Map.dbc", dbc(65, &[&[(0, Int(0))]])),
            ("AreaTable.dbc", dbc(36, &[&[(0, Int(1))]])[..40].to_vec()),
            ("CreatureModelData.dbc", out_of_strings),
        ],
    );
    let tables = Tables::new("enUS");
    let refused = tables.maps(&chain).err().unwrap();
    assert!(refused.starts_with("DBFilesClient\\Map.dbc: 65 columns"), "{refused}");
    assert!(tables.areas(&chain).err().unwrap().contains("cut short"));
    assert!(
        tables
            .creature_models(&chain)
            .err()
            .unwrap()
            .contains("a string at 999")
    );
    let missing = tables.creature_displays(&chain).err().unwrap();
    assert!(missing.contains("not in the client"), "{missing}");
    let _ = std::fs::remove_dir_all(folder);
}

#[test]
fn the_services_answer_once_the_client_is_open() {
    let mut tables = sample_tables();
    tables.push(("ModelFilePath.db2", wdc1(&[(8, "world\\x.m2")], Ids::Plain, &[])));
    let (folder, _) = tables_chain("services", &tables);
    let files = Files::default();
    assert_eq!(files.path_of(8), None);
    assert!(files.maps().is_err());
    let sources = vec![Source::open(&folder.join("common.mpq")).unwrap()];
    let (client, refused) = Client::open(sources, &folder, "enUS");
    assert!(refused.is_empty(), "{refused:?}");
    files.set(FilesState::Ready(Arc::new(client)));
    assert_eq!(files.path_of(8).as_deref(), Some("world\\x.m2"));
    assert_eq!(files.maps().unwrap()[0].name, "Eastern Kingdoms");
    assert_eq!(files.creature_models().unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(folder);
}

#[test]
fn a_table_asked_from_the_interface_thread_is_said_once_in_debug() {
    let files = Files::default();
    files.interface.set(std::thread::current().id()).unwrap();
    std::thread::scope(|scope| {
        scope.spawn(|| files.maps());
    });
    assert!(
        !files.warned.load(std::sync::atomic::Ordering::Relaxed),
        "another thread says nothing"
    );
    let _ = files.maps();
    assert_eq!(
        files.warned.load(std::sync::atomic::Ordering::Relaxed),
        cfg!(debug_assertions)
    );
}

#[test]
fn a_table_of_a_million_paths_reads_in_a_moment() {
    let names: Vec<String> = (0..1_000_000)
        .map(|id| format!("world\\textures\\{id:07}.blp"))
        .collect();
    let rows: Vec<(u32, &str)> = names
        .iter()
        .enumerate()
        .map(|(id, name)| (id as u32 * 3, name.as_str()))
        .collect();
    let bytes = wdc1(&rows, Ids::Plain, &[]);
    let start = Instant::now();
    let table = parse(bytes);
    let read = start.elapsed();
    assert_eq!(table.len(), rows.len());
    assert_eq!(table.path(2_999_997).as_deref(), Some("world\\textures\\0999999.blp"));
    eprintln!("{} rows read in {read:?}", table.len());
}

/// The client named by `UNIWOW_CLIENT`: its folder, locale and archives open, or none.
fn client() -> Option<(PathBuf, String, Chain)> {
    let Ok(folder) = std::env::var("UNIWOW_CLIENT") else {
        eprintln!("skipped: UNIWOW_CLIENT names no client folder");
        return None;
    };
    let folder = PathBuf::from(folder);
    let locale = chain::locale(&folder).unwrap();
    let sources = chain::order(&folder, &locale)
        .iter()
        .map(|path| Source::open(path).unwrap())
        .collect();
    Some((folder, locale, Chain::new(sources)))
}

#[test]
fn the_tables_of_the_lights_read_their_lights_params_bands_and_skies() {
    use Cell::*;
    // A band: its id, its count of keys, then its 16 times and its 16 values.
    let band = |id: u32, count: u32, keys: &[(u32, u32)]| -> Vec<(usize, Cell<'static>)> {
        let mut cells = vec![(0, Int(id)), (1, Int(count))];
        for (at, (time, value)) in keys.iter().enumerate() {
            cells.push((2 + at, Int(*time)));
            cells.push((18 + at, Int(*value)));
        }
        cells
    };
    let colours = [
        band(199, 2, &[(0, 0x00FF_8800), (1440, 0x0010_2030), (2000, 0x00FF_FFFF)]),
        band(200, 40, &[(5, 1)]),
    ];
    let numbers = [band(67, 1, &[(0, 18_000f32.to_bits()), (720, 1)])];
    let tables = vec![
        (
            "Light.dbc",
            dbc(
                15,
                &[
                    &[
                        (0, Int(2)),
                        (2, Float(612_096.0)),
                        (4, Float(998_400.0)),
                        (5, Float(3_600.0)),
                        (6, Float(7_200.0)),
                        (7, Int(20)),
                    ],
                    &[
                        (0, Int(1)),
                        (7, Int(12)),
                        (8, Int(13)),
                        (9, Int(10)),
                        (10, Int(13)),
                        (11, Int(4)),
                    ],
                ],
            ),
        ),
        (
            "LightParams.dbc",
            dbc(
                9,
                &[&[
                    (0, Int(12)),
                    (1, Int(1)),
                    (2, Int(83)),
                    (4, Float(0.65)),
                    (5, Float(0.5)),
                    (6, Float(1.0)),
                    (7, Float(0.75)),
                    (8, Float(1.0)),
                ]],
            ),
        ),
        (
            "LightIntBand.dbc",
            dbc(34, &colours.iter().map(Vec::as_slice).collect::<Vec<_>>()),
        ),
        (
            "LightFloatBand.dbc",
            dbc(34, &numbers.iter().map(Vec::as_slice).collect::<Vec<_>>()),
        ),
        (
            "LightSkybox.dbc",
            dbc(
                3,
                &[&[
                    (0, Int(83)),
                    (1, Text("Environments\\Stars\\DeathSkybox.mdx")),
                    (2, Int(2)),
                ]],
            ),
        ),
    ];
    let (_folder, chain) = tables_chain("lights", &tables);
    let tables = Tables::new("enUS");
    let lights = tables.lights(&chain).unwrap();
    assert_eq!(lights.iter().map(|light| light.id).collect::<Vec<_>>(), [1, 2], "by id");
    assert_eq!(
        (lights[0].map, lights[0].position, lights[0].params),
        (0, [0.0; 3], [12, 13, 10, 13, 4, 0, 0, 0])
    );
    assert_eq!(
        (lights[1].position, lights[1].radii),
        ([612_096.0, 0.0, 998_400.0], [3_600.0, 7_200.0])
    );
    let params = &tables.light_params(&chain).unwrap()[0];
    assert!(params.highlight_sky);
    assert_eq!((params.skybox, params.cloud, params.glow), (83, 0, 0.65));
    assert_eq!((params.river_alphas, params.ocean_alphas), ([0.5, 1.0], [0.75, 1.0]));
    // The keys past the count of a band left out, its count no more than 16; red first.
    let colours = tables.light_colours(&chain).unwrap();
    assert_eq!(colours[0].keys, [(0, [255, 136, 0]), (1440, [16, 32, 48])]);
    assert_eq!(colours[1].keys.len(), 16);
    let numbers = tables.light_numbers(&chain).unwrap();
    assert_eq!(numbers[0].keys, [(0, 18_000.0)]);
    let sky = &tables.light_skyboxes(&chain).unwrap()[0];
    assert_eq!(
        (sky.id, sky.model.as_str(), sky.flags),
        (83, "Environments\\Stars\\DeathSkybox.mdx", 2)
    );
}

#[test]
fn the_client_s_tables_read_as_the_client_shows_them() {
    let Some((folder, locale, chain)) = client() else {
        return;
    };
    let tables = Tables::new(&locale);
    let start = Instant::now();
    let maps = tables.maps(&chain).unwrap();
    let areas = tables.areas(&chain).unwrap();
    let displays = tables.creature_displays(&chain).unwrap();
    let models = tables.creature_models(&chain).unwrap();
    let looks = tables.creature_looks(&chain).unwrap();
    let hairs = tables.hair_geosets(&chain).unwrap();
    let facials = tables.facial_hairs(&chain).unwrap();
    let objects = tables.game_object_displays(&chain).unwrap();
    let sections = tables.char_sections(&chain).unwrap();
    let lights = tables.lights(&chain).unwrap();
    let light_params = tables.light_params(&chain).unwrap();
    let colours = tables.light_colours(&chain).unwrap();
    let numbers = tables.light_numbers(&chain).unwrap();
    let skies = tables.light_skyboxes(&chain).unwrap();
    let read = start.elapsed();
    // The global light of the Eastern Kingdoms, its params with their 18 colours and 6 numbers.
    let global = lights.iter().find(|light| light.id == 1).unwrap();
    assert_eq!((global.map, global.position), (0, [0.0; 3]));
    let clear = global.params[0];
    assert!(light_params.iter().any(|params| params.id == clear));
    assert!((0..18).all(|band| colours.iter().any(|found| found.id == clear * 18 - 17 + band)));
    assert!((0..6).all(|band| numbers.iter().any(|found| found.id == clear * 6 - 5 + band)));
    assert!(skies.iter().any(|sky| sky.model.to_ascii_lowercase().ends_with(".mdx")));
    // At noon, as the probes of 9.7 read them: the band between its keys around 1,440 half-minutes.
    let noon = |keys: &[(u32, f32)]| {
        let after = keys.iter().position(|(time, _)| *time > 1440).unwrap_or(keys.len());
        let (t1, v1) = keys[after.max(1) - 1];
        let (t2, v2) = keys.get(after).copied().unwrap_or((keys[0].0 + 2880, keys[0].1));
        v1 + (v2 - v1) * (1440.0 - t1 as f32) / (t2 as f32 - t1 as f32).max(1.0)
    };
    let colour = |params: u32, band: u32| -> [f32; 3] {
        let found = colours
            .iter()
            .find(|found| found.id == params * 18 - 17 + band)
            .unwrap();
        std::array::from_fn(|channel| {
            let keys: Vec<(u32, f32)> = found
                .keys
                .iter()
                .map(|(time, rgb)| (*time, f32::from(rgb[channel])))
                .collect();
            noon(&keys).round()
        })
    };
    let number = |params: u32, band: u32| {
        noon(
            &numbers
                .iter()
                .find(|found| found.id == params * 6 - 5 + band)
                .unwrap()
                .keys,
        )
    };
    assert_eq!(colour(clear, 0), [255.0, 136.0, 0.0], "the diffuse light, red first");
    assert_eq!(colour(clear, 1), [104.0, 130.0, 154.0], "the ambient light");
    assert_eq!(colour(clear, 2), [0.0, 31.0, 73.0], "the top of the sky");
    assert_eq!(
        (number(clear, 0) / 36.0, number(clear, 1)),
        (500.0, 0.25),
        "the fog, in yards"
    );
    let northrend = lights
        .iter()
        .find(|light| light.map == 571 && light.position == [0.0; 3])
        .unwrap();
    assert!((number(northrend.params[0], 0) / 36.0 - 889.0).abs() < 1.0);
    // The local lights of the Eastern Kingdoms over its tiles, their centres read as the module
    // `lighting` reads them: 32 tiles − z / 36 to the north, 32 tiles − x / 36 to the west.
    let azeroth = chain.read("World\\Maps\\Azeroth\\Azeroth.wdt").unwrap().unwrap();
    let azeroth = crate::terrain::wdt(&azeroth).unwrap();
    let middle = 32.0 * uniwow_api::formats::TILE;
    let locals: Vec<_> = lights
        .iter()
        .filter(|light| light.map == 0 && light.position != [0.0; 3])
        .collect();
    let over = locals
        .iter()
        .filter(|light| {
            let centre = [middle - light.position[2] / 36.0, middle - light.position[0] / 36.0];
            // The tile of the file `<x>_<y>`: x from the west coordinate, y from the north one.
            let [y, x] = centre.map(|axis| (32.0 - axis / uniwow_api::formats::TILE).floor() as i32);
            (0..64).contains(&x) && (0..64).contains(&y) && azeroth.tiles[(y * 64 + x) as usize]
        })
        .count();
    assert!(over * 10 >= locals.len() * 9, "{over} of {}", locals.len());
    // The looks of characters the displays name, but a few; hairs and facial hairs for the 20 bodies.
    let named: Vec<_> = displays.iter().filter(|display| display.extra != 0).collect();
    let missing = named
        .iter()
        .filter(|display| looks.binary_search_by_key(&display.extra, |look| look.id).is_err())
        .count();
    assert!(
        missing * 100 <= named.len(),
        "{missing} of {} looks missing",
        named.len()
    );
    for race in [1, 2, 3, 4, 5, 6, 7, 8, 10, 11] {
        for sex in [0, 1] {
            assert!(hairs.iter().any(|hair| (hair.race, hair.sex) == (race, sex)));
            assert!(facials.iter().any(|facial| (facial.race, facial.sex) == (race, sex)));
            // The texture of a hair, its section 3.
            assert!(sections.iter().any(|section| {
                (section.race, section.sex, section.section) == (race, sex, 3) && !section.textures[0].is_empty()
            }));
        }
    }
    let kinds: Vec<&str> = objects
        .iter()
        .map(|object| object.path.as_str())
        .filter(|path| {
            let path = path.to_ascii_lowercase();
            !path.is_empty() && ![".mdx", ".mdl", ".m2", ".wmo"].iter().any(|kind| path.ends_with(kind))
        })
        .collect();
    assert!(kinds.is_empty(), "{kinds:?}");
    let map = |id: u32| maps.iter().find(|map| map.id == id).unwrap();
    for (id, directory) in [
        (0, "Azeroth"),
        (1, "Kalimdor"),
        (530, "Expansion01"),
        (571, "Northrend"),
    ] {
        assert_eq!(map(id).directory, directory);
        assert!(!map(id).name.is_empty(), "the map {id} named in {locale}");
    }
    let dun_morogh = areas.iter().find(|area| area.id == 1).unwrap();
    assert_eq!((dun_morogh.map, dun_morogh.parent), (0, 0));
    assert!(!dun_morogh.name.is_empty());
    // The table names a model `.mdx`, a few `.m2`; the client reads the `.m2`.
    let others: Vec<&str> = models
        .iter()
        .map(|model| model.path.as_str())
        .filter(|path| {
            let path = path.to_ascii_lowercase();
            !path.ends_with(".mdx") && !path.ends_with(".m2")
        })
        .collect();
    assert!(others.is_empty(), "{others:?}");
    let found = models
        .iter()
        .filter(|model| {
            let path = model.path.to_ascii_lowercase();
            let stem = path.rsplit_once('.').map_or(path.as_str(), |(stem, _)| stem);
            chain.exists(&format!("{stem}.m2"))
        })
        .count();
    assert!(
        found * 10 >= models.len() * 9,
        "{found} of {} models found",
        models.len()
    );
    let known = displays
        .iter()
        .filter(|display| models.binary_search_by_key(&display.model, |model| model.id).is_ok())
        .count();
    assert!(
        known * 10 >= displays.len() * 9,
        "{known} of {} looks with their model",
        displays.len()
    );
    let (ids, refused) = FileIds::load(&folder, &chain);
    eprintln!(
        "{locale}: {} maps, {} areas, {} looks, {} models, {} looks of characters, {} hairs, {} facial hairs, \
         {} looks of objects, {} sections of characters read in {read:?}; {found} models found; {missing} of {} looks of characters named \
         missing; {} FileDataIDs named, refused {refused:?}; the map 0 is named {:?}",
        maps.len(),
        areas.len(),
        displays.len(),
        models.len(),
        looks.len(),
        hairs.len(),
        facials.len(),
        objects.len(),
        sections.len(),
        named.len(),
        ids.len(),
        map(0).name
    );
}
