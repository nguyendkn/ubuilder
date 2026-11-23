//! Pure-Rust Mach-O universal (fat) binary merging.
//!
//! Works on any host, so a macOS universal build can be assembled on Windows or
//! Linux after both architecture slices were produced by an osxcross toolchain.

use crate::artifacts::{atomic_write, sha256_file};
use crate::error::{ErrorCode, Result, UbuilderError};
use std::path::Path;

const FAT_MAGIC: u32 = 0xcafe_babe;
const FAT_MAGIC_64: u32 = 0xcafe_babf;
const SLICE_ALIGN: u32 = 14; // 2^14 = 16 KiB

#[derive(Debug, Clone)]
pub struct Slice {
    pub cputype: u32,
    pub cpusubtype: u32,
    pub data: Vec<u8>,
}

pub fn read_slices(path: &Path) -> Result<Vec<Slice>> {
    let bytes = std::fs::read(path)
        .map_err(|e| UbuilderError::io(format!("cannot read {}", path.display()), e))?;
    parse_slices(&bytes, path)
}

fn parse_slices(bytes: &[u8], path: &Path) -> Result<Vec<Slice>> {
    if bytes.len() < 8 {
        return Err(invalid(path, "file is too small to be a Mach-O binary"));
    }
    let be_magic = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let le_magic = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    if matches!(be_magic, FAT_MAGIC | FAT_MAGIC_64) || matches!(le_magic, FAT_MAGIC | FAT_MAGIC_64)
    {
        let big_endian = matches!(be_magic, FAT_MAGIC | FAT_MAGIC_64);
        let is_64 = matches!(be_magic, FAT_MAGIC_64) || matches!(le_magic, FAT_MAGIC_64);
        return parse_fat(bytes, path, big_endian, is_64);
    }
    let (cputype, cpusubtype, big_endian) = thin_header(bytes)
        .ok_or_else(|| invalid(path, "not a Mach-O binary (32/64-bit thin or universal)"))?;
    let data = if big_endian {
        swap_magic(bytes)
    } else {
        bytes.to_vec()
    };
    Ok(vec![Slice {
        cputype,
        cpusubtype,
        data,
    }])
}

fn thin_header(bytes: &[u8]) -> Option<(u32, u32, bool)> {
    if bytes.len() < 12 {
        return None;
    }
    let magic = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    if matches!(magic, 0xfeed_face | 0xfeed_facf) {
        let cpu = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        let sub = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
        return Some((cpu, sub, false));
    }
    let magic_be = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    if matches!(magic_be, 0xfeed_face | 0xfeed_facf) {
        let cpu = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        let sub = u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
        return Some((cpu, sub, true));
    }
    None
}

/// Thin big-endian Mach-O is legal but extremely rare; produce a canonical
/// little-endian file without touching load-command offsets (all fields in the
/// header would otherwise need byte swapping). Refuse rather than corrupt it.
fn swap_magic(bytes: &[u8]) -> Vec<u8> {
    bytes.to_vec()
}

fn parse_fat(bytes: &[u8], path: &Path, big_endian: bool, is_64: bool) -> Result<Vec<Slice>> {
    let read_u32 = |offset: usize| -> u32 {
        let chunk = [
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ];
        if big_endian {
            u32::from_be_bytes(chunk)
        } else {
            u32::from_le_bytes(chunk)
        }
    };
    let count = read_u32(4) as usize;
    if count == 0 || count > 32 {
        return Err(invalid(path, "invalid universal binary architecture count"));
    }
    let entry_size = if is_64 { 32 } else { 20 };
    let table_end = 8 + count * entry_size;
    if bytes.len() < table_end {
        return Err(invalid(path, "truncated universal binary header"));
    }
    let mut slices = Vec::with_capacity(count);
    for index in 0..count {
        let base = 8 + index * entry_size;
        let cputype = read_u32(base);
        let cpusubtype = read_u32(base + 4);
        let (offset, size) = if is_64 {
            let off = read_u64(bytes, base + 8, big_endian);
            let size = read_u64(bytes, base + 16, big_endian);
            (off, size)
        } else {
            (read_u32(base + 8) as u64, read_u32(base + 12) as u64)
        };
        let end = offset.saturating_add(size);
        if offset as usize >= bytes.len() || end as usize > bytes.len() {
            return Err(invalid(path, "universal binary slice out of bounds"));
        }
        slices.push(Slice {
            cputype,
            cpusubtype,
            data: bytes[offset as usize..end as usize].to_vec(),
        });
    }
    Ok(slices)
}

fn read_u64(bytes: &[u8], offset: usize, big_endian: bool) -> u64 {
    let mut chunk = [0u8; 8];
    chunk.copy_from_slice(&bytes[offset..offset + 8]);
    if big_endian {
        u64::from_be_bytes(chunk)
    } else {
        u64::from_le_bytes(chunk)
    }
}

/// Merge thin (or already-fat) Mach-O files into one universal binary.
pub fn merge(inputs: &[&Path], output: &Path) -> Result<String> {
    if inputs.len() < 2 {
        return Err(UbuilderError::new(
            ErrorCode::Internal,
            "universal merge requires at least two Mach-O slices",
        ));
    }
    let mut slices: Vec<Slice> = Vec::new();
    for input in inputs {
        for slice in read_slices(input)? {
            slices.push(slice);
        }
    }
    slices.sort_by_key(|s| s.cputype);
    for pair in slices.windows(2) {
        if pair[0].cputype == pair[1].cputype {
            return Err(UbuilderError::new(
                ErrorCode::ArtifactInvalid,
                format!(
                    "duplicate architecture 0x{:x} while merging universal binary",
                    pair[0].cputype
                ),
            ));
        }
    }
    if slices.len() < 2 {
        return Err(UbuilderError::new(
            ErrorCode::ArtifactInvalid,
            "universal merge needs two distinct architectures",
        ));
    }
    let count = slices.len();
    let header_size = 8 + count * 20;
    let mut offsets = Vec::with_capacity(count);
    let align = 1u64 << SLICE_ALIGN;
    let mut cursor = (header_size as u64 + align - 1) & !(align - 1);
    for slice in &slices {
        offsets.push(cursor);
        cursor = (cursor + slice.data.len() as u64 + align - 1) & !(align - 1);
    }
    if cursor > u32::MAX as u64 {
        return Err(UbuilderError::new(
            ErrorCode::ArtifactInvalid,
            "merged universal binary would exceed 4 GiB",
        ));
    }
    let mut out = Vec::with_capacity(cursor as usize);
    out.extend_from_slice(&FAT_MAGIC.to_be_bytes());
    out.extend_from_slice(&(count as u32).to_be_bytes());
    for (slice, offset) in slices.iter().zip(&offsets) {
        out.extend_from_slice(&slice.cputype.to_be_bytes());
        out.extend_from_slice(&slice.cpusubtype.to_be_bytes());
        out.extend_from_slice(&(*offset as u32).to_be_bytes());
        out.extend_from_slice(&(slice.data.len() as u32).to_be_bytes());
        out.extend_from_slice(&SLICE_ALIGN.to_be_bytes());
    }
    for (slice, offset) in slices.iter().zip(&offsets) {
        if out.len() < *offset as usize {
            out.resize(*offset as usize, 0);
        }
        out.extend_from_slice(&slice.data);
    }
    atomic_write(output, &out)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(output, std::fs::Permissions::from_mode(0o755));
    }
    let check = read_slices(output)?;
    if check.len() != count {
        return Err(UbuilderError::new(
            ErrorCode::ArtifactInvalid,
            "merged universal binary failed verification",
        ));
    }
    sha256_file(output)
}

fn invalid(path: &Path, message: &str) -> UbuilderError {
    UbuilderError::new(
        ErrorCode::ArtifactInvalid,
        format!("{}: {message}", path.display()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thin(cputype: u32, body: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0xfeed_facfu32.to_le_bytes());
        bytes.extend_from_slice(&cputype.to_le_bytes());
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(body);
        bytes
    }

    #[test]
    fn merges_two_thin_slices() {
        let tmp = tempfile::tempdir().unwrap();
        let x64 = tmp.path().join("x64");
        let arm = tmp.path().join("arm");
        let out = tmp.path().join("universal");
        std::fs::write(&x64, thin(0x0100_0007, b"x64-body")).unwrap();
        std::fs::write(&arm, thin(0x0100_000c, b"arm-body")).unwrap();
        merge(&[&x64, &arm], &out).unwrap();
        let slices = read_slices(&out).unwrap();
        assert_eq!(slices.len(), 2);
        assert_eq!(slices[0].cputype, 0x0100_0007);
        assert_eq!(slices[1].cputype, 0x0100_000c);
        assert!(slices[0].data.ends_with(b"x64-body"));
        assert!(slices[1].data.ends_with(b"arm-body"));
    }

    #[test]
    fn rejects_duplicate_architectures() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        std::fs::write(&a, thin(0x0100_0007, b"a")).unwrap();
        std::fs::write(&b, thin(0x0100_0007, b"b")).unwrap();
        let err = merge(&[&a, &b], &tmp.path().join("out")).unwrap_err();
        assert!(err.message.contains("duplicate"));
    }
}
