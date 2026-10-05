//! The tables of paths of WarcraftXL, `TextureFilePath.db2` and `ModelFilePath.db2`: the FileDataID
//! of a file and its path, read as wxl-db2 reads them (its sources read, nothing copied: GPL-3).
//! WDC1, the version DB2Gen writes, is read from the public description of the format; WDC2, `1SLC`
//! and WDC3 are translated from the reader of wow.export (MIT, see THIRD_PARTY.md). As wxl-db2
//! does: the offset of a path counts from the start of the strings of all the sections, end to end,
//! in every version; a list of ids gives each record its id as it stands; an id named twice keeps
//! its last row, copies included; WDC4 and WDC5, the records of variable size, whose paths it does
//! not read, and a column of relations are refused.

use std::path::Path;

use crate::chain::Chain;

const WDC1: u32 = u32::from_le_bytes(*b"WDC1");
const WDC2: u32 = u32::from_le_bytes(*b"WDC2");
const CLS1: u32 = u32::from_le_bytes(*b"1SLC");
const WDC3: u32 = u32::from_le_bytes(*b"WDC3");
const WDC4: u32 = u32::from_le_bytes(*b"WDC4");
const WDC5: u32 = u32::from_le_bytes(*b"WDC5");

/// How a column is stored.
pub(crate) const NONE: u32 = 0;
pub(crate) const BITPACKED: u32 = 1;
pub(crate) const COMMON: u32 = 2;
pub(crate) const PALLET: u32 = 3;
pub(crate) const PALLET_ARRAY: u32 = 4;
pub(crate) const SIGNED: u32 = 5;

/// The flag of a table whose records vary in size, found through a map of offsets.
const SPARSE: u16 = 1;

const SPARSE_REFUSED: &str = "records of variable size, whose paths WarcraftXL does not read";
const RELATIONS_REFUSED: &str = "a column of relations, which WarcraftXL does not take in a table of paths";

/// The tables of paths, in the order WarcraftXL looks a FileDataID up.
pub const TABLES: [&str; 2] = ["TextureFilePath.db2", "ModelFilePath.db2"];

/// The place of a row whose path is empty or out of the strings, which WarcraftXL reads as empty.
const NO_PATH: u32 = u32::MAX;

/// Reads little-endian integers, refusing to go past the end of the bytes.
struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8], at: usize) -> Self {
        Self { bytes, at }
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], String> {
        let taken = self
            .at
            .checked_add(len)
            .and_then(|end| self.bytes.get(self.at..end))
            .ok_or_else(|| format!("cut short at byte {}", self.at))?;
        self.at += len;
        Ok(taken)
    }

    fn skip(&mut self, len: usize) -> Result<(), String> {
        self.take(len).map(|_| ())
    }

    fn u16(&mut self) -> Result<u16, String> {
        self.take(2).map(|b| u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, String> {
        self.take(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn size(&mut self) -> Result<usize, String> {
        self.u32().map(|value| value as usize)
    }

    fn u32s(&mut self, count: usize) -> Result<Vec<u32>, String> {
        let bytes = self.take(count.checked_mul(4).ok_or("too many values")?)?;
        Ok(bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| u32::from_le_bytes(*b))
            .collect())
    }
}

/// How a column of the records is stored.
#[derive(Clone, Copy)]
struct Column {
    offset_bits: usize,
    size_bits: usize,
    additional: usize,
    compression: u32,
    packing: [u32; 3],
}

/// A section of a table, the whole table in WDC1.
#[derive(Default)]
struct Section {
    records: usize,
    count: usize,
    strings: usize,
    strings_size: usize,
    ids: Vec<u32>,
    /// The rows copied: the id of the copy, then that of the row copied.
    copies: Vec<(u32, u32)>,
}

/// A table, whatever its version.
struct Layout<'a> {
    bytes: &'a [u8],
    record_size: usize,
    id_index: usize,
    columns: Vec<Column>,
    /// The values of each column stored in a pallet: where they start, and how many.
    pallets: Vec<(usize, usize)>,
    sections: Vec<Section>,
}

fn columns(at: &mut Cursor, size: usize) -> Result<Vec<Column>, String> {
    (0..size / 24)
        .map(|_| {
            Ok(Column {
                offset_bits: usize::from(at.u16()?),
                size_bits: usize::from(at.u16()?),
                additional: at.size()?,
                compression: at.u32()?,
                packing: [at.u32()?, at.u32()?, at.u32()?],
            })
        })
        .collect()
}

/// The pallets of the columns, then their common data, which only the pallets' sizes need.
fn pallets(
    at: &mut Cursor,
    columns: &[Column],
    pallet_size: usize,
    common_size: usize,
) -> Result<Vec<(usize, usize)>, String> {
    let start = at.at;
    let pallets = columns
        .iter()
        .map(|column| {
            if matches!(column.compression, PALLET | PALLET_ARRAY) {
                let place = (at.at, column.additional / 4);
                at.skip(column.additional)?;
                Ok(place)
            } else {
                Ok((0, 0))
            }
        })
        .collect::<Result<Vec<_>, String>>()?;
    let common: usize = columns
        .iter()
        .filter(|column| column.compression == COMMON)
        .map(|column| column.additional)
        .sum();
    if at.at - start != pallet_size || common != common_size {
        return Err("its pallets or its common data do not match its header".to_owned());
    }
    at.skip(common_size)?;
    Ok(pallets)
}

fn pairs(values: &[u32]) -> Vec<(u32, u32)> {
    values
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| (pair[0], pair[1]))
        .collect()
}

/// Sorts `rows` by id, keeping the last row of each id.
fn keep_last(rows: &mut Vec<(u32, u32)>) {
    rows.reverse();
    rows.sort_by_key(|row| row.0);
    rows.dedup_by_key(|row| row.0);
}

impl<'a> Layout<'a> {
    fn parse(bytes: &'a [u8]) -> Result<Self, String> {
        if bytes.len() > u32::MAX as usize {
            return Err("larger than 4 GB".to_owned());
        }
        match Cursor::new(bytes, 0).u32()? {
            WDC1 => Self::wdc1(bytes),
            WDC2 | CLS1 => Self::wdc(bytes, 2),
            WDC3 => Self::wdc(bytes, 3),
            WDC4 => Err("WDC4, which WarcraftXL does not read for its tables of paths".to_owned()),
            WDC5 => Err("WDC5, which WarcraftXL does not read for its tables of paths".to_owned()),
            _ => Err("not a DB2 of a version WarcraftXL reads for its tables of paths (WDC1 to WDC3)".to_owned()),
        }
    }

    /// WDC1: one section, its columns after the records.
    fn wdc1(bytes: &'a [u8]) -> Result<Self, String> {
        let mut at = Cursor::new(bytes, 4);
        let record_count = at.size()?;
        at.skip(4)?; // the count of the columns, which the storage of the columns gives
        let record_size = at.size()?;
        let strings_size = at.size()?;
        at.skip(16)?; // the hashes of the table and of its layout, the lowest and highest ids
        at.skip(4)?; // the locale
        let copy_size = at.size()?;
        if at.u16()? & SPARSE != 0 {
            return Err(SPARSE_REFUSED.to_owned());
        }
        let id_index = usize::from(at.u16()?);
        let total_fields = at.size()?;
        at.skip(12)?; // where the bits packed start, the count of the lookup columns, the map of offsets
        let ids_size = at.size()?;
        let info_size = at.size()?;
        let common_size = at.size()?;
        let pallet_size = at.size()?;
        if at.u32()? != 0 {
            return Err(RELATIONS_REFUSED.to_owned());
        }
        at.skip(total_fields.checked_mul(4).ok_or("too many columns")?)?;
        let mut section = Section {
            records: at.at,
            count: record_count,
            strings_size,
            ..Section::default()
        };
        at.skip(record_count.checked_mul(record_size).ok_or("too many records")?)?;
        section.strings = at.at;
        at.skip(strings_size)?;
        section.ids = at.u32s(ids_size / 4)?;
        section.copies = pairs(&at.u32s(copy_size / 8 * 2)?);
        let columns = columns(&mut at, info_size)?;
        let pallets = pallets(&mut at, &columns, pallet_size, common_size)?;
        Ok(Self {
            bytes,
            record_size,
            id_index,
            columns,
            pallets,
            sections: vec![section],
        })
    }

    /// WDC2 and WDC3: the sections, each found by the offset its header gives.
    fn wdc(bytes: &'a [u8], version: u32) -> Result<Self, String> {
        let mut at = Cursor::new(bytes, 4);
        at.skip(4)?; // the count of the records, which each section gives
        at.skip(4)?; // the count of the columns, which the storage of the columns gives
        let record_size = at.size()?;
        at.skip(4)?; // the size of the strings, which each section gives
        at.skip(16)?; // the hashes of the table and of its layout, the lowest and highest ids
        at.skip(4)?; // the locale
        if at.u16()? & SPARSE != 0 {
            return Err(SPARSE_REFUSED.to_owned());
        }
        let id_index = usize::from(at.u16()?);
        let total_fields = at.size()?;
        at.skip(8)?; // where the bits packed start in a record, and the count of the lookup columns
        let info_size = at.size()?;
        let common_size = at.size()?;
        let pallet_size = at.size()?;
        let section_count = at.size()?;
        let header_size = if version == 2 { 36 } else { 40 };
        let mut headers = Cursor::new(
            at.take(section_count.checked_mul(header_size).ok_or("too many sections")?)?,
            0,
        );
        at.skip(total_fields.checked_mul(4).ok_or("too many columns")?)?;
        let columns = columns(&mut at, info_size)?;
        let pallets = pallets(&mut at, &columns, pallet_size, common_size)?;
        let mut sections = Vec::with_capacity(section_count);
        for _ in 0..section_count {
            headers.skip(8)?; // the hash of the key that encrypts it
            let file_offset = headers.size()?;
            let count = headers.size()?;
            let strings_size = headers.size()?;
            let (copy_count, ids_size, relations_size, map_count) = if version == 2 {
                let copy_size = headers.size()?;
                headers.skip(4)?; // the map of offsets of records of variable size
                let ids_size = headers.size()?;
                (copy_size / 8, ids_size, headers.size()?, 0)
            } else {
                headers.skip(4)?; // the end of records of variable size
                let ids_size = headers.size()?;
                let relations_size = headers.size()?;
                let map_count = headers.size()?;
                (headers.size()?, ids_size, relations_size, map_count)
            };
            if relations_size != 0 {
                return Err(RELATIONS_REFUSED.to_owned());
            }
            let mut at = Cursor::new(bytes, file_offset);
            let mut section = Section {
                records: file_offset,
                count,
                strings_size,
                ..Section::default()
            };
            at.skip(count.checked_mul(record_size).ok_or("too many records")?)?;
            section.strings = at.at;
            at.skip(strings_size)?;
            section.ids = at.u32s(ids_size / 4)?;
            section.copies = pairs(&at.u32s(copy_count.checked_mul(2).ok_or("too many copies")?)?);
            // A map of offsets and its ids, which only records of variable size use.
            at.skip(map_count.checked_mul(10).ok_or("too many offsets")?)?;
            sections.push(section);
        }
        Ok(Self {
            bytes,
            record_size,
            id_index,
            columns,
            pallets,
            sections,
        })
    }

    /// The integer of `column` in the record at `start`.
    fn int(&self, column: usize, start: usize) -> Result<u32, String> {
        let stored = self.columns[column];
        let byte = start + stored.offset_bits / 8;
        match stored.compression {
            NONE => self.plain(stored, byte),
            BITPACKED | SIGNED | PALLET | PALLET_ARRAY => {
                if !(1..=32).contains(&stored.size_bits) {
                    return Err(format!("a column of {} bits packed", stored.size_bits));
                }
                let mut raw = [0u8; 8];
                let rest = self.bytes.get(byte..).unwrap_or_default();
                let len = rest.len().min(8);
                raw[..len].copy_from_slice(&rest[..len]);
                let value = (u64::from_le_bytes(raw) >> (stored.offset_bits & 7)) & ((1 << stored.size_bits) - 1);
                match stored.compression {
                    BITPACKED => Ok(value as u32),
                    SIGNED => {
                        let shift = 32 - stored.size_bits;
                        Ok((((value as u32) << shift) as i32 >> shift) as u32)
                    }
                    _ => {
                        if stored.compression == PALLET_ARRAY && stored.packing[2] != 1 {
                            return Err("an id stored as an array".to_owned());
                        }
                        let (pallet, count) = self.pallets[column];
                        if value as usize >= count {
                            return Err(format!("a value {value} out of its pallet of {count}"));
                        }
                        Cursor::new(self.bytes, pallet + value as usize * 4).u32()
                    }
                }
            }
            COMMON => Err("an id stored as common data, which gives a value by id".to_owned()),
            other => Err(format!("a column stored in a way unknown ({other})")),
        }
    }

    /// The integer of a column stored plainly, of 1 to 4 bytes, at `byte`.
    fn plain(&self, stored: Column, byte: usize) -> Result<u32, String> {
        let width = stored.size_bits / 8;
        if !stored.size_bits.is_multiple_of(8) || !(1..=4).contains(&width) {
            return Err(format!("a column of {} bits", stored.size_bits));
        }
        let bytes = Cursor::new(self.bytes, byte).take(width)?;
        Ok(bytes.iter().rev().fold(0, |value, byte| value << 8 | u32::from(*byte)))
    }

    /// Where the string at `offset` starts, among the strings of the sections end to end, each
    /// section's starting at its place in `bases`; `NO_PATH` for an empty one or one out of them.
    fn string(&self, offset: usize, bases: &[usize]) -> u32 {
        self.sections
            .iter()
            .zip(bases)
            .find(|(section, base)| (**base..**base + section.strings_size).contains(&offset))
            .map(|(section, base)| section.strings + offset - base)
            .filter(|start| self.bytes[*start] != 0)
            .map_or(NO_PATH, |start| start as u32)
    }

    /// The rows of a table of paths: each id, and where its path starts. The table holds its paths
    /// and, with no list of ids, its ids; a record takes the id of its place in the list of ids of
    /// its section, else that of the column of ids, else its place.
    fn paths(&self) -> Result<Vec<(u32, u32)>, String> {
        let listed = self.sections.iter().any(|section| !section.ids.is_empty());
        let path = match (listed, self.columns.len(), self.id_index) {
            (true, 1, _) => 0,
            (false, 2, id @ 0..=1) => 1 - id,
            (_, count, _) => {
                return Err(format!(
                    "{count} columns {} a list of ids, where WarcraftXL takes a path and an id",
                    if listed { "and" } else { "without" }
                ));
            }
        };
        let stored = self.columns[path];
        if stored.compression != NONE || stored.size_bits != 32 {
            return Err("its paths are not offsets of 32 bits".to_owned());
        }
        let mut bases = Vec::with_capacity(self.sections.len());
        let mut base = 0;
        for section in &self.sections {
            bases.push(base);
            base += section.strings_size;
        }
        let mut rows = Vec::new();
        for section in &self.sections {
            for index in 0..section.count {
                let start = section.records + index * self.record_size;
                let id = match section.ids.get(index) {
                    Some(id) => *id,
                    None if self.id_index < self.columns.len() => self.int(self.id_index, start)?,
                    None => index as u32,
                };
                let offset = Cursor::new(self.bytes, start + stored.offset_bits / 8).size()?;
                rows.push((id, self.string(offset, &bases)));
            }
        }
        keep_last(&mut rows);
        let copied: Vec<(u32, u32)> = self
            .sections
            .iter()
            .flat_map(|section| &section.copies)
            .filter_map(|(copy, copied)| {
                let found = rows.binary_search_by_key(copied, |row| row.0).ok()?;
                Some((*copy, rows[found].1))
            })
            .collect();
        if !copied.is_empty() {
            rows.extend(copied);
            keep_last(&mut rows);
        }
        Ok(rows)
    }
}

/// A table of paths: its bytes, and for each FileDataID, by increasing id, where its path starts.
pub struct PathTable {
    bytes: Vec<u8>,
    rows: Vec<(u32, u32)>,
    /// The rows naming a path.
    named: usize,
}

impl PathTable {
    pub fn parse(bytes: Vec<u8>) -> Result<Self, String> {
        let rows = Layout::parse(&bytes)?.paths()?;
        let named = rows.iter().filter(|row| row.1 != NO_PATH).count();
        Ok(Self { bytes, rows, named })
    }

    /// The path of the file `id`, none when the table does not name it or names it empty.
    pub fn path(&self, id: u32) -> Option<String> {
        let found = self.rows.binary_search_by_key(&id, |row| row.0).ok()?;
        let start = self.rows[found].1;
        if start == NO_PATH {
            return None;
        }
        let rest = &self.bytes[start as usize..];
        let end = rest.iter().position(|byte| *byte == 0).unwrap_or(rest.len());
        Some(String::from_utf8_lossy(&rest[..end]).into_owned())
    }

    pub fn len(&self) -> usize {
        self.named
    }
}

/// The FileDataIDs of the client, through its tables of paths, in the order of `TABLES`.
#[derive(Default)]
pub struct FileIds {
    tables: Vec<PathTable>,
}

impl FileIds {
    /// Reads the tables of paths of the client in `client`, both at once, each loose in its
    /// `DBFilesClient` first, then in `chain`, an empty file counting as none: a table missing is
    /// left out, as one that cannot be read, which is returned with why.
    pub fn load(client: &Path, chain: &Chain) -> (Self, Vec<String>) {
        let read = |name: &str| -> Result<Option<PathTable>, String> {
            let loose = client.join("DBFilesClient").join(name);
            let mut bytes = if loose.is_file() {
                std::fs::read(&loose).map_err(|e| format!("{}: {e}", loose.display()))?
            } else {
                Vec::new()
            };
            if bytes.is_empty() {
                bytes = chain.read(&format!("DBFilesClient\\{name}"))?.unwrap_or_default();
            }
            if bytes.is_empty() {
                return Ok(None);
            }
            PathTable::parse(bytes).map(Some).map_err(|e| format!("{name}: {e}"))
        };
        let read = &read;
        let found: Vec<Result<Option<PathTable>, String>> = std::thread::scope(|scope| {
            TABLES
                .map(|name| scope.spawn(move || read(name)))
                .into_iter()
                .zip(TABLES)
                .map(|(thread, name)| {
                    thread
                        .join()
                        .unwrap_or_else(|_| Err(format!("{name}: its reading failed")))
                })
                .collect()
        });
        let mut ids = Self::default();
        let mut refused = Vec::new();
        for table in found {
            match table {
                Ok(Some(table)) => ids.tables.push(table),
                Ok(None) => {}
                Err(reason) => refused.push(reason),
            }
        }
        (ids, refused)
    }

    /// The path of the file `id`: that of the first table naming it with a path; none for 0, as
    /// WarcraftXL resolves it.
    pub fn path_of(&self, id: u32) -> Option<String> {
        if id == 0 {
            return None;
        }
        self.tables.iter().find_map(|table| table.path(id))
    }

    /// The FileDataIDs the tables name, one named by both counted twice.
    pub fn len(&self) -> usize {
        self.tables.iter().map(PathTable::len).sum()
    }
}
