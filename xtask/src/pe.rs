use std::path::Path;

/// Number of exports of a 64-bit PE file (DLL), counting toward the Windows limit of 65,535.
pub fn exported_names(path: &Path) -> Result<u32, String> {
    exported_names_in(&std::fs::read(path).map_err(|e| e.to_string())?)
}

/// The larger of NumberOfFunctions and NumberOfNames of the export directory: exports by ordinal
/// only have no name but still count.
pub fn exported_names_in(data: &[u8]) -> Result<u32, String> {
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
            let functions = u32_at(offset + 20).ok_or_else(invalid)?;
            let names = u32_at(offset + 24).ok_or_else(invalid)?;
            return Ok(functions.max(names));
        }
    }
    Err("export directory not found".to_owned())
}

#[cfg(test)]
mod tests {
    use super::exported_names_in;

    /// A minimal 64-bit PE image with one section holding an export directory.
    fn image(functions: u32, names: u32, with_exports: bool) -> Vec<u8> {
        let mut data = vec![0u8; 0x600];
        let pe = 0x40usize;
        data[0x3c..0x40].copy_from_slice(&(pe as u32).to_le_bytes());
        data[pe..pe + 4].copy_from_slice(b"PE\0\0");
        data[pe + 6..pe + 8].copy_from_slice(&1u16.to_le_bytes());
        data[pe + 20..pe + 22].copy_from_slice(&0xF0u16.to_le_bytes());
        let optional = pe + 24;
        data[optional..optional + 2].copy_from_slice(&0x20bu16.to_le_bytes());
        if with_exports {
            data[optional + 112..optional + 116].copy_from_slice(&0x1000u32.to_le_bytes());
            data[optional + 116..optional + 120].copy_from_slice(&0x28u32.to_le_bytes());
        }
        let section = optional + 0xF0;
        data[section..section + 6].copy_from_slice(b".edata");
        data[section + 8..section + 12].copy_from_slice(&0x100u32.to_le_bytes());
        data[section + 12..section + 16].copy_from_slice(&0x1000u32.to_le_bytes());
        data[section + 16..section + 20].copy_from_slice(&0x200u32.to_le_bytes());
        data[section + 20..section + 24].copy_from_slice(&0x400u32.to_le_bytes());
        data[0x400 + 20..0x400 + 24].copy_from_slice(&functions.to_le_bytes());
        data[0x400 + 24..0x400 + 28].copy_from_slice(&names.to_le_bytes());
        data
    }

    #[test]
    fn exports_by_ordinal_only_are_counted() {
        assert_eq!(exported_names_in(&image(7, 5, true)), Ok(7));
    }

    #[test]
    fn the_larger_count_wins() {
        assert_eq!(exported_names_in(&image(3, 4, true)), Ok(4));
    }

    #[test]
    fn a_dll_without_exports_counts_zero() {
        assert_eq!(exported_names_in(&image(0, 0, false)), Ok(0));
    }

    #[test]
    fn a_file_that_is_not_a_pe_is_refused() {
        assert!(exported_names_in(b"not a dll").is_err());
    }

    #[cfg(windows)]
    #[test]
    fn kernel32_exports_over_a_thousand_symbols() {
        use super::exported_names;

        let windows = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_owned());
        let kernel32 = std::path::Path::new(&windows).join("System32").join("kernel32.dll");
        let count = exported_names(&kernel32).expect("kernel32.dll is a PE file");
        assert!(count > 1000, "{count}");
    }
}
