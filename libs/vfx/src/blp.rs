//! BLP2 textures of the 3.3.5a client written from an RGBA image, as `tools/vfx/blp.py`: two
//! encodings with an 8-bit alpha and the whole chain of mipmaps down to 1 x 1.
//! - palette (colour encoding 1): 256 colours, one byte of alpha a texel; exact for the white or
//!   grey shapes a particle tints;
//! - DXT5 (colour encoding 2, alpha type 7): what most of Blizzard's spell textures use, for
//!   textures of many colours.
//!
//! `Auto` takes the palette when the image holds 256 colours or fewer. Read back too (palette,
//! DXT1, DXT3, DXT5, raw), the first level, to show a file.

use crate::texgen::Image;
use std::collections::HashMap;

pub const MAX_SIDE: u32 = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Palette,
    Dxt5,
    Auto,
}

impl Encoding {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "palette" => Some(Self::Palette),
            "dxt5" => Some(Self::Dxt5),
            "auto" => Some(Self::Auto),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Palette => "palette",
            Self::Dxt5 => "dxt5",
            Self::Auto => "auto",
        }
    }
}

/// The distinct colours of an image (alpha aside) in the order met, `None` past `most`.
fn colours(image: &Image, most: usize) -> Option<Vec<[u8; 3]>> {
    let mut seen: HashMap<[u8; 3], ()> = HashMap::new();
    let mut out = Vec::new();
    for texel in image.rgba.as_chunks::<4>().0 {
        let c = [texel[0], texel[1], texel[2]];
        if seen.insert(c, ()).is_none() {
            out.push(c);
            if out.len() > most {
                return None;
            }
        }
    }
    Some(out)
}

/// The mipmaps, the colour averaged weighted by alpha: black or garbage under transparent texels
/// never bleeds into the smaller levels.
pub fn mipmaps(image: &Image) -> Vec<Image> {
    let mut levels = vec![image.clone()];
    let (mut w, mut h) = (image.width as usize, image.height as usize);
    let mut cur: Vec<[f32; 4]> = image
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .map(|t| {
            let a = f32::from(t[3]) / 255.0;
            [
                f32::from(t[0]) / 255.0 * a,
                f32::from(t[1]) / 255.0 * a,
                f32::from(t[2]) / 255.0 * a,
                a,
            ]
        })
        .collect();
    while w > 1 || h > 1 {
        let (w2, h2) = ((w / 2).max(1), (h / 2).max(1));
        let (fx, fy) = (w / w2, h / h2);
        let mut next = vec![[0f32; 4]; w2 * h2];
        for y in 0..h2 {
            for x in 0..w2 {
                let mut sum = [0f32; 4];
                for dy in 0..fy {
                    for dx in 0..fx {
                        let t = cur[(y * fy + dy) * w + x * fx + dx];
                        for c in 0..4 {
                            sum[c] += t[c];
                        }
                    }
                }
                next[y * w2 + x] = sum.map(|v| v / (fx * fy) as f32);
            }
        }
        let mut level = Image::new(w2 as u32, h2 as u32);
        for (texel, t) in level.rgba.as_chunks_mut::<4>().0.iter_mut().zip(&next) {
            let a = t[3];
            for c in 0..3 {
                let v = if a > 1e-6 { t[c] / a.max(1e-6) } else { 0.0 };
                texel[c] = (v * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
            }
            texel[3] = (a * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
        }
        levels.push(level);
        cur = next;
        (w, h) = (w2, h2);
    }
    levels
}

/// 256 colours for an image of more: boxes of colours cut at their median along their widest
/// side, each giving its mean.
fn median_cut(image: &Image) -> Vec<[u8; 3]> {
    let mut counts: HashMap<[u8; 3], u32> = HashMap::new();
    for texel in image.rgba.as_chunks::<4>().0 {
        *counts.entry([texel[0], texel[1], texel[2]]).or_default() += 1;
    }
    let mut all: Vec<([u8; 3], u32)> = counts.into_iter().collect();
    all.sort();
    let mut boxes: Vec<Vec<([u8; 3], u32)>> = vec![all];
    let range = |b: &[([u8; 3], u32)]| -> (usize, u8) {
        (0..3)
            .map(|c| {
                let lo = b.iter().map(|(k, _)| k[c]).min().unwrap_or(0);
                let hi = b.iter().map(|(k, _)| k[c]).max().unwrap_or(0);
                (c, hi - lo)
            })
            .max_by_key(|(c, r)| (*r, std::cmp::Reverse(*c)))
            .unwrap_or((0, 0))
    };
    while boxes.len() < 256 {
        let Some((at, (axis, _))) = boxes
            .iter()
            .enumerate()
            .filter(|(_, b)| b.len() > 1)
            .map(|(at, b)| (at, range(b)))
            .max_by_key(|(at, (_, r))| (*r, std::cmp::Reverse(*at)))
        else {
            break;
        };
        let mut b = boxes.swap_remove(at);
        b.sort_by_key(|(k, _)| k[axis]);
        let total: u64 = b.iter().map(|(_, n)| u64::from(*n)).sum();
        let mut acc = 0u64;
        let mut cut = 1;
        for (index, (_, n)) in b.iter().enumerate() {
            acc += u64::from(*n);
            if acc * 2 >= total {
                cut = (index + 1).clamp(1, b.len() - 1);
                break;
            }
        }
        let rest = b.split_off(cut);
        boxes.push(b);
        boxes.push(rest);
    }
    boxes
        .iter()
        .map(|b| {
            let total: f64 = b.iter().map(|(_, n)| f64::from(*n)).sum();
            std::array::from_fn(|c| {
                let sum: f64 = b.iter().map(|(k, n)| f64::from(k[c]) * f64::from(*n)).sum();
                (sum / total).round() as u8
            })
        })
        .collect()
}

/// One palette for every level, from the colours of the largest; each level as its indices then
/// its alphas.
fn palette(levels: &[Image]) -> (Vec<u8>, Vec<Vec<u8>>) {
    let mut pal = colours(&levels[0], 256).unwrap_or_else(|| median_cut(&levels[0]));
    pal.truncate(256);
    pal.resize(256, [0, 0, 0]);
    let mut nearest: HashMap<[u8; 3], u8> = HashMap::new();
    let datas = levels
        .iter()
        .map(|level| {
            let mut indices = Vec::with_capacity(level.rgba.len() / 4);
            let mut alphas = Vec::with_capacity(level.rgba.len() / 4);
            for t in level.rgba.as_chunks::<4>().0 {
                let c = [t[0], t[1], t[2]];
                let index = *nearest.entry(c).or_insert_with(|| {
                    let mut best = (u32::MAX, 0u8);
                    for (index, p) in pal.iter().enumerate() {
                        let d: u32 = (0..3).map(|k| (i32::from(c[k]) - i32::from(p[k])).pow(2) as u32).sum();
                        if d < best.0 {
                            best = (d, index as u8);
                        }
                    }
                    best.1
                });
                indices.push(index);
                alphas.push(t[3]);
            }
            indices.extend(alphas);
            indices
        })
        .collect();
    let bytes = pal.iter().flat_map(|[r, g, b]| [*b, *g, *r, 0]).collect();
    (bytes, datas)
}

fn rgb565(c: [f32; 3]) -> u16 {
    let c = c.map(|v| v.clamp(0.0, 255.0) as u16);
    ((c[0] >> 3) << 11) | ((c[1] >> 2) << 5) | (c[2] >> 3)
}

fn from565(v: u16) -> [f32; 3] {
    let (r, g, b) = ((v >> 11) & 31, (v >> 5) & 63, v & 31);
    [(r << 3) | (r >> 2), (g << 2) | (g >> 4), (b << 3) | (b >> 2)].map(f32::from)
}

/// One block of 4 x 4 texels as DXT5.
fn dxt5_block(texels: &[[f32; 4]; 16]) -> [u8; 16] {
    let mut out = [0u8; 16];
    // alpha: two endpoints, eight interpolated values (a0 > a1)
    let max = texels.iter().map(|t| t[3]).fold(f32::NEG_INFINITY, f32::max);
    let min = texels.iter().map(|t| t[3]).fold(f32::INFINITY, f32::min);
    let (mut a0, mut a1) = (max.round_ties_even() as i32, min.round_ties_even() as i32);
    if a0 == a1 {
        if a0 > 0 {
            a1 = a0 - 1;
        } else {
            a0 = 1;
        }
    }
    let mut pal_a = [a0 as f32, a1 as f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
    for k in 1..7 {
        pal_a[k + 1] = ((7 - k) as f32 * a0 as f32 + k as f32 * a1 as f32) / 7.0;
    }
    let mut abits = 0u64;
    for (k, t) in texels.iter().enumerate() {
        let index = (0..8)
            .min_by(|i, j| (t[3] - pal_a[*i]).abs().total_cmp(&(t[3] - pal_a[*j]).abs()))
            .expect("eight");
        abits |= (index as u64) << (3 * k);
    }
    out[0] = a0 as u8;
    out[1] = a1 as u8;
    out[2..8].copy_from_slice(&abits.to_le_bytes()[..6]);

    // colour: endpoints along the main axis, weighted by alpha so that hidden texels do not count
    let weight: Vec<f32> = texels.iter().map(|t| t[3].max(1.0)).collect();
    let total: f32 = weight.iter().sum();
    let mut mean = [0f32; 3];
    for (t, w) in texels.iter().zip(&weight) {
        for c in 0..3 {
            mean[c] += t[c] * w;
        }
    }
    mean = mean.map(|v| v / total);
    let cen: Vec<[f32; 3]> = texels
        .iter()
        .map(|t| [t[0] - mean[0], t[1] - mean[1], t[2] - mean[2]])
        .collect();
    let mut cov = [[0f32; 3]; 3];
    for (c, w) in cen.iter().zip(&weight) {
        for i in 0..3 {
            for j in 0..3 {
                cov[i][j] += c[i] * w * c[j];
            }
        }
    }
    let mut axis = [1f32; 3];
    for _ in 0..8 {
        let next: [f32; 3] = std::array::from_fn(|i| (0..3).map(|j| cov[i][j] * axis[j]).sum());
        let norm = next.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-6);
        axis = next.map(|v| v / norm);
    }
    let proj: Vec<f32> = cen
        .iter()
        .map(|c| c[0] * axis[0] + c[1] * axis[1] + c[2] * axis[2])
        .collect();
    let lo_t = proj.iter().copied().fold(f32::INFINITY, f32::min);
    let hi_t = proj.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let lo = std::array::from_fn(|c| mean[c] + axis[c] * lo_t);
    let hi = std::array::from_fn(|c| mean[c] + axis[c] * hi_t);
    let (mut c0, mut c1) = (rgb565(hi), rgb565(lo));
    if c0 < c1 {
        std::mem::swap(&mut c0, &mut c1);
    }
    if c0 == c1 {
        if c0 > 0 {
            c1 = c0 - 1;
        } else {
            c0 = 1;
        }
    }
    let (e0, e1) = (from565(c0), from565(c1));
    let pal_c: [[f32; 3]; 4] = [
        e0,
        e1,
        std::array::from_fn(|c| (2.0 * e0[c] + e1[c]) / 3.0),
        std::array::from_fn(|c| (e0[c] + 2.0 * e1[c]) / 3.0),
    ];
    let mut cbits = 0u32;
    for (k, t) in texels.iter().enumerate() {
        let d = |p: &[f32; 3]| (0..3).map(|c| (t[c] - p[c]).powi(2)).sum::<f32>();
        let index = (0..4)
            .min_by(|i, j| d(&pal_c[*i]).total_cmp(&d(&pal_c[*j])))
            .expect("four");
        cbits |= (index as u32) << (2 * k);
    }
    out[8..10].copy_from_slice(&c0.to_le_bytes());
    out[10..12].copy_from_slice(&c1.to_le_bytes());
    out[12..16].copy_from_slice(&cbits.to_le_bytes());
    out
}

fn dxt5(image: &Image) -> Vec<u8> {
    let (w, h) = (image.width as usize, image.height as usize);
    let (bw, bh) = (w.div_ceil(4).max(1), h.div_ceil(4).max(1));
    let mut out = Vec::with_capacity(bw * bh * 16);
    for by in 0..bh {
        for bx in 0..bw {
            // texels past the edge repeat the last row and column
            let texels: [[f32; 4]; 16] = std::array::from_fn(|k| {
                let (x, y) = ((bx * 4 + k % 4).min(w - 1), (by * 4 + k / 4).min(h - 1));
                let at = (y * w + x) * 4;
                std::array::from_fn(|c| f32::from(image.rgba[at + c]))
            });
            out.extend_from_slice(&dxt5_block(&texels));
        }
    }
    out
}

/// The image as a BLP2 file.
pub fn encode(image: &Image, encoding: Encoding) -> Result<Vec<u8>, String> {
    let (w, h) = (image.width, image.height);
    if !w.is_power_of_two() || !h.is_power_of_two() || w.max(h) > MAX_SIDE {
        return Err(format!("{w}x{h}: sides must be powers of two, 1,024 at most"));
    }
    let encoding = match encoding {
        Encoding::Auto if colours(image, 256).is_some() => Encoding::Palette,
        Encoding::Auto => Encoding::Dxt5,
        other => other,
    };
    let levels = mipmaps(image);
    let (pal, mut datas, head) = match encoding {
        Encoding::Palette => {
            let (pal, datas) = palette(&levels);
            (pal, datas, [1u8, 8, 8, 1])
        }
        _ => (Vec::new(), levels.iter().map(dxt5).collect(), [2u8, 8, 7, 1]),
    };
    datas.truncate(16);
    let mut out = b"BLP2".to_vec();
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&head);
    out.extend_from_slice(&w.to_le_bytes());
    out.extend_from_slice(&h.to_le_bytes());
    let mut at = (out.len() + 128 + pal.len()) as u32;
    let mut offsets = [0u32; 16];
    let mut sizes = [0u32; 16];
    for (k, d) in datas.iter().enumerate() {
        offsets[k] = at;
        sizes[k] = d.len() as u32;
        at += d.len() as u32;
    }
    for v in offsets.iter().chain(&sizes) {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&pal);
    for d in &datas {
        out.extend_from_slice(d);
    }
    Ok(out)
}

// --- reading ------------------------------------------------------------------------------------

fn decode_block(block: &[u8], alpha: u8, out: &mut [[u8; 4]; 16]) {
    let colour = if alpha == 0 { block } else { &block[8..] };
    let (c0, c1) = (
        u16::from_le_bytes([colour[0], colour[1]]),
        u16::from_le_bytes([colour[2], colour[3]]),
    );
    let (e0, e1) = (from565(c0), from565(c1));
    let four = alpha != 0 || c0 > c1;
    let pal: [[f32; 4]; 4] = if four {
        [
            [e0[0], e0[1], e0[2], 255.0],
            [e1[0], e1[1], e1[2], 255.0],
            std::array::from_fn(|c| if c == 3 { 255.0 } else { (2.0 * e0[c] + e1[c]) / 3.0 }),
            std::array::from_fn(|c| if c == 3 { 255.0 } else { (e0[c] + 2.0 * e1[c]) / 3.0 }),
        ]
    } else {
        [
            [e0[0], e0[1], e0[2], 255.0],
            [e1[0], e1[1], e1[2], 255.0],
            std::array::from_fn(|c| if c == 3 { 255.0 } else { (e0[c] + e1[c]) / 2.0 }),
            [0.0; 4],
        ]
    };
    let bits = u32::from_le_bytes(colour[4..8].try_into().expect("four bytes"));
    for (k, texel) in out.iter_mut().enumerate() {
        let p = pal[((bits >> (2 * k)) & 3) as usize];
        *texel = p.map(|v| v.round() as u8);
    }
    match alpha {
        // DXT3: four bits a texel
        1 => {
            for (k, texel) in out.iter_mut().enumerate() {
                let nibble = (block[k / 2] >> (4 * (k % 2))) & 15;
                texel[3] = nibble * 17;
            }
        }
        // DXT5: two endpoints, three bits a texel
        2 => {
            let (a0, a1) = (f32::from(block[0]), f32::from(block[1]));
            let mut bytes = [0u8; 8];
            bytes[..6].copy_from_slice(&block[2..8]);
            let bits = u64::from_le_bytes(bytes);
            for (k, texel) in out.iter_mut().enumerate() {
                let i = ((bits >> (3 * k)) & 7) as usize;
                let v = match i {
                    0 => a0,
                    1 => a1,
                    _ if a0 > a1 => ((8 - i) as f32 * a0 + (i - 1) as f32 * a1) / 7.0,
                    6 => 0.0,
                    7 => 255.0,
                    _ => ((6 - i) as f32 * a0 + (i - 1) as f32 * a1) / 5.0,
                };
                texel[3] = v.round() as u8;
            }
        }
        _ => {}
    }
}

/// The first level of a BLP2 file.
pub fn decode(data: &[u8]) -> Result<Image, String> {
    if data.get(..4) != Some(b"BLP2".as_slice()) || data.len() < 148 {
        return Err("not BLP2".into());
    }
    let (enc, depth, atype) = (data[8], data[9], data[10]);
    let w = u32::from_le_bytes(data[12..16].try_into().expect("four bytes"));
    let h = u32::from_le_bytes(data[16..20].try_into().expect("four bytes"));
    let offset = u32::from_le_bytes(data[20..24].try_into().expect("four bytes")) as usize;
    let size = u32::from_le_bytes(data[84..88].try_into().expect("four bytes")) as usize;
    if w == 0 || h == 0 || w > 4096 || h > 4096 {
        return Err(format!("{w}x{h}"));
    }
    let level = data.get(offset..offset + size).ok_or("the first level past the end")?;
    let (wu, hu) = (w as usize, h as usize);
    let mut image = Image::new(w, h);
    match enc {
        1 => {
            let pal = data.get(148..148 + 1024).ok_or("no palette")?;
            for at in 0..wu * hu {
                let index = *level.get(at).ok_or("indices past the end")? as usize;
                let a = match depth {
                    8 => *level.get(wu * hu + at).ok_or("alphas past the end")?,
                    4 => (level.get(wu * hu + at / 2).ok_or("alphas past the end")? >> (4 * (at % 2)) & 15) * 17,
                    1 => {
                        if level.get(wu * hu + at / 8).ok_or("alphas past the end")? >> (at % 8) & 1 != 0 {
                            255
                        } else {
                            0
                        }
                    }
                    _ => 255,
                };
                image.rgba[at * 4..at * 4 + 4].copy_from_slice(&[
                    pal[index * 4 + 2],
                    pal[index * 4 + 1],
                    pal[index * 4],
                    a,
                ]);
            }
        }
        2 => {
            let (alpha, block_size) = match (depth, atype) {
                (0 | 1, _) => (0, 8),
                (_, 7) => (2, 16),
                _ => (1, 16),
            };
            let (bw, bh) = (wu.div_ceil(4), hu.div_ceil(4));
            let mut texels = [[0u8; 4]; 16];
            for by in 0..bh {
                for bx in 0..bw {
                    let at = (by * bw + bx) * block_size;
                    let block = level.get(at..at + block_size).ok_or("blocks past the end")?;
                    decode_block(block, alpha, &mut texels);
                    if depth == 0 {
                        texels.iter_mut().for_each(|t| t[3] = 255);
                    }
                    for (k, t) in texels.iter().enumerate() {
                        let (x, y) = (bx * 4 + k % 4, by * 4 + k / 4);
                        if x < wu && y < hu {
                            let p = (y * wu + x) * 4;
                            image.rgba[p..p + 4].copy_from_slice(t);
                        }
                    }
                }
            }
        }
        3 => {
            for at in 0..wu * hu {
                let t = level.get(at * 4..at * 4 + 4).ok_or("texels past the end")?;
                image.rgba[at * 4..at * 4 + 4].copy_from_slice(&[t[2], t[1], t[0], t[3]]);
            }
        }
        other => return Err(format!("colour encoding {other}")),
    }
    Ok(image)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::Value;
    use crate::texgen;

    #[test]
    fn a_shape_reads_back_as_it_was_drawn() {
        let image = texgen::make("glow", None, &Value::Map(Vec::new())).unwrap();
        let back = decode(&encode(&image, Encoding::Palette).unwrap()).unwrap();
        assert_eq!(back, image, "the palette is exact for a white shape");
        let back = decode(&encode(&image, Encoding::Dxt5).unwrap()).unwrap();
        let worst = image
            .rgba
            .iter()
            .zip(&back.rgba)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap();
        assert!(worst <= 8, "DXT5 within its precision: {worst}");
    }
}
