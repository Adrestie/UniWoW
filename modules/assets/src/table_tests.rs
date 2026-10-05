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
use crate::db2::{BITPACKED, COMMON, FileIds, NONE, PALLET, PathTable, SIGNED, SPARSE};
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

/// A WDC1 as DB2Gen writes it, an id then the offset of a path from the start of the strings, with
/// what the format may hold besides: a list of ids, records of variable size, copies.
fn wdc1(rows: &[(u32, &str)], ids: Ids, sparse: bool, copies: &[(u32, u32)]) -> Vec<u8> {
    assert!(matches!(ids, Ids::Plain | Ids::List));
    let listed = ids == Ids::List;
    let columns = if listed {
        vec![storage(0, 32, NONE)]
    } else {
        vec![storage(0, 32, NONE), storage(32, 32, NONE)]
    };
    let record_size = columns.len() as u32 * 4;
    let min_id = rows.iter().map(|row| row.0).min().unwrap_or(0);
    let max_id = rows.iter().map(|row| row.0).max().unwrap_or(0);
    let mut out = vec![0u8; 84];
    put_fields(&mut out, &columns);
    let mut map_offset = 0;
    let mut strings_size = 0;
    if sparse {
        let mut offsets = vec![(0u32, 0u16); (max_id - min_id + 1) as usize];
        for (id, path) in rows {
            let start = out.len();
            if !listed {
                put(&mut out, &[*id]);
            }
            out.extend(path.as_bytes());
            out.push(0);
            offsets[(id - min_id) as usize] = (start as u32, (out.len() - start) as u16);
        }
        map_offset = out.len() as u32;
        for (offset, size) in offsets {
            put(&mut out, &[offset]);
            out.extend(size.to_le_bytes());
        }
    } else {
        let mut strings = Strings::new();
        for (id, path) in rows {
            if !listed {
                put(&mut out, &[*id]);
            }
            let offset = strings.add(path);
            put(&mut out, &[offset]);
        }
        strings_size = strings.bytes.len() as u32;
        out.extend(&strings.bytes);
    }
    if listed {
        put(&mut out, &rows.iter().map(|row| row.0).collect::<Vec<u32>>());
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
            rows.len() as u32,
            count,
            record_size,
            strings_size,
            0,
            0,
            min_id,
            max_id,
            0,
            copies.len() as u32 * 8,
        ],
    );
    header.extend((if sparse { SPARSE } else { 0 }).to_le_bytes());
    header.extend(0u16.to_le_bytes());
    let ids_size = if listed { rows.len() as u32 * 4 } else { 0 };
    put(
        &mut header,
        &[count, record_size, 0, map_offset, ids_size, count * 24, 0, 0, 0],
    );
    out[..84].copy_from_slice(&header);
    out
}

/// A DB2 of WDC2 or after: its sections, how it keeps its ids, whether its records vary in size,
/// a section encrypted with a key the client lacks, the copies of its last section.
struct Wdc<'a> {
    magic: &'a [u8; 4],
    sections: Vec<Vec<(u32, &'a str)>>,
    ids: Ids,
    sparse: bool,
    encrypted: Option<usize>,
    copies: Vec<(u32, u32)>,
}

fn wdc<'a>(magic: &'a [u8; 4], sections: &[&[(u32, &'a str)]]) -> Wdc<'a> {
    Wdc {
        magic,
        sections: sections.iter().map(|rows| rows.to_vec()).collect(),
        ids: Ids::Plain,
        sparse: false,
        encrypted: None,
        copies: Vec::new(),
    }
}

impl Wdc<'_> {
    fn write(&self) -> Vec<u8> {
        let version2 = matches!(self.magic, b"WDC2" | b"1SLC");
        let listed = matches!(self.ids, Ids::List | Ids::Zeros);
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
            _ if listed => (0, 0),
            _ => (1, 0),
        };
        let record_size = columns.len() * 4;
        let min_id = all.iter().map(|row| row.0).min().unwrap_or(0);
        let max_id = all.iter().map(|row| row.0).max().unwrap_or(0);
        let strings: Vec<Strings> = self
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
        if self.magic == b"WDC5" {
            put(&mut out, &[5]);
            out.extend([b'x'; 128]);
        }
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
        if self.magic == b"WDC5" {
            // What WDC5 puts before its sections, which their offsets step over.
            for _ in 1..self.sections.len() {
                put(&mut out, &[1, 0xDEAD_BEEF]);
            }
        }
        let mut headers = Vec::new();
        let mut outside = 0;
        let mut base = 0;
        for (place, rows) in self.sections.iter().enumerate() {
            let file_offset = out.len();
            let mut map = Vec::new();
            let encrypted = self.encrypted == Some(place);
            if self.sparse {
                for (id, path) in rows {
                    let start = out.len();
                    if !listed {
                        put(&mut out, &[*id]);
                    }
                    out.extend(path.as_bytes());
                    out.push(0);
                    if encrypted {
                        out[start..].fill(0);
                        map.push((0, 0));
                    } else {
                        map.push((start as u32, (out.len() - start) as u16));
                    }
                }
            } else {
                for (index, (id, path)) in rows.iter().enumerate() {
                    let mut record = vec![0u8; record_size];
                    let local = strings[place].at.get(*path).copied().unwrap_or(0) as usize;
                    let offset = if local == 0 {
                        0
                    } else {
                        base + local + all.len() * record_size - ((outside + index) * record_size + path_byte)
                    };
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
            }
            let records_end = if self.sparse {
                out.len()
            } else {
                file_offset + rows.len() * record_size
            };
            let mut map_offset = 0;
            if version2 && self.sparse {
                map_offset = out.len();
                let mut by_id = vec![(0u32, 0u16); (max_id - min_id + 1) as usize];
                for ((id, _), entry) in rows.iter().zip(&map) {
                    by_id[(id - min_id) as usize] = *entry;
                }
                for (offset, size) in by_id {
                    put(&mut out, &[offset]);
                    out.extend(size.to_le_bytes());
                }
            }
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
            let mut relations = 0;
            if !version2 && self.sparse {
                for (offset, size) in &map {
                    put(&mut out, &[*offset]);
                    out.extend(size.to_le_bytes());
                }
                // Relations of none, which the ids of the map follow.
                put(&mut out, &[0, 0, 0]);
                relations = 12;
                put(&mut out, &rows.iter().map(|row| row.0).collect::<Vec<u32>>());
            }
            let strings_size = if self.sparse { 0 } else { strings[place].bytes.len() };
            let key: u64 = if encrypted { 0x1234_5678_9ABC_DEF0 } else { 0 };
            headers.extend(key.to_le_bytes());
            put(
                &mut headers,
                &[file_offset as u32, rows.len() as u32, strings_size as u32],
            );
            if version2 {
                put(
                    &mut headers,
                    &[copies.len() as u32 * 8, map_offset as u32, ids.len() as u32 * 4, 0],
                );
            } else {
                let map_count = if self.sparse { map.len() as u32 } else { 0 };
                put(
                    &mut headers,
                    &[
                        records_end as u32,
                        ids.len() as u32 * 4,
                        relations,
                        map_count,
                        copies.len() as u32,
                    ],
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
        header.extend((if self.sparse { SPARSE } else { 0 }).to_le_bytes());
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

/// Whether `table` names each of `rows` by its id, and nothing else, an empty path naming nothing.
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

const ROWS: [(u32, &str); 5] = [
    (10, "world\\a.blp"),
    (3, "world\\b.blp"),
    (7, ""),
    (12, "world\\a.blp"),
    (5_000_000, "character\\human\\male\\humanmale.blp"),
];

#[test]
fn a_wdc1_as_db2gen_writes_it_names_each_file_by_its_id() {
    let table = parse(wdc1(&ROWS, Ids::Plain, false, &[]));
    names(&table, &ROWS);
    assert_eq!(table.path(11), None);
    assert_eq!(table.path(0), None);
    let twice = parse(wdc1(&[(3, "first.blp"), (3, "second.blp")], Ids::Plain, false, &[]));
    assert_eq!(
        twice.path(3).as_deref(),
        Some("first.blp"),
        "an id named twice keeps its first row"
    );
    assert_eq!(twice.len(), 1);
}

#[test]
fn a_wdc1_with_a_list_of_ids_records_of_variable_size_or_copies_reads_the_same() {
    names(&parse(wdc1(&ROWS[..4], Ids::List, false, &[])), &ROWS[..4]);
    names(&parse(wdc1(&ROWS[..4], Ids::Plain, true, &[])), &ROWS[..4]);
    names(&parse(wdc1(&ROWS[..4], Ids::List, true, &[])), &ROWS[..4]);
    let copied = parse(wdc1(&ROWS, Ids::Plain, false, &[(20, 3), (21, 7), (22, 999)]));
    assert_eq!(copied.path(20).as_deref(), Some("world\\b.blp"));
    assert_eq!(copied.path(21), None, "a copy of an empty path names nothing");
    assert_eq!(copied.path(22), None, "a copy of a row missing names nothing");
    assert_eq!(copied.len(), 5);
}

#[test]
fn every_version_from_wdc2_reads_its_sections_and_their_strings() {
    let first: &[(u32, &str)] = &ROWS[..3];
    let second: &[(u32, &str)] = &[(40, "world\\c.m2"), (41, "world\\c.m2"), (42, "")];
    let all = [first, second].concat();
    for magic in [b"WDC2", b"1SLC", b"WDC3", b"WDC5"] {
        let table = PathTable::parse(wdc(magic, &[first, second]).write())
            .unwrap_or_else(|e| panic!("{}: {e}", String::from_utf8_lossy(magic)));
        names(&table, &all);
    }
}

#[test]
fn ids_packed_signed_in_a_pallet_or_listed_read_as_wow_export_reads_them() {
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
    names(&parse(zeros.write()), &[(0, "a.blp"), (1, "b.blp"), (2, "c.blp")]);
    let common = Wdc {
        ids: Ids::Common,
        ..wdc(b"WDC3", &[rows])
    };
    let refused = PathTable::parse(common.write()).err().unwrap();
    assert!(refused.contains("common data"), "{refused}");
}

#[test]
fn records_of_variable_size_and_copies_read_in_every_version() {
    let first: &[(u32, &str)] = &[(4, "world\\a.wmo"), (9, ""), (5, "world\\c.wmo")];
    let second: &[(u32, &str)] = &[(6, "world\\b.wmo")];
    let both = [first, second].concat();
    let copies = vec![(30, 4), (31, 6)];
    let expected = [
        (4, "world\\a.wmo"),
        (5, "world\\c.wmo"),
        (6, "world\\b.wmo"),
        (30, "world\\a.wmo"),
        (31, "world\\b.wmo"),
    ];
    // WDC2 maps its records by id over the whole table: one section.
    let single = Wdc {
        sparse: true,
        copies: copies.clone(),
        ..wdc(b"WDC2", &[both.as_slice()])
    };
    names(&parse(single.write()), &expected);
    for magic in [b"WDC3", b"WDC5"] {
        for ids in [Ids::Plain, Ids::List] {
            let table = Wdc {
                sparse: true,
                ids,
                copies: copies.clone(),
                ..wdc(magic, &[first, second])
            };
            names(&parse(table.write()), &expected);
        }
    }
}

#[test]
fn a_section_encrypted_with_a_key_the_client_lacks_names_nothing() {
    let first: &[(u32, &str)] = &[(1, "a.blp"), (2, "b.blp")];
    let second: &[(u32, &str)] = &[(3, "c.blp")];
    for sparse in [false, true] {
        for ids in [Ids::Plain, Ids::List] {
            let table = Wdc {
                encrypted: Some(0),
                sparse,
                ids,
                ..wdc(b"WDC3", &[first, second])
            };
            names(&parse(table.write()), second);
        }
    }
}

#[test]
fn what_warcraftxl_does_not_read_is_refused_and_a_file_damaged_never_panics() {
    let mut wdc4 = wdc(b"WDC3", &[&ROWS[..]]).write();
    wdc4[..4].copy_from_slice(b"WDC4");
    let refused = PathTable::parse(wdc4).err().unwrap();
    assert!(refused.contains("WDC4"), "{refused}");
    assert!(PathTable::parse(b"WDBC\0\0\0\0".to_vec()).is_err());
    let mut pallet = Wdc {
        ids: Ids::Pallet,
        ..wdc(b"WDC3", &[&ROWS[..]])
    }
    .write();
    // The size of the pallets in the header, 4 bytes more than they hold.
    let size = u32::from_le_bytes(pallet[64..68].try_into().unwrap());
    pallet[64..68].copy_from_slice(&(size + 4).to_le_bytes());
    let refused = PathTable::parse(pallet).err().unwrap();
    assert!(refused.contains("pallets"), "{refused}");
    let samples = [
        wdc1(&ROWS, Ids::Plain, false, &[(20, 3)]),
        wdc1(&ROWS[..4], Ids::Plain, true, &[]),
        wdc(b"WDC2", &[&ROWS[..]]).write(),
        Wdc {
            ids: Ids::Pallet,
            ..wdc(b"WDC3", &[&ROWS[..3], &ROWS[3..]])
        }
        .write(),
        Wdc {
            sparse: true,
            ..wdc(b"WDC5", &[&ROWS[..2], &ROWS[2..]])
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
    let textures = wdc1(&[(1, "a.blp"), (5, "e.blp")], Ids::Plain, false, &[]);
    let model_rows: &[(u32, &str)] = &[(1, "m.m2"), (2, "n.m2")];
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
    assert_eq!(ids.path_of(3), None);
    assert_eq!(ids.len(), 4);

    std::fs::remove_file(&loose).unwrap();
    let (ids, refused) = FileIds::load(&folder, &chain);
    assert_eq!(refused.len(), 1, "{refused:?}");
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
                    (6, Text("WolfSkinGrey")),
                    (8, Text("WolfSkinBlack")),
                ]],
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
    tables.push(("ModelFilePath.db2", wdc1(&[(8, "world\\x.m2")], Ids::Plain, false, &[])));
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
fn a_table_of_a_million_paths_reads_in_a_moment() {
    let names: Vec<String> = (0..1_000_000)
        .map(|id| format!("world\\textures\\{id:07}.blp"))
        .collect();
    let rows: Vec<(u32, &str)> = names
        .iter()
        .enumerate()
        .map(|(id, name)| (id as u32 * 3, name.as_str()))
        .collect();
    let bytes = wdc1(&rows, Ids::Plain, false, &[]);
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
    let read = start.elapsed();
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
        "{locale}: {} maps, {} areas, {} looks, {} models read in {read:?}; {found} models found; \
         {} FileDataIDs named, refused {refused:?}; the map 0 is named {:?}",
        maps.len(),
        areas.len(),
        displays.len(),
        models.len(),
        ids.len(),
        map(0).name
    );
}
