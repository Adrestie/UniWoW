//! One MPQ archive of the 3.3.5a client, read from any thread at once: its tables read once, its
//! files read by position, without a lock, each into one allocation of its size.
//!
//! The encryption table, the hash of names, the decryption of the tables and the search of the hash
//! table are copied from wow-mpq (warcraft-rs), and the reading adapted from it: THIRD_PARTY.md.
//! Formats 1 and 2 only, which are those of 3.3.5a; files stored, or compressed with zlib, in
//! sectors or in one unit. What the archives of 3.3.5a do not hold, the encryption of files and the
//! other compressions, is refused by name.

use std::cell::RefCell;
use std::fs::File;
use std::path::{Path, PathBuf};

use uniwow_api::miniz_oxide;

/// The signature of an archive, and of the user data some archives start with.
const SIGNATURE: &[u8; 4] = b"MPQ\x1a";
const USER_DATA: &[u8; 4] = b"MPQ\x1b";

/// The flags of a file in the block table.
pub const FILE_IMPLODE: u32 = 0x0000_0100;
pub const FILE_COMPRESS: u32 = 0x0000_0200;
pub const FILE_ENCRYPTED: u32 = 0x0001_0000;
pub const FILE_PATCH: u32 = 0x0010_0000;
pub const FILE_SINGLE_UNIT: u32 = 0x0100_0000;
pub const FILE_DELETE_MARKER: u32 = 0x0200_0000;
pub const FILE_SECTOR_CRC: u32 = 0x0400_0000;
pub const FILE_EXISTS: u32 = 0x8000_0000;

/// The compression of a sector that is zlib.
const ZLIB: u8 = 0x02;

/// An entry of the hash table that was never used, ending a search.
const HASH_EMPTY: u32 = 0xFFFF_FFFF;

// --- Copied from wow-mpq: crypto/keys.rs, crypto/hash.rs, crypto/decryption.rs, crypto/types.rs.

/// The kinds of hash of a name.
pub mod hash_type {
    pub const TABLE_OFFSET: u32 = 0x000;
    pub const NAME_A: u32 = 0x100;
    pub const NAME_B: u32 = 0x200;
    pub const FILE_KEY: u32 = 0x300;
}

const fn encryption_table() -> [u32; 0x500] {
    let mut table = [0u32; 0x500];
    let mut seed: u32 = 0x0010_0001;
    let mut index1 = 0;
    while index1 < 0x100 {
        let mut index2 = 0;
        while index2 < 5 {
            let table_index = index1 + index2 * 0x100;
            seed = seed.wrapping_mul(125).wrapping_add(3) % 0x2A_AAAB;
            let temp1 = (seed & 0xFFFF) << 0x10;
            seed = seed.wrapping_mul(125).wrapping_add(3) % 0x2A_AAAB;
            let temp2 = seed & 0xFFFF;
            table[table_index] = temp1 | temp2;
            index2 += 1;
        }
        index1 += 1;
    }
    table
}

pub const ENCRYPTION_TABLE: [u32; 0x500] = encryption_table();

/// The hash of a name, slashes taken as backslashes and letters as capitals.
pub fn hash_string(name: &str, hash_type: u32) -> u32 {
    let mut seed1: u32 = 0x7FED_7FED;
    let mut seed2: u32 = 0xEEEE_EEEE;
    for &byte in name.as_bytes() {
        let character = if byte == b'/' { b'\\' } else { byte.to_ascii_uppercase() };
        let index = hash_type.wrapping_add(u32::from(character)) as usize;
        seed1 = ENCRYPTION_TABLE[index] ^ seed1.wrapping_add(seed2);
        seed2 = u32::from(character)
            .wrapping_add(seed1)
            .wrapping_add(seed2)
            .wrapping_add(seed2 << 5)
            .wrapping_add(3);
    }
    seed1
}

/// Decrypts a block of numbers with `key`.
pub fn decrypt_block(data: &mut [u32], mut key: u32) {
    if key == 0 {
        return;
    }
    let mut seed: u32 = 0xEEEE_EEEE;
    for value in data.iter_mut() {
        seed = seed.wrapping_add(ENCRYPTION_TABLE[0x400 + (key & 0xFF) as usize]);
        let plain = *value ^ key.wrapping_add(seed);
        *value = plain;
        key = (!key << 0x15).wrapping_add(0x1111_1111) | (key >> 0x0B);
        seed = plain.wrapping_add(seed).wrapping_add(seed << 5).wrapping_add(3);
    }
}

// --- End of what is copied.

/// An entry of the hash table.
#[derive(Clone, Copy)]
struct HashEntry {
    name_a: u32,
    name_b: u32,
    locale: u16,
    block: u32,
}

/// A file of the block table: where its data is in the file of the archive, its packed and real
/// sizes, and its flags.
#[derive(Clone, Copy, Debug)]
pub struct Block {
    pub offset: u64,
    pub packed: u64,
    pub size: u64,
    pub flags: u32,
}

/// What an archive holds under a name.
#[derive(Clone, Copy, Debug)]
pub enum Entry {
    /// A file, by its index in the block table.
    File(usize),
    /// A delete marker: an archive read before this one holds the file, which a patch deleted.
    Deleted,
}

/// An archive open, its tables read.
/// The largest sector the archives of 3.3.5a could have: 512 << 23, 4 GB; theirs are of 4 KB.
const MAX_SECTOR_SHIFT: u16 = 23;
/// The largest file an archive may give, far beyond any of the client's.
const MAX_FILE_SIZE: u64 = 1 << 30;

/// The three hashes of a name an archive finds its file by.
pub struct NameHashes {
    name_a: u32,
    name_b: u32,
    table_offset: u32,
}

impl NameHashes {
    pub fn of(name: &str) -> Self {
        Self {
            name_a: hash_string(name, hash_type::NAME_A),
            name_b: hash_string(name, hash_type::NAME_B),
            table_offset: hash_string(name, hash_type::TABLE_OFFSET),
        }
    }
}

pub struct Archive {
    path: PathBuf,
    file: File,
    /// The length of the file, which no data of a block may pass.
    length: u64,
    sector_size: u64,
    hash: Vec<HashEntry>,
    blocks: Vec<Block>,
}

thread_local! {
    /// The packed bytes of a file and its table of sectors, reused by each thread for every file up
    /// to `KEPT_BUFFER` bytes.
    pub(crate) static PACKED: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// The most a thread's buffer holds: nearly every file of a map is smaller; a larger one takes an
/// allocation of its own, not kept.
pub(crate) const KEPT_BUFFER: usize = 8 << 20;

/// Reads `buffer.len()` bytes at `offset`, without moving a shared position.
fn read_at(file: &File, buffer: &mut [u8], offset: u64) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let mut done = 0;
        while done < buffer.len() {
            let read = file.seek_read(&mut buffer[done..], offset + done as u64)?;
            if read == 0 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            done += read;
        }
        Ok(())
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        file.read_exact_at(buffer, offset)
    }
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from(u32_at(bytes, at)) | (u64::from(u32_at(bytes, at + 4)) << 32)
}

/// Reads a table of `count` entries of 16 bytes at `offset`, decrypted with the key of `name`.
fn read_table(file: &File, offset: u64, count: u32, name: &str) -> Result<Vec<u32>, String> {
    let mut bytes = vec![0u8; count as usize * 16];
    read_at(file, &mut bytes, offset).map_err(|e| format!("its {name} cannot be read: {e}"))?;
    let mut numbers: Vec<u32> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|n| u32::from_le_bytes(*n))
        .collect();
    decrypt_block(&mut numbers, hash_string(name, hash_type::FILE_KEY));
    Ok(numbers)
}

impl Archive {
    /// Opens the archive at `path`: its header and its tables.
    pub fn open(path: &Path) -> Result<Self, String> {
        let file = File::open(path).map_err(|e| e.to_string())?;
        let length = file.metadata().map_err(|e| e.to_string())?.len();
        let mut header = [0u8; 44];
        let start = header.len().min(length as usize);
        read_at(&file, &mut header[..start], 0).map_err(|e| format!("no header: {e}"))?;
        // Some archives start with user data pointing to the header.
        let base = if &header[..4] == USER_DATA {
            let at = u64::from(u32_at(&header, 8));
            read_at(&file, &mut header, at).map_err(|e| format!("no header: {e}"))?;
            at
        } else {
            0
        };
        if &header[..4] != SIGNATURE {
            return Err("not an MPQ archive".to_owned());
        }
        let format = u16_at(&header, 0x0C);
        if format > 1 {
            return Err(format!(
                "format {} of MPQ, newer than those of 3.3.5a (1 and 2)",
                format + 1
            ));
        }
        let shift = u16_at(&header, 0x0E);
        if shift > MAX_SECTOR_SHIFT {
            return Err(format!("sectors of 512 << {shift} bytes, which no archive has"));
        }
        let sector_size = 512u64 << shift;
        let (hash_count, block_count) = (u32_at(&header, 0x18), u32_at(&header, 0x1C));
        // Positions are unsigned: those of a large archive of format 1 pass 2 GB.
        let (mut hash_at, mut block_at) = (u64::from(u32_at(&header, 0x10)), u64::from(u32_at(&header, 0x14)));
        let mut high_blocks_at = 0;
        if format == 1 {
            high_blocks_at = u64_at(&header, 0x20);
            hash_at |= u64::from(u16_at(&header, 0x28)) << 32;
            block_at |= u64::from(u16_at(&header, 0x2A)) << 32;
        }
        if !hash_count.is_power_of_two() {
            return Err(format!("its hash table has {hash_count} entries, not a power of two"));
        }
        for (at, count, name) in [
            (hash_at, hash_count, "hash table"),
            (block_at, block_count, "block table"),
        ] {
            if base + at + u64::from(count) * 16 > length {
                return Err(format!("its {name} goes past the end of the file"));
            }
        }
        let hash = read_table(&file, base + hash_at, hash_count, "(hash table)")?
            .as_chunks::<4>()
            .0
            .iter()
            .map(|entry| HashEntry {
                name_a: entry[0],
                name_b: entry[1],
                locale: (entry[2] & 0xFFFF) as u16,
                block: entry[3],
            })
            .collect();
        let mut high = vec![
            0u8;
            if high_blocks_at == 0 {
                0
            } else {
                block_count as usize * 2
            }
        ];
        if high_blocks_at != 0 {
            read_at(&file, &mut high, base + high_blocks_at).map_err(|e| format!("its high offsets: {e}"))?;
        }
        let blocks = read_table(&file, base + block_at, block_count, "(block table)")?
            .as_chunks::<4>()
            .0
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let high = if high.is_empty() {
                    0
                } else {
                    u64::from(u16_at(&high, index * 2)) << 32
                };
                Block {
                    offset: base + (u64::from(entry[0]) | high),
                    packed: u64::from(entry[1]),
                    size: u64::from(entry[2]),
                    flags: entry[3],
                }
            })
            .collect();
        Ok(Self {
            path: path.to_owned(),
            file,
            length,
            sector_size,
            hash,
            blocks,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// What the archive holds under `name`: the entry of the neutral locale first, as the client
    /// reads it, then any other.
    pub fn find(&self, name: &str) -> Option<Entry> {
        self.find_hashed(&NameHashes::of(name))
    }

    /// As `find`, for a name already hashed, as a chain of archives looks it up in each.
    pub fn find_hashed(&self, hashes: &NameHashes) -> Option<Entry> {
        let mask = self.hash.len() - 1;
        let (name_a, name_b) = (hashes.name_a, hashes.name_b);
        let start = hashes.table_offset as usize & mask;
        let mut found = None;
        for step in 0..self.hash.len() {
            let entry = self.hash[(start + step) & mask];
            if entry.block == HASH_EMPTY {
                break;
            }
            if entry.name_a == name_a && entry.name_b == name_b && (entry.block as usize) < self.blocks.len() {
                found = Some(entry);
                if entry.locale == 0 {
                    break;
                }
            }
        }
        let block = self.blocks[found?.block as usize];
        if block.flags & FILE_EXISTS == 0 {
            return None;
        }
        Some(if block.flags & FILE_DELETE_MARKER != 0 {
            Entry::Deleted
        } else {
            Entry::File(found?.block as usize)
        })
    }

    /// The bytes of the file of the block `index`, in one allocation of its size.
    pub fn read(&self, index: usize) -> Result<Vec<u8>, String> {
        let block = self.blocks[index];
        if block.flags & FILE_ENCRYPTED != 0 {
            return Err("an encrypted file, which the archives of 3.3.5a do not hold".to_owned());
        }
        if block.flags & FILE_IMPLODE != 0 {
            return Err("a file compressed with PKWare's implode, which the archives of 3.3.5a do not hold".to_owned());
        }
        if block.flags & FILE_PATCH != 0 {
            return Err("an incremental patch, which the archives of 3.3.5a do not hold".to_owned());
        }
        // A block damaged, or of an archive that is not the client's, must not ask for more than
        // the archive holds, nor for gigabytes.
        if block.offset.saturating_add(block.packed) > self.length {
            return Err("its data goes past the end of the archive".to_owned());
        }
        if block.size > MAX_FILE_SIZE || (block.flags & FILE_COMPRESS == 0 && block.size > block.packed) {
            return Err(format!("a file of {} bytes, more than its block holds", block.size));
        }
        let size = usize::try_from(block.size).map_err(|_| "a file too large".to_owned())?;
        let mut data = vec![0u8; size];
        if size == 0 {
            return Ok(data);
        }
        let failed = |error: std::io::Error| format!("cannot be read: {error}");
        if block.flags & FILE_COMPRESS == 0 {
            read_at(&self.file, &mut data, block.offset).map_err(failed)?;
            return Ok(data);
        }
        let unpack_into = |packed: &mut Vec<u8>, data: &mut [u8]| {
            if block.flags & FILE_SINGLE_UNIT != 0 {
                packed.resize(block.packed as usize, 0);
                read_at(&self.file, packed, block.offset).map_err(failed)?;
                unpack(packed, data)
            } else {
                self.read_sectors(block, packed, data)
            }
        };
        if block.packed > KEPT_BUFFER as u64 {
            unpack_into(&mut Vec::new(), &mut data)?;
        } else {
            PACKED.with_borrow_mut(|packed| unpack_into(packed, &mut data))?;
        }
        Ok(data)
    }

    /// The sectors of a compressed file: its table of sectors, then all its packed bytes in one
    /// read, each sector unpacked into its place.
    fn read_sectors(&self, block: Block, packed: &mut Vec<u8>, data: &mut [u8]) -> Result<(), String> {
        let failed = |error: std::io::Error| format!("cannot be read: {error}");
        let sectors = block.size.div_ceil(self.sector_size) as usize;
        let entries = sectors + 1 + usize::from(block.flags & FILE_SECTOR_CRC != 0);
        packed.resize(entries * 4, 0);
        read_at(&self.file, packed, block.offset).map_err(failed)?;
        let table: Vec<usize> = packed
            .as_chunks::<4>()
            .0
            .iter()
            .map(|n| u32::from_le_bytes(*n) as usize)
            .collect();
        let end = table[sectors];
        if end as u64 > block.packed || table.windows(2).take(sectors).any(|pair| pair[1] < pair[0]) {
            return Err("its table of sectors is broken".to_owned());
        }
        packed.resize(end, 0);
        read_at(&self.file, packed, block.offset).map_err(failed)?;
        let sector_size = self.sector_size as usize;
        for (sector, place) in data.chunks_mut(sector_size).enumerate() {
            unpack(&packed[table[sector]..table[sector + 1]], place)?;
        }
        Ok(())
    }

    /// The names the archive's list gives, `(listfile)`, if it has one.
    pub fn listed(&self) -> Result<Vec<String>, String> {
        let Some(Entry::File(index)) = self.find("(listfile)") else {
            return Ok(Vec::new());
        };
        let bytes = self.read(index)?;
        Ok(bytes
            .split(|byte| matches!(byte, b'\r' | b'\n' | b';'))
            .filter(|name| !name.is_empty())
            .map(|name| String::from_utf8_lossy(name).trim().to_owned())
            .filter(|name| !name.is_empty())
            .collect())
    }
}

/// A packed sector, or file in one unit, into `place`, whose size is what it unpacks to: stored as
/// is when as long, otherwise its first byte names its compression.
fn unpack(packed: &[u8], place: &mut [u8]) -> Result<(), String> {
    if packed.len() == place.len() {
        place.copy_from_slice(packed);
        return Ok(());
    }
    let (&method, compressed) = packed.split_first().ok_or("an empty sector")?;
    if method != ZLIB {
        return Err(format!(
            "a compression 0x{method:02X}, which the archives of 3.3.5a do not use (they use zlib)"
        ));
    }
    let unpacked =
        miniz_oxide::inflate::decompress_slice_iter_to_slice(place, std::iter::once(compressed), true, false)
            .map_err(|e| format!("zlib: {e:?}"))?;
    if unpacked != place.len() {
        return Err(format!("{unpacked} bytes unpacked where {} were expected", place.len()));
    }
    Ok(())
}
