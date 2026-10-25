//! MPQ archives of the client, read and written as WoW-mods' installer does (`mpq_archive.py`):
//! formats v1 and v2, files in sectors or whole, zlib, encrypted or not. A file written into an
//! archive goes after everything it holds, its tables after it, and the header is written last:
//! until then the archive described is the old one, so that a failure leaves it as it was. A
//! replaced file keeps its block entry; a new one takes a free hash entry and a new block, and its
//! name joins `(listfile)`; `(attributes)` follows. The space of the old version is not given back
//! (compaction, apart).

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const M32: u64 = 0xFFFF_FFFF;
const SIGNATURE: &[u8; 4] = b"MPQ\x1a";
const USER_DATA: &[u8; 4] = b"MPQ\x1b";
const FILE_IMPLODE: u32 = 0x0000_0100;
const FILE_COMPRESS: u32 = 0x0000_0200;
const FILE_ENCRYPTED: u32 = 0x0001_0000;
const FILE_FIX_KEY: u32 = 0x0002_0000;
const FILE_SINGLE_UNIT: u32 = 0x0100_0000;
const FILE_DELETE_MARKER: u32 = 0x0200_0000;
const FILE_SECTOR_CRC: u32 = 0x0400_0000;
const FILE_EXISTS: u32 = 0x8000_0000;
const HASH_EMPTY: u32 = 0xFFFF_FFFF;
const HASH_DELETED: u32 = 0xFFFF_FFFE;

fn crypt_table() -> &'static [u32; 0x500] {
    static TABLE: std::sync::OnceLock<[u32; 0x500]> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = [0u32; 0x500];
        let mut seed: u32 = 0x0010_0001;
        for i in 0..0x100 {
            let mut index = i;
            for _ in 0..5 {
                seed = (seed * 125 + 3) % 0x2A_AAAB;
                let high = (seed & 0xFFFF) << 16;
                seed = (seed * 125 + 3) % 0x2A_AAAB;
                table[index] = high | (seed & 0xFFFF);
                index += 0x100;
            }
        }
        table
    })
}

/// The hash of a file's name: 0 its place in the table, 1 and 2 its checks, 3 its key.
pub fn hash(name: &str, kind: u32) -> u32 {
    let table = crypt_table();
    let (mut s1, mut s2): (u32, u32) = (0x7FED_7FED, 0xEEEE_EEEE);
    for c in name.replace('/', "\\").bytes() {
        let c = c.to_ascii_uppercase();
        s1 = table[((kind << 8) + u32::from(c)) as usize] ^ s1.wrapping_add(s2);
        s2 = u32::from(c)
            .wrapping_add(s1)
            .wrapping_add(s2)
            .wrapping_add(s2 << 5)
            .wrapping_add(3);
    }
    s1
}

fn decrypt(data: &mut [u8], mut key: u32) {
    let table = crypt_table();
    let mut s: u32 = 0xEEEE_EEEE;
    for word in data.as_chunks_mut::<4>().0.iter_mut() {
        s = s.wrapping_add(table[0x400 + (key & 0xFF) as usize]);
        let plain = u32::from_le_bytes(*word) ^ key.wrapping_add(s);
        key = ((!key) << 0x15).wrapping_add(0x1111_1111) | (key >> 0x0B);
        s = plain.wrapping_add(s).wrapping_add(s << 5).wrapping_add(3);
        word.copy_from_slice(&plain.to_le_bytes());
    }
}

fn encrypt(data: &mut [u8], mut key: u32) {
    let table = crypt_table();
    let mut s: u32 = 0xEEEE_EEEE;
    for word in data.as_chunks_mut::<4>().0.iter_mut() {
        s = s.wrapping_add(table[0x400 + (key & 0xFF) as usize]);
        let plain = u32::from_le_bytes(*word);
        word.copy_from_slice(&(plain ^ key.wrapping_add(s)).to_le_bytes());
        key = ((!key) << 0x15).wrapping_add(0x1111_1111) | (key >> 0x0B);
        s = plain.wrapping_add(s).wrapping_add(s << 5).wrapping_add(3);
    }
}

pub fn crc32(bytes: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        std::array::from_fn(|n| {
            let mut c = n as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            c
        })
    });
    let mut c = 0xFFFF_FFFFu32;
    for b in bytes {
        c = table[((c ^ u32::from(*b)) & 0xFF) as usize] ^ (c >> 8);
    }
    !c
}

/// MD5 (RFC 1321), for `(attributes)`.
pub fn md5(input: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14,
        20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6,
        10, 15, 21,
    ];
    let k: [u32; 64] = std::array::from_fn(|i| ((i as f64 + 1.0).sin().abs() * 4_294_967_296.0) as u32);
    let mut state: [u32; 4] = [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476];
    let mut message = input.to_vec();
    let bits = (input.len() as u64).wrapping_mul(8);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bits.to_le_bytes());
    for chunk in message.as_chunks::<64>().0 {
        let m: [u32; 16] =
            std::array::from_fn(|i| u32::from_le_bytes(chunk[i * 4..i * 4 + 4].try_into().expect("four")));
        let [mut a, mut b, mut c, mut d] = state;
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        state = [
            state[0].wrapping_add(a),
            state[1].wrapping_add(b),
            state[2].wrapping_add(c),
            state[3].wrapping_add(d),
        ];
    }
    let mut out = [0u8; 16];
    for (i, word) in state.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    out
}

fn hash_key() -> u32 {
    hash("(hash table)", 3)
}

fn block_key() -> u32 {
    hash("(block table)", 3)
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct HashEntry {
    a: u32,
    b: u32,
    locale: u16,
    platform: u16,
    block: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Block {
    position: u32,
    packed: u32,
    size: u32,
    flags: u32,
}

/// An archive opened: its tables in memory.
pub struct Archive {
    pub path: PathBuf,
    file_size: u64,
    offset: u64,
    archive_size: u64,
    version: u16,
    pub sector_size: usize,
    hi_block_table: u64,
    hashes: Vec<HashEntry>,
    blocks: Vec<Block>,
    hi_blocks: Vec<u16>,
    signed: bool,
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().expect("four"))
}

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(b[at..at + 2].try_into().expect("two"))
}

fn read_at(file: &mut std::fs::File, at: u64, len: usize) -> Result<Vec<u8>, String> {
    let mut out = vec![0u8; len];
    file.seek(SeekFrom::Start(at)).map_err(|e| e.to_string())?;
    file.read_exact(&mut out).map_err(|e| e.to_string())?;
    Ok(out)
}

impl Archive {
    pub fn open(path: &Path) -> Result<Self, String> {
        let shown = path.display().to_string();
        let mut file = std::fs::File::open(path).map_err(|e| format!("{shown}: {e}"))?;
        let file_size = file.metadata().map_err(|e| e.to_string())?.len();
        let start = read_at(&mut file, 0, 16)?;
        let offset = if &start[..4] == SIGNATURE {
            0
        } else if &start[..4] == USER_DATA {
            u64::from(le32(&start, 8))
        } else {
            return Err(format!("{shown}: not an MPQ archive"));
        };
        let header = read_at(&mut file, offset, 44.min((file_size - offset) as usize))?;
        let archive_size = u64::from(le32(&header, 8));
        let version = le16(&header, 12);
        if version > 1 {
            return Err(format!("{shown}: MPQ format v{} (v1 and v2 only)", version + 1));
        }
        let sector_size = 512usize << (le16(&header, 14) & 0xFF);
        let (mut hash_pos, mut block_pos) = (u64::from(le32(&header, 16)), u64::from(le32(&header, 20)));
        let (hash_count, block_count) = (le32(&header, 24) as usize, le32(&header, 28) as usize);
        let mut hi_block_table = 0;
        if version == 1 {
            hi_block_table = u64::from_le_bytes(header[32..40].try_into().expect("eight"));
            hash_pos |= u64::from(le16(&header, 40)) << 32;
            block_pos |= u64::from(le16(&header, 42)) << 32;
        }
        if hash_count == 0 || !hash_count.is_power_of_two() {
            return Err(format!("{shown}: a hash table of {hash_count} entries"));
        }
        let mut raw = read_at(&mut file, offset + hash_pos, hash_count * 16)?;
        decrypt(&mut raw, hash_key());
        let hashes = raw
            .as_chunks::<16>()
            .0
            .iter()
            .map(|e| HashEntry {
                a: le32(e, 0),
                b: le32(e, 4),
                locale: le16(e, 8),
                platform: le16(e, 10),
                block: le32(e, 12),
            })
            .collect();
        let mut raw = read_at(&mut file, offset + block_pos, block_count * 16)?;
        decrypt(&mut raw, block_key());
        let blocks = raw
            .as_chunks::<16>()
            .0
            .iter()
            .map(|e| Block {
                position: le32(e, 0),
                packed: le32(e, 4),
                size: le32(e, 8),
                flags: le32(e, 12),
            })
            .collect();
        let hi_blocks = if hi_block_table != 0 {
            let raw = read_at(&mut file, offset + hi_block_table, block_count * 2)?;
            raw.as_chunks::<2>().0.iter().map(|b| u16::from_le_bytes(*b)).collect()
        } else {
            vec![0; block_count]
        };
        // a strong signature after the archive: changing it would break it
        let end = offset + archive_size;
        let signed = file_size > end + 4 && read_at(&mut file, end, 4)? == b"NGIS";
        Ok(Self {
            path: path.to_path_buf(),
            file_size,
            offset,
            archive_size,
            version,
            sector_size,
            hi_block_table,
            hashes,
            blocks,
            hi_blocks,
            signed,
        })
    }

    fn index_in(hashes: &[HashEntry], blocks: &[Block], name: &str) -> Option<usize> {
        let mask = hashes.len() - 1;
        let start = hash(name, 0) as usize & mask;
        let (a, b) = (hash(name, 1), hash(name, 2));
        for step in 0..hashes.len() {
            let i = (start + step) & mask;
            let e = hashes[i];
            if e.block == HASH_EMPTY {
                return None;
            }
            if e.a == a && e.b == b && e.block != HASH_DELETED && (e.block as usize) < blocks.len() {
                let block = blocks[e.block as usize];
                if block.flags & FILE_EXISTS != 0 && block.flags & FILE_DELETE_MARKER == 0 {
                    return Some(i);
                }
            }
        }
        None
    }

    fn index(&self, name: &str) -> Option<usize> {
        Self::index_in(&self.hashes, &self.blocks, name)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.index(name).is_some()
    }

    /// The size of a file once read, if it is there.
    pub fn size(&self, name: &str) -> Option<u64> {
        self.index(name)
            .map(|i| u64::from(self.blocks[self.hashes[i].block as usize].size))
    }

    pub fn read(&self, name: &str) -> Result<Vec<u8>, String> {
        let i = self
            .index(name)
            .ok_or_else(|| format!("{name} is not in {}", self.path.display()))?;
        self.read_block(self.hashes[i].block as usize, name)
    }

    fn read_block(&self, index: usize, name: &str) -> Result<Vec<u8>, String> {
        let block = self.blocks[index];
        let position = u64::from(block.position) | (u64::from(self.hi_blocks[index]) << 32);
        if block.flags & FILE_IMPLODE != 0 {
            return Err(format!("{name}: PKWARE compression (implode) is not read here"));
        }
        let mut file = std::fs::File::open(&self.path).map_err(|e| e.to_string())?;
        let mut data = read_at(&mut file, self.offset + position, block.packed as usize)?;
        let size = block.size as usize;
        let encrypted = block.flags & FILE_ENCRYPTED != 0;
        let mut key = 0u32;
        if encrypted {
            key = hash(name.replace('/', "\\").rsplit('\\').next().unwrap_or(name), 3);
            if block.flags & FILE_FIX_KEY != 0 {
                key = key.wrapping_add(block.position) ^ block.size;
            }
        }
        let compressed = block.flags & FILE_COMPRESS != 0;
        if block.flags & FILE_SINGLE_UNIT != 0 {
            if encrypted {
                decrypt(&mut data, key);
            }
            if compressed && (block.packed as usize) < size {
                return decompress(&data, size, name);
            }
            data.truncate(size);
            return Ok(data);
        }
        let count = size.div_ceil(self.sector_size);
        if !compressed {
            if encrypted {
                for (s, sector) in data.chunks_mut(self.sector_size).enumerate() {
                    decrypt(sector, key.wrapping_add(s as u32));
                }
            }
            data.truncate(size);
            return Ok(data);
        }
        let offsets_count = count + 1 + usize::from(block.flags & FILE_SECTOR_CRC != 0);
        let mut table = data
            .get(..offsets_count * 4)
            .ok_or("a sector table past the file")?
            .to_vec();
        if encrypted {
            decrypt(&mut table, key.wrapping_sub(1));
        }
        let offsets: Vec<usize> = table
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| u32::from_le_bytes(*b) as usize)
            .collect();
        let mut out = Vec::with_capacity(size);
        for s in 0..count {
            let expected = self.sector_size.min(size - s * self.sector_size);
            let mut sector = data
                .get(offsets[s]..offsets[s + 1])
                .ok_or("a sector past the file")?
                .to_vec();
            if encrypted {
                decrypt(&mut sector, key.wrapping_add(s as u32));
            }
            if sector.len() < expected {
                out.extend(decompress(&sector, expected, name)?);
            } else {
                out.extend_from_slice(&sector[..expected]);
            }
        }
        Ok(out)
    }
}

fn decompress(chunk: &[u8], expected: usize, name: &str) -> Result<Vec<u8>, String> {
    let (method, rest) = chunk.split_first().ok_or("an empty sector")?;
    let raw = match method {
        0x02 => miniz_oxide::inflate::decompress_to_vec_zlib(rest).map_err(|e| format!("{name}: zlib: {e:?}"))?,
        other => {
            return Err(format!(
                "{name}: compression 0x{other:02X} is not read here (zlib only)"
            ));
        }
    };
    if raw.len() != expected {
        return Err(format!("{name}: {} bytes decompressed, {expected} expected", raw.len()));
    }
    Ok(raw)
}

/// The file as zlib sectors after their table of offsets.
fn compress_sectors(raw: &[u8], sector_size: usize) -> Vec<u8> {
    let count = raw.len().div_ceil(sector_size).max(1);
    let chunks: Vec<Vec<u8>> = (0..count)
        .map(|i| {
            let sector = &raw[(i * sector_size).min(raw.len())..((i + 1) * sector_size).min(raw.len())];
            let packed = miniz_oxide::deflate::compress_to_vec_zlib(sector, 9);
            // a sector that does not shrink is kept as it is, without a method byte
            if packed.len() + 1 < sector.len() {
                let mut out = vec![0x02];
                out.extend(packed);
                out
            } else {
                sector.to_vec()
            }
        })
        .collect();
    let mut out = Vec::new();
    let mut pos = ((count + 1) * 4) as u32;
    for c in &chunks {
        out.extend_from_slice(&pos.to_le_bytes());
        pos += c.len() as u32;
    }
    out.extend_from_slice(&pos.to_le_bytes());
    for c in chunks {
        out.extend(c);
    }
    out
}

fn filetime_now() -> u64 {
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    (since.as_secs_f64() * 10_000_000.0) as u64 + 116_444_736_000_000_000
}

/// `(attributes)` resized for a block table of `after` blocks, with the checksums of the files
/// written; none for a layout not known (the file then left as it is).
fn updated_attributes(raw: &[u8], before: usize, after: usize, written: &[(usize, &[u8])]) -> Option<Vec<u8>> {
    if raw.len() < 8 {
        return None;
    }
    let (version, flags) = (le32(raw, 0), le32(raw, 4));
    if version != 100 || flags & !0xF != 0 {
        return None;
    }
    let per =
        if flags & 1 != 0 { 4 } else { 0 } + if flags & 2 != 0 { 8 } else { 0 } + if flags & 4 != 0 { 16 } else { 0 };
    let patch_bit = flags & 8 != 0;
    let patch_size = |shape: u8, n: usize| match shape {
        1 => n.div_ceil(8),
        2 => n * 4,
        _ => 0,
    };
    let mut layouts = vec![
        (before, if patch_bit { 1 } else { 0 }),
        (before.saturating_sub(1), if patch_bit { 1 } else { 0 }),
    ];
    if patch_bit {
        layouts.push((before, 0));
        layouts.push((before, 2));
    }
    let (n, shape) = layouts
        .into_iter()
        .find(|(n, shape)| *n > 0 && raw.len() == 8 + n * per + patch_size(*shape, before))?;
    let mut pos = 8;
    let mut crcs = (flags & 1 != 0).then(|| {
        let v: Vec<u32> = (0..n).map(|k| le32(raw, pos + 4 * k)).collect();
        pos += 4 * n;
        v
    });
    let mut times = (flags & 2 != 0).then(|| {
        let v: Vec<u64> = (0..n)
            .map(|k| u64::from_le_bytes(raw[pos + 8 * k..pos + 8 * k + 8].try_into().expect("eight")))
            .collect();
        pos += 8 * n;
        v
    });
    let mut md5s = (flags & 4 != 0).then(|| {
        let v: Vec<[u8; 16]> = (0..n)
            .map(|k| raw[pos + 16 * k..pos + 16 * k + 16].try_into().expect("sixteen"))
            .collect();
        pos += 16 * n;
        v
    });
    let tail = raw[pos..].to_vec();
    let gap = before - n;
    let n2 = after - gap;
    if let Some(v) = crcs.as_mut() {
        v.resize(n2, 0);
    }
    if let Some(v) = times.as_mut() {
        v.resize(n2, 0);
    }
    if let Some(v) = md5s.as_mut() {
        v.resize(n2, [0; 16]);
    }
    let now = filetime_now();
    for (index, content) in written {
        if *index < n2 {
            if let Some(v) = crcs.as_mut() {
                v[*index] = crc32(content);
            }
            if let Some(v) = times.as_mut() {
                v[*index] = now;
            }
            if let Some(v) = md5s.as_mut() {
                v[*index] = md5(content);
            }
        }
    }
    let new_size = patch_size(shape, after);
    let mut out = Vec::new();
    out.extend_from_slice(&version.to_le_bytes());
    out.extend_from_slice(&flags.to_le_bytes());
    for c in crcs.unwrap_or_default() {
        out.extend_from_slice(&c.to_le_bytes());
    }
    for t in times.unwrap_or_default() {
        out.extend_from_slice(&t.to_le_bytes());
    }
    for m in md5s.unwrap_or_default() {
        out.extend_from_slice(&m);
    }
    let mut tail = tail;
    tail.resize(new_size, 0);
    out.extend(tail);
    Some(out)
}

/// What a write would do: the files written, the bytes the archive grows by, its end after.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub grows: u64,
    pub end: u64,
}

/// Writes `files` (name, content) into the archive at `path`: everything prepared and checked in
/// memory first; with `check_only`, nothing is written (the plan tells what would be). Refused: a
/// signed archive, a full hash table, a v1 archive past 4 GB.
pub fn write_into(path: &Path, files: &[(String, Vec<u8>)], check_only: bool) -> Result<Plan, String> {
    let shown = path.display().to_string();
    let a = Archive::open(path)?;
    if a.signed {
        return Err(format!(
            "{shown} carries a strong signature: changing it would break it"
        ));
    }
    let mask = a.hashes.len() - 1;
    let mut blocks = a.blocks.clone();
    let mut hi_blocks = a.hi_blocks.clone();
    let mut hashes = a.hashes.clone();
    let mut contents: Vec<(usize, Vec<u8>)> = Vec::new();
    let mut new_names = Vec::new();
    for (name, content) in files {
        if let Some(i) = Archive::index_in(&hashes, &blocks, name) {
            contents.push((hashes[i].block as usize, content.clone()));
            continue;
        }
        let free = hashes
            .iter()
            .filter(|e| e.block == HASH_EMPTY || e.block == HASH_DELETED)
            .count();
        // a table without a free entry can no longer answer "absent": one always stays free
        if free < 2 {
            return Err(format!("{shown}: hash table full, {name} cannot be added"));
        }
        let start = hash(name, 0) as usize & mask;
        let j = (0..hashes.len())
            .map(|step| (start + step) & mask)
            .find(|j| hashes[*j].block == HASH_EMPTY || hashes[*j].block == HASH_DELETED)
            .expect("a free entry");
        hashes[j] = HashEntry {
            a: hash(name, 1),
            b: hash(name, 2),
            locale: 0,
            platform: 0,
            block: blocks.len() as u32,
        };
        contents.push((blocks.len(), content.clone()));
        blocks.push(Block::default());
        hi_blocks.push(0);
        new_names.push(name.clone());
    }
    if !new_names.is_empty() && a.contains("(listfile)") {
        let before = String::from_utf8_lossy(&a.read("(listfile)")?).into_owned();
        let known: std::collections::HashSet<String> = before
            .split(['\r', '\n', ';'])
            .map(|n| n.trim().to_uppercase())
            .collect();
        let added: Vec<&String> = new_names
            .iter()
            .filter(|n| !known.contains(&n.to_uppercase()))
            .collect();
        if !added.is_empty() {
            let mut text = before.trim_end_matches(['\r', '\n']).to_owned();
            for name in added {
                text.push_str("\r\n");
                text.push_str(name);
            }
            text.push_str("\r\n");
            let i = a.index("(listfile)").expect("contained");
            contents.push((hashes[i].block as usize, text.into_bytes()));
        }
    }
    if let Some(i) = a.index("(attributes)") {
        let attributes = hashes[i].block as usize;
        let written: Vec<(usize, &[u8])> = contents
            .iter()
            .filter(|(k, _)| *k != attributes)
            .map(|(k, v)| (*k, v.as_slice()))
            .collect();
        if let Some(new) = updated_attributes(&a.read("(attributes)")?, a.blocks.len(), blocks.len(), &written) {
            contents.push((attributes, new));
        }
    }
    let stored: Vec<(usize, usize, Vec<u8>)> = contents
        .iter()
        .map(|(index, content)| (*index, content.len(), compress_sectors(content, a.sector_size)))
        .collect();
    let start = a.file_size.max(a.offset + a.archive_size);
    let mut pos = start - a.offset;
    for (index, size, packed) in &stored {
        blocks[*index] = Block {
            position: (pos & M32) as u32,
            packed: packed.len() as u32,
            size: *size as u32,
            flags: FILE_EXISTS | FILE_COMPRESS,
        };
        hi_blocks[*index] = (pos >> 32) as u16;
        pos += packed.len() as u64;
    }
    let hash_pos = pos;
    let block_pos = hash_pos + hashes.len() as u64 * 16;
    let mut end = block_pos + blocks.len() as u64 * 16;
    let mut hi_pos = 0;
    if a.version == 1 && (a.hi_block_table != 0 || hi_blocks.iter().any(|h| *h != 0)) {
        hi_pos = end;
        end += hi_blocks.len() as u64 * 2;
    }
    if a.version == 0 && end > M32 {
        return Err(format!("{shown}: a v1 archive cannot go past 4 GB (compaction needed)"));
    }
    let plan = Plan {
        grows: (a.offset + end).saturating_sub(a.file_size),
        end: a.offset + end,
    };
    if check_only {
        return Ok(plan);
    }
    let mut table = Vec::with_capacity(hashes.len() * 16);
    for e in &hashes {
        table.extend_from_slice(&e.a.to_le_bytes());
        table.extend_from_slice(&e.b.to_le_bytes());
        table.extend_from_slice(&e.locale.to_le_bytes());
        table.extend_from_slice(&e.platform.to_le_bytes());
        table.extend_from_slice(&e.block.to_le_bytes());
    }
    encrypt(&mut table, hash_key());
    let mut block_table = Vec::with_capacity(blocks.len() * 16);
    for b in &blocks {
        for v in [b.position, b.packed, b.size, b.flags] {
            block_table.extend_from_slice(&v.to_le_bytes());
        }
    }
    encrypt(&mut block_table, block_key());
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| format!("{shown}: {e}"))?;
    let io = |e: std::io::Error| format!("{shown}: {e}");
    // the data and the tables after everything the file holds; the header last
    file.seek(SeekFrom::Start(start)).map_err(io)?;
    for (_, _, packed) in &stored {
        file.write_all(packed).map_err(io)?;
    }
    file.write_all(&table).map_err(io)?;
    file.write_all(&block_table).map_err(io)?;
    if hi_pos != 0 {
        for h in &hi_blocks {
            file.write_all(&h.to_le_bytes()).map_err(io)?;
        }
    }
    file.sync_all().map_err(io)?;
    let header_len = if a.version == 1 { 44 } else { 32 };
    let mut header = read_at(&mut file, a.offset, header_len)?;
    header[8..12].copy_from_slice(&((end & M32) as u32).to_le_bytes());
    header[16..20].copy_from_slice(&((hash_pos & M32) as u32).to_le_bytes());
    header[20..24].copy_from_slice(&((block_pos & M32) as u32).to_le_bytes());
    header[24..28].copy_from_slice(&(hashes.len() as u32).to_le_bytes());
    header[28..32].copy_from_slice(&(blocks.len() as u32).to_le_bytes());
    if a.version == 1 {
        header[32..40].copy_from_slice(&hi_pos.to_le_bytes());
        header[40..42].copy_from_slice(&((hash_pos >> 32) as u16).to_le_bytes());
        header[42..44].copy_from_slice(&((block_pos >> 32) as u16).to_le_bytes());
    }
    file.seek(SeekFrom::Start(a.offset)).map_err(io)?;
    file.write_all(&header).map_err(io)?;
    file.sync_all().map_err(io)?;
    Ok(plan)
}

/// A new v1 archive holding `files` and their `(listfile)` (for the tests).
pub fn create(path: &Path, files: &[(String, Vec<u8>)], hash_entries: usize) -> Result<(), String> {
    let mut all: Vec<(String, Vec<u8>)> = files.to_vec();
    let mut list = String::new();
    for (name, _) in files {
        list.push_str(name);
        list.push_str("\r\n");
    }
    all.push(("(listfile)".into(), list.into_bytes()));
    let sector_size = 4096;
    let mut hashes = vec![
        HashEntry {
            a: HASH_EMPTY,
            b: HASH_EMPTY,
            locale: 0xFFFF,
            platform: 0xFFFF,
            block: HASH_EMPTY,
        };
        hash_entries
    ];
    let mut blocks = Vec::new();
    let mut data = Vec::new();
    let mut pos = 32u32;
    for (name, content) in &all {
        let packed = compress_sectors(content, sector_size);
        let mut i = hash(name, 0) as usize & (hash_entries - 1);
        while hashes[i].block != HASH_EMPTY {
            i = (i + 1) & (hash_entries - 1);
        }
        hashes[i] = HashEntry {
            a: hash(name, 1),
            b: hash(name, 2),
            locale: 0,
            platform: 0,
            block: blocks.len() as u32,
        };
        blocks.push(Block {
            position: pos,
            packed: packed.len() as u32,
            size: content.len() as u32,
            flags: FILE_EXISTS | FILE_COMPRESS,
        });
        pos += packed.len() as u32;
        data.extend(packed);
    }
    let hash_pos = pos;
    let block_pos = hash_pos + hash_entries as u32 * 16;
    let end = block_pos + blocks.len() as u32 * 16;
    let mut out = SIGNATURE.to_vec();
    for v in [32u32, end] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&3u16.to_le_bytes());
    for v in [hash_pos, block_pos, hash_entries as u32, blocks.len() as u32] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend(data);
    let mut table = Vec::new();
    for e in &hashes {
        table.extend_from_slice(&e.a.to_le_bytes());
        table.extend_from_slice(&e.b.to_le_bytes());
        table.extend_from_slice(&e.locale.to_le_bytes());
        table.extend_from_slice(&e.platform.to_le_bytes());
        table.extend_from_slice(&e.block.to_le_bytes());
    }
    encrypt(&mut table, hash_key());
    out.extend(table);
    let mut block_table = Vec::new();
    for b in &blocks {
        for v in [b.position, b.packed, b.size, b.flags] {
            block_table.extend_from_slice(&v.to_le_bytes());
        }
    }
    encrypt(&mut block_table, block_key());
    out.extend(block_table);
    std::fs::write(path, out).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums_as_the_references() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        let hex: String = md5(b"abc").iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "900150983cd24fb0d6963f7d28e17f72");
        // the known key of the hash table
        assert_eq!(hash("(hash table)", 3), 0xC3AF_3770);
    }

    #[test]
    fn a_file_replaced_and_one_added_read_back() {
        let dir = std::env::temp_dir().join(format!("uniwow-mpq-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.MPQ");
        let big: Vec<u8> = (0..20_000u32).flat_map(|v| (v % 251).to_le_bytes()).collect();
        create(
            &path,
            &[
                ("Spells\\A.m2".into(), b"first".to_vec()),
                ("Spells\\B.blp".into(), big.clone()),
            ],
            16,
        )
        .unwrap();
        let archive = Archive::open(&path).unwrap();
        assert_eq!(archive.read("spells/a.M2").unwrap(), b"first");
        assert_eq!(archive.read("Spells\\B.blp").unwrap(), big);
        let plan = write_into(&path, &[("Spells\\A.m2".into(), b"second, longer".to_vec())], true).unwrap();
        assert!(plan.grows > 0);
        assert_eq!(
            Archive::open(&path).unwrap().read("Spells\\A.m2").unwrap(),
            b"first",
            "a check writes nothing"
        );
        write_into(
            &path,
            &[
                ("Spells\\A.m2".into(), b"second, longer".to_vec()),
                ("Spells\\C.skin".into(), vec![7; 9000]),
            ],
            false,
        )
        .unwrap();
        let archive = Archive::open(&path).unwrap();
        assert_eq!(archive.read("Spells\\A.m2").unwrap(), b"second, longer");
        assert_eq!(archive.read("Spells\\B.blp").unwrap(), big);
        assert_eq!(archive.read("Spells\\C.skin").unwrap(), vec![7; 9000]);
        let list = String::from_utf8(archive.read("(listfile)").unwrap()).unwrap();
        assert!(list.contains("Spells\\C.skin"), "{list}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
