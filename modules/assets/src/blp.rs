//! BLP textures of version 2, those of 3.3.5a and the modern ones alike: their header, palette and
//! levels, and the decoding of DXT1, DXT3 and DXT5 into RGBA, translated from wow.export
//! (`src/js/casc/blp.js`, MIT, see THIRD_PARTY.md). A level cut short, as the last ones of a few
//! textures of the client, ends the levels kept. The older versions are refused, as Wow.exe 12340
//! does: it knows the mark of version 2 only.

use uniwow_api::formats::{Texture, TextureFormat};

/// The bytes of a level of each format, by block or by texel.
fn level_size(format: Encoding, width: usize, height: usize) -> usize {
    let blocks = width.div_ceil(4) * height.div_ceil(4);
    match format {
        Encoding::Palette { alpha_depth } => width * height + (width * height * usize::from(alpha_depth)).div_ceil(8),
        Encoding::Dxt1 => blocks * 8,
        Encoding::Dxt3 | Encoding::Dxt5 => blocks * 16,
        Encoding::Bgra => width * height * 4,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Encoding {
    Palette { alpha_depth: u8 },
    Dxt1,
    Dxt3,
    Dxt5,
    Bgra,
}

/// The texture in `bytes`: DXT kept as BC unless `decode`, the others always as RGBA.
pub fn texture(bytes: &[u8], decode: bool) -> Result<Texture, String> {
    let header = bytes.get(..148).ok_or("cut short in its header")?;
    match &header[..4] {
        b"BLP2" => {}
        b"BLP0" | b"BLP1" => {
            return Err(format!(
                "a {}, which the client of 3.3.5a does not read",
                String::from_utf8_lossy(&header[..4])
            ));
        }
        _ => return Err("not a BLP".to_owned()),
    }
    let word = |at: usize| u32::from_le_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]]);
    if word(4) != 1 {
        return Err(format!("a BLP of type {}, where 1 is read", word(4)));
    }
    let (encoding, alpha_depth, alpha_encoding) = (header[8], header[9], header[10]);
    let encoding = match encoding {
        1 => Encoding::Palette { alpha_depth },
        2 if alpha_depth <= 1 => Encoding::Dxt1,
        2 if alpha_encoding == 7 => Encoding::Dxt5,
        2 => Encoding::Dxt3,
        3 => Encoding::Bgra,
        other => return Err(format!("an encoding {other} unknown")),
    };
    let (width, height) = (word(12), word(16));
    if width == 0 || height == 0 || width > 8192 || height > 8192 {
        return Err(format!("a size of {width} × {height}"));
    }
    let palette = match encoding {
        Encoding::Palette { .. } => bytes.get(148..148 + 1024).ok_or("its palette cut short")?,
        _ => &[][..],
    };
    let mut levels = Vec::new();
    for index in 0..16 {
        let (offset, size) = (word(20 + index * 4) as usize, word(84 + index * 4) as usize);
        if offset == 0 {
            break;
        }
        let (w, h) = ((width as usize >> index).max(1), (height as usize >> index).max(1));
        let needed = level_size(encoding, w, h);
        let Some(data) = bytes
            .get(offset..offset.saturating_add(size))
            .filter(|_| size >= needed)
        else {
            break;
        };
        let data = &data[..needed];
        levels.push(match encoding {
            Encoding::Palette { alpha_depth } => from_palette(data, palette, w * h, alpha_depth),
            Encoding::Bgra => data
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| [p[2], p[1], p[0], p[3]])
                .collect(),
            _ if decode => decode_dxt(data, encoding, w, h),
            _ => data.to_vec(),
        });
        if w == 1 && h == 1 {
            break;
        }
    }
    if levels.is_empty() {
        return Err("no level".to_owned());
    }
    let format = match encoding {
        Encoding::Dxt1 if !decode => TextureFormat::Bc1,
        Encoding::Dxt3 if !decode => TextureFormat::Bc2,
        Encoding::Dxt5 if !decode => TextureFormat::Bc3,
        _ => TextureFormat::Rgba8,
    };
    Ok(Texture {
        width,
        height,
        format,
        levels,
    })
}

/// Texels of a palette, stored blue, green, red, then their alphas of `depth` bits after them.
fn from_palette(data: &[u8], palette: &[u8], texels: usize, depth: u8) -> Vec<u8> {
    let alpha = |index: usize| -> u8 {
        let alphas = &data[texels..];
        match depth {
            1 => {
                if alphas[index / 8] & (1 << (index % 8)) == 0 {
                    0
                } else {
                    255
                }
            }
            4 => {
                // Widened as the client's other 4-bit alphas are: 0xF is opaque.
                let byte = alphas[index / 2];
                if index.is_multiple_of(2) {
                    (byte & 0x0F) * 17
                } else {
                    (byte >> 4) * 17
                }
            }
            8 => alphas[index],
            _ => 255,
        }
    };
    (0..texels)
        .flat_map(|index| {
            let entry = usize::from(data[index]) * 4;
            [palette[entry + 2], palette[entry + 1], palette[entry], alpha(index)]
        })
        .collect()
}

/// A colour of 5, 6 and 5 bits, widened to a byte each, and the value it was packed in.
fn unpack(block: &[u8], at: usize) -> ([u8; 4], u16) {
    let value = u16::from_le_bytes([block[at], block[at + 1]]);
    let (r, g, b) = ((value >> 11) & 0x1F, (value >> 5) & 0x3F, value & 0x1F);
    let colour = [
        (r << 3 | r >> 2) as u8,
        (g << 2 | g >> 4) as u8,
        (b << 3 | b >> 2) as u8,
        255,
    ];
    (colour, value)
}

/// The blocks of DXT1, DXT3 or DXT5 in `data` as RGBA, `width` × `height`.
fn decode_dxt(data: &[u8], encoding: Encoding, width: usize, height: usize) -> Vec<u8> {
    let dxt1 = encoding == Encoding::Dxt1;
    let block_bytes = if dxt1 { 8 } else { 16 };
    let mut out = vec![0u8; width * height * 4];
    let mut blocks = data.chunks_exact(block_bytes);
    for y in (0..height).step_by(4) {
        for x in (0..width).step_by(4) {
            let Some(block) = blocks.next() else {
                return out;
            };
            let colour_at = if dxt1 { 0 } else { 8 };
            let (c0, a) = unpack(block, colour_at);
            let (c1, b) = unpack(block, colour_at + 2);
            let mut colours = [c0, c1, [0; 4], [0; 4]];
            let three = dxt1 && a <= b;
            for i in 0..3 {
                let (c, d) = (u16::from(c0[i]), u16::from(c1[i]));
                if three {
                    colours[2][i] = ((c + d) / 2) as u8;
                    colours[3][i] = 0;
                } else {
                    colours[2][i] = ((2 * c + d) / 3) as u8;
                    colours[3][i] = ((c + 2 * d) / 3) as u8;
                }
            }
            colours[2][3] = 255;
            colours[3][3] = if three { 0 } else { 255 };
            let mut texels = [[0u8; 4]; 16];
            for (i, texel) in texels.iter_mut().enumerate() {
                let index = (block[colour_at + 4 + i / 4] >> (2 * (i % 4))) & 0x3;
                *texel = colours[usize::from(index)];
            }
            match encoding {
                Encoding::Dxt3 => {
                    for (i, texel) in texels.iter_mut().enumerate() {
                        let nibble = (block[i / 2] >> (4 * (i % 2))) & 0x0F;
                        texel[3] = nibble | nibble << 4;
                    }
                }
                Encoding::Dxt5 => {
                    let (a0, a1) = (u16::from(block[0]), u16::from(block[1]));
                    let mut alphas = [a0 as u8, a1 as u8, 0, 0, 0, 0, 0, 0];
                    if a0 <= a1 {
                        for i in 1..5 {
                            alphas[i + 1] = (((5 - i as u16) * a0 + i as u16 * a1) / 5) as u8;
                        }
                        alphas[6] = 0;
                        alphas[7] = 255;
                    } else {
                        for i in 1..7 {
                            alphas[i + 1] = (((7 - i as u16) * a0 + i as u16 * a1) / 7) as u8;
                        }
                    }
                    let bits = u64::from_le_bytes([block[2], block[3], block[4], block[5], block[6], block[7], 0, 0]);
                    for (i, texel) in texels.iter_mut().enumerate() {
                        texel[3] = alphas[((bits >> (3 * i)) & 0x7) as usize];
                    }
                }
                _ => {}
            }
            for (i, texel) in texels.iter().enumerate() {
                let (tx, ty) = (x + i % 4, y + i / 4);
                if tx < width && ty < height {
                    let at = (ty * width + tx) * 4;
                    out[at..at + 4].copy_from_slice(texel);
                }
            }
        }
    }
    out
}
