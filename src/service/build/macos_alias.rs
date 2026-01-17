use std::path::{Path, PathBuf};
use std::time::SystemTime;
use std::os::unix::fs::MetadataExt;
use std::io::{Cursor, Write};
use byteorder::{BigEndian, WriteBytesExt};
use anyhow::Result;

/// Apple Epoch is Jan 1 1904. Rust SystemTime is usually Unix Epoch (Jan 1 1970).
const APPLE_EPOCH_OFFSET: u64 = 2082844800; // Seconds between 1904 and 1970

#[derive(Debug)]
struct TargetInfo {
    id: u32,
    type_: String, // "file" or "directory"
    filename: String,
    created: SystemTime,
}

#[derive(Debug)]
struct ParentInfo {
    id: u32,
    name: String,
}

#[derive(Debug)]
struct VolumeInfo {
    name: String,
    created: SystemTime,
    signature: String, // "H+", "BD", "HX"
    type_: String, // "local", "other"
}

#[derive(Debug)]
struct ExtraItem {
    type_: u16,
    data: Vec<u8>,
}

#[derive(Debug)]
pub struct AliasInfo {
    version: u16,
    target: TargetInfo,
    parent: ParentInfo,
    volume: VolumeInfo,
    extra: Vec<ExtraItem>,
}

impl AliasInfo {
    pub fn new(path: &Path) -> Result<Self> {
        let path = path.canonicalize()?;
        let metadata = path.metadata()?;
        
        // 1. Target Info
        let target_info = TargetInfo {
            id: metadata.ino() as u32,
            type_: if metadata.is_dir() { "directory".to_string() } else { "file".to_string() },
            filename: path.file_name().unwrap_or_default().to_string_lossy().to_string(),
            created: metadata.created()?,
        };

        // 2. Parent Info
        let parent = path.parent().ok_or_else(|| anyhow::anyhow!("No parent"))?;
        let parent_metadata = parent.metadata()?;
        let parent_info = ParentInfo {
            id: parent_metadata.ino() as u32,
            name: parent.file_name().unwrap_or_default().to_string_lossy().to_string(),
        };

        // 3. Volume Info (Simplified logic)
        // Find volume root by traversing up until st_dev changes
        let (vol_path, vol_metadata) = find_volume(&path)?;
        
        // Volume Name is tricky without native calls. 
        // Hack: Use diskutil or assume directory name if not root.
        // For DMG creation, the volume name is the mounted DMG name.
        let vol_name = get_volume_name(&vol_path)?;
        
        let volume_info = VolumeInfo {
            name: vol_name,
            created: vol_metadata.created()?, // Approximate
            signature: "H+".to_string(), // HFS+ / APFS usually act like H+
            type_: if vol_path == Path::new("/") { "local".to_string() } else { "other".to_string() },
        };

        let mut extra = Vec::new();
        
        // Type 0: Parent Name
        extra.push(ExtraItem {
            type_: 0,
            data: parent_info.name.as_bytes().to_vec(),
        });
        
        // Type 1: Parent ID
        {
            let mut buf = vec![];
            buf.write_u32::<BigEndian>(parent_info.id)?;
            extra.push(ExtraItem { type_: 1, data: buf });
        }
        
        // Type 14: Unicode Filename
        {
            let mut buf = vec![];
            let u16_str = utf16be(&target_info.filename);
            buf.write_u16::<BigEndian>(target_info.filename.chars().count() as u16)?;
            buf.extend_from_slice(&u16_str);
            extra.push(ExtraItem { type_: 14, data: buf });
        }
        
        // Type 15: Unicode Volume Name
        {
            let mut buf = vec![];
            let u16_str = utf16be(&volume_info.name);
            buf.write_u16::<BigEndian>(volume_info.name.chars().count() as u16)?;
            buf.extend_from_slice(&u16_str);
            extra.push(ExtraItem { type_: 15, data: buf });
        }
        
        // Type 18: POSIX Path
        {
            let vol_path_str = vol_path.to_string_lossy();
            let path_str = path.to_string_lossy();
            if path_str.starts_with(vol_path_str.as_ref()) {
                let relative = &path_str[vol_path_str.len()..];
                extra.push(ExtraItem { type_: 18, data: relative.as_bytes().to_vec() });
            }
        }
        
        // Type 19: Volume Mount Point
        extra.push(ExtraItem {
            type_: 19,
            data: vol_path.to_string_lossy().as_bytes().to_vec(),
        });

        Ok(AliasInfo {
            version: 2,
            target: target_info,
            parent: parent_info,
            volume: volume_info,
            extra,
        })
    }
    
    pub fn encode(&self) -> Result<Vec<u8>> {
        let base_length = 150;
        let extra_length: usize = self.extra.iter().map(|e| {
            let padding = e.data.len() % 2;
            4 + e.data.len() + padding
        }).sum();
        let trailer_length = 4;
        let total_len = base_length + extra_length + trailer_length;
        
        let mut wtr = Cursor::new(vec![0u8; total_len]);
        
        // 0x00: User Type (0)
        wtr.write_u32::<BigEndian>(0)?;
        
        // 0x04: Alias Size
        wtr.write_u16::<BigEndian>(total_len as u16)?;
        
        // 0x06: Version (2)
        wtr.write_u16::<BigEndian>(self.version)?;
        
        // 0x08: Target Type (0=file, 1=dir)
        let type_val = if self.target.type_ == "directory" { 1 } else { 0 };
        wtr.write_u16::<BigEndian>(type_val)?;
        
        // 0x0A: Volume Name Len & Name (27 chars max)
        let vol_name_bytes = self.volume.name.as_bytes();
        let len = vol_name_bytes.len().min(27);
        wtr.write_u8(len as u8)?;
        // Write 27 bytes (padded with 0)
        let mut name_buf = [0u8; 27];
        name_buf[..len].copy_from_slice(&vol_name_bytes[..len]);
        wtr.write_all(&name_buf)?;
        
        // 0x26: Volume Create Date (Seconds since 1904)
        wtr.write_u32::<BigEndian>(to_apple_date(self.volume.created))?;
        
        // 0x2A: Signature ("H+")
        wtr.write_all(b"H+")?;
        
        // 0x2C: Volume Type (0=local, etc. Simplified to 0)
        wtr.write_u16::<BigEndian>(0)?;
        
        // 0x2E: Parent ID
        wtr.write_u32::<BigEndian>(self.parent.id)?;
        
        // 0x32: File Name Len & Name (63 chars max)
        let file_name_bytes = self.target.filename.as_bytes();
        let len = file_name_bytes.len().min(63);
        wtr.write_u8(len as u8)?;
        let mut name_buf = [0u8; 63];
        name_buf[..len].copy_from_slice(&file_name_bytes[..len]);
        wtr.write_all(&name_buf)?;
        
        // 0x72: Target ID (Inode)
        wtr.write_u32::<BigEndian>(self.target.id)?;
        
        // 0x76: Target Create Date
        wtr.write_u32::<BigEndian>(to_apple_date(self.target.created))?;
        
        // 0x7A: File Type (4) + Creator (4) = 8 bytes zeros
        wtr.write_all(&[0u8; 8])?;
        
        // 0x82: nlvlFrom (-1)
        wtr.write_i16::<BigEndian>(-1)?;
        // 0x84: nlvlTo (-1)
        wtr.write_i16::<BigEndian>(-1)?;
        
        // 0x86: Vol Attributes (0x00000D02)
        wtr.write_u32::<BigEndian>(0x00000D02)?;
        
        // 0x8A: Vol FS ID (0)
        wtr.write_u16::<BigEndian>(0)?;
        
        // 0x8C: Reserved (10 bytes zeros)
        wtr.write_all(&[0u8; 10])?;
        
        // 0x96: End of header, start of extra (150)
        assert_eq!(wtr.position(), 150);
        
        // Extra Data
        for extra in &self.extra {
            wtr.write_i16::<BigEndian>(extra.type_ as i16)?;
            wtr.write_u16::<BigEndian>(extra.data.len() as u16)?;
            wtr.write_all(&extra.data)?;
            
            // Pad if odd length
            if extra.data.len() % 2 == 1 {
                wtr.write_u8(0)?;
            }
        }
        
        // Trailer: -1 and 0
        wtr.write_i16::<BigEndian>(-1)?;
        wtr.write_u16::<BigEndian>(0)?;
        
        Ok(wtr.into_inner())
    }
}

fn to_apple_date(time: SystemTime) -> u32 {
    let unix_ts = time.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    (unix_ts + APPLE_EPOCH_OFFSET) as u32
}

fn utf16be(s: &str) -> Vec<u8> {
    let mut buf = Vec::with_capacity(s.len() * 2);
    for c in s.encode_utf16() {
        buf.push((c >> 8) as u8);
        buf.push((c & 0xFF) as u8);
    }
    buf
}

fn find_volume(start_path: &Path) -> Result<(PathBuf, std::fs::Metadata)> {
    let mut current = start_path.to_path_buf();
    let mut last_dev = current.metadata()?.dev();
    
    loop {
        let parent = match current.parent() {
            Some(p) => p.to_path_buf(),
            None => return Ok((current.clone(), current.metadata()?)),
        };
        
        let parent_meta = parent.metadata()?;
        if parent_meta.dev() != last_dev {
            // Dev changed, so current is the mount point
            return Ok((current.clone(), current.metadata()?));
        }
        
        last_dev = parent_meta.dev();
        current = parent;
    }
}

fn get_volume_name(vol_path: &Path) -> Result<String> {
    // try to get from user command line tools
    use std::process::Command;
    let output = Command::new("diskutil")
        .arg("info")
        .arg(vol_path)
        .output();
        
    if let Ok(out) = output {
        let stdout = String::from_utf8_lossy(&out.stdout);
        for line in stdout.lines() {
            if line.contains("Volume Name:") {
                return Ok(line.split(':').nth(1).unwrap_or("").trim().to_string());
            }
        }
    }
    
    // Fallback: basename
    Ok(vol_path.file_name().unwrap_or_default().to_string_lossy().to_string())
}
