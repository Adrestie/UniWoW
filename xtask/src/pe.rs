use std::path::Path;

/// Number of named exports of a 64-bit PE file (DLL).
pub fn exported_names(path: &Path) -> Result<u32, String> {
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    let u16_at = |o: usize| data.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let u32_at = |o: usize| data.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let invalid = || "not a PE file".to_owned();

    let pe = u32_at(0x3c).ok_or_else(invalid)? as usize;
    let optional = pe + 24;
    let directories = match u16_at(optional).ok_or_else(invalid)? {
        0x20b => optional + 112,
        _ => optional + 96,
    };
    let export_rva = u32_at(directories).ok_or_else(invalid)?;
    if export_rva == 0 {
        return Ok(0);
    }
    let sections = u16_at(pe + 6).ok_or_else(invalid)? as usize;
    let optional_size = u16_at(pe + 20).ok_or_else(invalid)? as usize;
    for i in 0..sections {
        let header = optional + optional_size + i * 40;
        let virtual_size = u32_at(header + 8).ok_or_else(invalid)?;
        let virtual_address = u32_at(header + 12).ok_or_else(invalid)?;
        let raw_size = u32_at(header + 16).ok_or_else(invalid)?;
        let raw_pointer = u32_at(header + 20).ok_or_else(invalid)?;
        if (virtual_address..virtual_address + virtual_size.max(raw_size)).contains(&export_rva) {
            let offset = (export_rva - virtual_address + raw_pointer) as usize;
            return u32_at(offset + 24).ok_or_else(invalid);
        }
    }
    Err("export directory not found".to_owned())
}
