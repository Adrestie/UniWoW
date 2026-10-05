//! The tables of paths of WarcraftXL, `TextureFilePath.db2` and `ModelFilePath.db2`: the FileDataID
//! of a file and its path, in a DB2 of a version WarcraftXL reads. WDC1, the version DB2Gen writes,
//! is read from the public description of the format; WDC2, `1SLC`, WDC3 and WDC5 are translated
//! from the reader of wow.export (MIT, see THIRD_PARTY.md), WDC2 taking the offsets of its strings
//! from their field, as WDC3 does. WDC4, which WarcraftXL does not read, is refused. A section
//! encrypted with a key the client lacks holds zeros, which name no file: it needs no reading of
//! its own.

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
pub(crate) const SPARSE: u16 = 1;

/// The tables of paths, in the order WarcraftXL looks a FileDataID up.
pub const TABLES: [&str; 2] = ["TextureFilePath.db2", "ModelFilePath.db2"];

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

    /// A map of offsets: the offset in the file of each record, and its size.
    fn offsets(&mut self, count: usize) -> Result<Vec<(usize, u16)>, String> {
        let bytes = self.take(count.checked_mul(6).ok_or("too many offsets")?)?;
        Ok(bytes
            .as_chunks::<6>()
            .0
            .iter()
            .map(|b| {
                (
                    u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize,
                    u16::from_le_bytes([b[4], b[5]]),
                )
            })
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

/// A section of a table, the whole table before WDC2.
#[derive(Default)]
struct Section {
    records: usize,
    records_size: usize,
    count: usize,
    strings: usize,
    strings_size: usize,
    ids: Vec<u32>,
    /// The rows copied: the id of the copy, then that of the row copied.
    copies: Vec<(u32, u32)>,
    /// A sparse section: for each record, its place in the section, its offset in the file, and
    /// its id when the map gives it.
    sparse: Vec<(usize, usize, Option<u32>)>,
}

/// A table, whatever its version.
struct Layout<'a> {
    bytes: &'a [u8],
    /// From WDC2 on, the offset of a string counts from its field, and the strings of all the
    /// sections follow the records of all the sections; before, it counts from the strings.
    from_field: bool,
    sparse: bool,
    record_size: usize,
    record_count: usize,
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

/// The records of a map of offsets by id, from `min_id`; an entry of no size holds none.
fn by_id(offsets: Vec<(usize, u16)>, min_id: u32) -> Vec<(usize, usize, Option<u32>)> {
    offsets
        .into_iter()
        .zip(min_id..)
        .filter(|((_, size), _)| *size != 0)
        .enumerate()
        .map(|(place, ((offset, _), id))| (place, offset, Some(id)))
        .collect()
}

/// How many entries a map of offsets by id holds.
fn id_span(min_id: u32, max_id: u32) -> usize {
    if max_id < min_id {
        0
    } else {
        (max_id - min_id) as usize + 1
    }
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
            WDC5 => Self::wdc(bytes, 5),
            WDC4 => Err("WDC4, which WarcraftXL does not read".to_owned()),
            _ => Err("not a DB2 of a version WarcraftXL reads (WDC1 to WDC3, WDC5)".to_owned()),
        }
    }

    /// WDC1: one section, its map of offsets by id, its pallets after the records.
    fn wdc1(bytes: &'a [u8]) -> Result<Self, String> {
        let mut at = Cursor::new(bytes, 4);
        let record_count = at.size()?;
        at.skip(4)?; // the count of the columns, which the storage of the columns gives
        let record_size = at.size()?;
        let strings_size = at.size()?;
        at.skip(8)?; // the hashes of the table and of its layout
        let min_id = at.u32()?;
        let max_id = at.u32()?;
        at.skip(4)?; // the locale
        let copy_size = at.size()?;
        let sparse = at.u16()? & SPARSE != 0;
        let id_index = usize::from(at.u16()?);
        let total_fields = at.size()?;
        at.skip(8)?; // where the bits packed start in a record, and the count of the lookup columns
        let map_offset = at.size()?;
        let ids_size = at.size()?;
        let info_size = at.size()?;
        let common_size = at.size()?;
        let pallet_size = at.size()?;
        at.skip(4)?; // the relations, last in the file
        at.skip(total_fields.checked_mul(4).ok_or("too many columns")?)?;
        let mut section = Section {
            records: at.at,
            ..Section::default()
        };
        if sparse {
            section.records_size = map_offset
                .checked_sub(section.records)
                .ok_or("its map of offsets before its records")?;
            at = Cursor::new(bytes, map_offset);
            section.sparse = by_id(at.offsets(id_span(min_id, max_id))?, min_id);
        } else {
            section.records_size = record_count.checked_mul(record_size).ok_or("too many records")?;
            section.count = record_count;
            at.skip(section.records_size)?;
            section.strings = at.at;
            section.strings_size = strings_size;
            at.skip(strings_size)?;
        }
        section.ids = at.u32s(ids_size / 4)?;
        section.copies = pairs(&at.u32s(copy_size / 8 * 2)?);
        let columns = columns(&mut at, info_size)?;
        let pallets = pallets(&mut at, &columns, pallet_size, common_size)?;
        Ok(Self {
            bytes,
            from_field: false,
            sparse,
            record_size,
            record_count,
            id_index,
            columns,
            pallets,
            sections: vec![section],
        })
    }

    /// WDC2 and after: the sections, each found by the offset its header gives, which steps over
    /// what WDC4 and WDC5 put before them.
    fn wdc(bytes: &'a [u8], version: u32) -> Result<Self, String> {
        // WDC5 starts with the version of its schema and the build that wrote it.
        let mut at = Cursor::new(bytes, if version == 5 { 4 + 4 + 128 } else { 4 });
        let record_count = at.size()?;
        at.skip(4)?; // the count of the columns, which the storage of the columns gives
        let record_size = at.size()?;
        at.skip(4)?; // the size of the strings, which each section gives
        at.skip(8)?; // the hashes of the table and of its layout
        let min_id = at.u32()?;
        let max_id = at.u32()?;
        at.skip(4)?; // the locale
        let sparse = at.u16()? & SPARSE != 0;
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
            let mut section = Section {
                records: file_offset,
                strings_size,
                ..Section::default()
            };
            let mut at = Cursor::new(bytes, file_offset);
            if version == 2 {
                let copy_size = headers.size()?;
                let map_offset = headers.size()?;
                let ids_size = headers.size()?;
                headers.skip(4)?; // the relations
                if sparse {
                    section.records_size = map_offset
                        .checked_sub(file_offset)
                        .ok_or("its map of offsets before its records")?;
                    at = Cursor::new(bytes, map_offset);
                    section.sparse = by_id(at.offsets(id_span(min_id, max_id))?, min_id);
                    section.strings_size = 0;
                } else {
                    section.count = count;
                    section.records_size = count.checked_mul(record_size).ok_or("too many records")?;
                    at.skip(section.records_size)?;
                    section.strings = at.at;
                    at.skip(strings_size)?;
                }
                section.ids = at.u32s(ids_size / 4)?;
                section.copies = pairs(&at.u32s(copy_size / 8 * 2)?);
            } else {
                let records_end = headers.size()?;
                let ids_size = headers.size()?;
                let relations_size = headers.size()?;
                let map_count = headers.size()?;
                let copy_count = headers.size()?;
                if sparse {
                    section.records_size = records_end
                        .checked_sub(file_offset)
                        .ok_or("its records end before they start")?;
                } else {
                    section.count = count;
                    section.records_size = count.checked_mul(record_size).ok_or("too many records")?;
                }
                at.skip(section.records_size)?;
                section.strings = at.at;
                at.skip(strings_size)?;
                section.ids = at.u32s(ids_size / 4)?;
                section.copies = pairs(&at.u32s(copy_count.checked_mul(2).ok_or("too many copies")?)?);
                let offsets = at.offsets(map_count)?;
                // The relations, then the ids of the map, which repeat the list of ids.
                at.skip(relations_size)?;
                at.skip(map_count * 4)?;
                if sparse {
                    section.sparse = offsets
                        .into_iter()
                        .enumerate()
                        .filter(|(_, (_, size))| *size != 0)
                        .map(|(place, (offset, _))| (place, offset, None))
                        .collect();
                }
            }
            sections.push(section);
        }
        Ok(Self {
            bytes,
            from_field: true,
            sparse,
            record_size,
            record_count,
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

    /// Where the path of the record `index` of the section `place` starts, its column at `start`;
    /// none for an empty path. `before` gives, for each section, the bytes of the records and of
    /// the strings of the sections before it.
    fn string(
        &self,
        path: usize,
        place: usize,
        index: usize,
        start: usize,
        before: &[(usize, usize)],
    ) -> Result<Option<usize>, String> {
        let stored = self.columns[path];
        if stored.compression != NONE || stored.size_bits != 32 {
            return Err("its paths are not offsets of 32 bits".to_owned());
        }
        let field = stored.offset_bits / 8;
        let offset = Cursor::new(self.bytes, start + field).size()?;
        if offset == 0 {
            return Ok(None);
        }
        let section = &self.sections[place];
        if !self.from_field {
            return if offset < section.strings_size {
                Ok(Some(section.strings + offset))
            } else {
                Err(format!("a path at {offset}, out of its strings"))
            };
        }
        let index = (before[place].0 + index * self.record_size + field + offset)
            .checked_sub(self.record_count * self.record_size)
            .ok_or("a path inside the records")?;
        self.sections
            .iter()
            .zip(before)
            .find(|(section, (_, base))| (*base..*base + section.strings_size).contains(&index))
            .map(|(section, (_, base))| Some(section.strings + index - base))
            .ok_or_else(|| format!("a path at {index}, out of the strings"))
    }

    /// The rows of a table of paths: each id, and where its path starts. The id is the one the map
    /// of offsets gives, else the one of the list of ids, else that of its column.
    fn paths(&self) -> Result<Vec<(u32, u32)>, String> {
        let (path, id_column) = match (self.columns.len(), self.id_index) {
            (1, _) => (0, None),
            (2, id @ 0..=1) => (1 - id, Some(id)),
            (count, _) => {
                return Err(format!(
                    "{count} columns, where a table of paths holds an id and a path"
                ));
            }
        };
        let mut before = Vec::with_capacity(self.sections.len());
        let (mut records, mut strings) = (0, 0);
        for section in &self.sections {
            before.push((records, strings));
            records += section.records_size;
            strings += section.strings_size;
        }
        let mut rows = Vec::new();
        let mut copies = Vec::new();
        for (place, section) in self.sections.iter().enumerate() {
            // A list of ids all zero gives each record its place.
            let all_zero = !section.ids.is_empty() && section.ids.iter().all(|id| *id == 0);
            let listed = |index: usize| {
                if all_zero {
                    Some(index as u32)
                } else {
                    section.ids.get(index).copied()
                }
            };
            let no_id = |index: usize| format!("no id for the record {index}");
            if self.sparse {
                for (index, offset, mapped) in &section.sparse {
                    let (found, inline) = self.sparse_record(path, *offset)?;
                    let id = mapped
                        .or_else(|| listed(*index))
                        .or(inline)
                        .ok_or_else(|| no_id(*index))?;
                    if let Some(found) = found {
                        rows.push((id, found as u32));
                    }
                }
            } else {
                for index in 0..section.count {
                    let start = section.records + index * self.record_size;
                    let id = match (listed(index), id_column) {
                        (Some(id), _) => id,
                        (None, Some(column)) => self.int(column, start)?,
                        (None, None) => return Err(no_id(index)),
                    };
                    if let Some(found) = self.string(path, place, index, start, &before)? {
                        rows.push((id, found as u32));
                    }
                }
            }
            copies.extend_from_slice(&section.copies);
        }
        rows.sort_by_key(|row| row.0);
        rows.dedup_by_key(|row| row.0);
        let copied: Vec<(u32, u32)> = copies
            .iter()
            .filter_map(|(copy, copied)| {
                let found = rows.binary_search_by_key(copied, |row| row.0).ok()?;
                Some((*copy, rows[found].1))
            })
            .collect();
        if !copied.is_empty() {
            rows.extend(copied);
            rows.sort_by_key(|row| row.0);
            rows.dedup_by_key(|row| row.0);
        }
        Ok(rows)
    }

    /// A record of variable size at `offset`, its columns one after the other, its path inline:
    /// where its path starts, none when empty, and its id when the record holds it.
    fn sparse_record(&self, path: usize, offset: usize) -> Result<(Option<usize>, Option<u32>), String> {
        let mut at = offset;
        let mut found = None;
        let mut inline = None;
        for column in 0..self.columns.len() {
            if column == path {
                let len = self
                    .bytes
                    .get(at..)
                    .and_then(|rest| rest.iter().position(|byte| *byte == 0))
                    .ok_or("a path without its end")?;
                if len > 0 {
                    found = Some(at);
                }
                at += len + 1;
            } else {
                let stored = self.columns[column];
                if stored.compression != NONE {
                    return Err("an id packed in a record of variable size".to_owned());
                }
                inline = Some(self.plain(stored, at)?);
                at += stored.size_bits / 8;
            }
        }
        Ok((found, inline))
    }
}

fn pairs(values: &[u32]) -> Vec<(u32, u32)> {
    values
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| (pair[0], pair[1]))
        .collect()
}

/// A table of paths: its bytes, and for each FileDataID, by increasing id, where its path starts.
pub struct PathTable {
    bytes: Vec<u8>,
    rows: Vec<(u32, u32)>,
}

impl PathTable {
    pub fn parse(bytes: Vec<u8>) -> Result<Self, String> {
        let rows = Layout::parse(&bytes)?.paths()?;
        Ok(Self { bytes, rows })
    }

    /// The path of the file `id`, none when the table does not name it.
    pub fn path(&self, id: u32) -> Option<String> {
        let found = self.rows.binary_search_by_key(&id, |row| row.0).ok()?;
        let rest = &self.bytes[self.rows[found].1 as usize..];
        let end = rest.iter().position(|byte| *byte == 0).unwrap_or(rest.len());
        Some(String::from_utf8_lossy(&rest[..end]).into_owned())
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }
}

/// The FileDataIDs of the client, through its tables of paths, in the order of `TABLES`.
#[derive(Default)]
pub struct FileIds {
    tables: Vec<PathTable>,
}

impl FileIds {
    /// Reads the tables of paths of the client in `client`, both at once, each loose in its
    /// `DBFilesClient` first, then in `chain`: a table missing is left out, as one that cannot be
    /// read, which is returned with why.
    pub fn load(client: &Path, chain: &Chain) -> (Self, Vec<String>) {
        let read = |name: &str| -> Result<Option<PathTable>, String> {
            let loose = client.join("DBFilesClient").join(name);
            let bytes = if loose.is_file() {
                std::fs::read(&loose).map_err(|e| format!("{}: {e}", loose.display()))?
            } else {
                match chain.read(&format!("DBFilesClient\\{name}"))? {
                    Some(bytes) => bytes,
                    None => return Ok(None),
                }
            };
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

    pub fn path_of(&self, id: u32) -> Option<String> {
        self.tables.iter().find_map(|table| table.path(id))
    }

    /// The FileDataIDs the tables name, one named by both counted twice.
    pub fn len(&self) -> usize {
        self.tables.iter().map(PathTable::len).sum()
    }
}
