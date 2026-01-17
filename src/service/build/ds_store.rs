use std::io::{Write};
use byteorder::{BigEndian, WriteBytesExt};
use serde::{Serialize};
use anyhow::{Result, Context};
use base64::Engine;

use crate::service::build::ds_store_template::DS_STORE_CLEAN_B64;

#[derive(Debug)]
pub struct Entry {
    filename: String,
    structure_id: String, // "Iloc", "bwsp", "icvp"
    data_type: String, // "blob"
    blob: Vec<u8>,
}

impl Entry {
    pub fn new_iloc(filename: &str, x: u32, y: u32) -> Self {
        let mut blob = Vec::new();
        // blob header: length 16
        
        let content_len = 16u32;
        blob = Vec::with_capacity((4 + content_len) as usize);
        blob.write_u32::<BigEndian>(content_len).unwrap();
        blob.write_u32::<BigEndian>(x).unwrap();
        blob.write_u32::<BigEndian>(y).unwrap();
        blob.write_u32::<BigEndian>(0xffffffff).unwrap();
        blob.write_u32::<BigEndian>(0).unwrap();
        
        Entry {
            filename: filename.to_string(),
            structure_id: "Iloc".to_string(),
            data_type: "blob".to_string(),
            blob,
        }
    }

    pub fn new_bwsp(width: u32, height: u32) -> Result<Self> {
        let x = 335; // Center-ish
        let y = 184;

        let bounds_str = format!("{{{{{}, {}}}, {{{}, {}}}}}", x, y, width, height);
        
        #[derive(Serialize)]
        struct BwspPlist {
            #[serde(rename = "ContainerShowSidebar")]
            container_show_sidebar: bool,
            #[serde(rename = "ShowPathbar")]
            show_pathbar: bool,
            #[serde(rename = "ShowSidebar")]
            show_sidebar: bool,
            #[serde(rename = "ShowStatusBar")]
            show_status_bar: bool,
            #[serde(rename = "ShowTabView")]
            show_tab_view: bool,
            #[serde(rename = "ShowToolbar")]
            show_toolbar: bool,
            #[serde(rename = "SidebarWidth")]
            sidebar_width: u32,
            #[serde(rename = "WindowBounds")]
            window_bounds: String,
        }
        
        let plist_obj = BwspPlist {
            container_show_sidebar: false,
            show_pathbar: false,
            show_sidebar: true,
            show_status_bar: false,
            show_tab_view: false,
            show_toolbar: false,
            sidebar_width: 0,
            window_bounds: bounds_str,
        };
        
        let mut plist_buf = Vec::new();
        plist::to_writer_binary(&mut plist_buf, &plist_obj)?;
        
        // entry.js: dataType = 'bplist' converted to 'blob' with length prefix
        let mut blob = Vec::new();
        blob.write_u32::<BigEndian>(plist_buf.len() as u32).unwrap();
        blob.write_all(&plist_buf).unwrap();

        Ok(Entry {
            filename: ".".to_string(),
            structure_id: "bwsp".to_string(),
            data_type: "blob".to_string(),
            blob,
        })
    }

    pub fn new_icvp(icon_size: f64, bg_alias: Option<Vec<u8>>) -> Result<Self> {
        #[derive(Serialize)]
        struct IcvpPlist {
            #[serde(rename = "viewOptionsVersion")]
            version: u32,
            #[serde(rename = "backgroundType")]
            bg_type: u32, 
            #[serde(rename = "iconSize")]
            icon_size: f64,
            #[serde(rename = "gridSpacing")]
            grid_spacing: f64,
            #[serde(rename = "backgroundColorRed")]
            bg_red: f64,
            #[serde(rename = "backgroundColorGreen")]
            bg_green: f64,
            #[serde(rename = "backgroundColorBlue")]
            bg_blue: f64,
            #[serde(rename = "showIconPreview")]
            show_icon_preview: bool,
            #[serde(rename = "showItemInfo")]
            show_item_info: bool,
            #[serde(rename = "textSize")]
            text_size: f64,
            #[serde(rename = "labelOnBottom")]
            label_on_bottom: bool,
            #[serde(rename = "arrangeBy")]
            arrange_by: String,
            #[serde(rename = "gridOffsetX")]
            grid_offset_x: f64,
            #[serde(rename = "gridOffsetY")]
            grid_offset_y: f64,
            #[serde(rename = "backgroundImageAlias", skip_serializing_if = "Option::is_none")]
            bg_alias: Option<serde_bytes::ByteBuf>,
        }
        
        let mut bg_type = 1;
        let mut alias_buf = None;
        
        if let Some(alias) = bg_alias {
            bg_type = 2;
            alias_buf = Some(serde_bytes::ByteBuf::from(alias));
        }
        
        let plist_obj = IcvpPlist {
            version: 1,
            bg_type,
            icon_size,
            grid_spacing: 100.0,
            bg_red: 1.0,
            bg_green: 1.0, 
            bg_blue: 1.0,
            show_icon_preview: true,
            show_item_info: false,
            text_size: 12.0,
            label_on_bottom: true,
            arrange_by: "none".to_string(),
            grid_offset_x: 0.0,
            grid_offset_y: 0.0,
            bg_alias: alias_buf,
        };
        
        let mut plist_buf = Vec::new();
        plist::to_writer_binary(&mut plist_buf, &plist_obj)?;
        
        let mut blob = Vec::new();
        blob.write_u32::<BigEndian>(plist_buf.len() as u32).unwrap();
        blob.write_all(&plist_buf).unwrap();

        Ok(Entry {
            filename: ".".to_string(),
            structure_id: "icvp".to_string(),
            data_type: "blob".to_string(),
            blob,
        })
    }
    
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        // Filename is converted to Utf16BE
        let name_utf16: Vec<u16> = self.filename.encode_utf16().collect();
        let name_len = name_utf16.len() as u32; // character count
        
        buf.write_u32::<BigEndian>(name_len).unwrap();
        for c in name_utf16 {
            buf.write_u16::<BigEndian>(c).unwrap();
        }
        
        buf.write_all(self.structure_id.as_bytes()).unwrap();
        buf.write_all(self.data_type.as_bytes()).unwrap();
        
        buf.write_all(&self.blob).unwrap();
        buf
    }
}

pub async fn write_ds_store(path: &std::path::Path, entries: Vec<Entry>) -> Result<()> {
    // 1. Decode clean template
    let mut store_data = base64::engine::general_purpose::STANDARD.decode(DS_STORE_CLEAN_B64)
        .context("Failed to decode DSStore template")?;
        
    // 2. We need to overwrite from offset 4100 (0x1004)
    // ds-store.js: modified.copy(buf, 4100)
    // The clean template is approx 6KB.
    
    // Sort entries? Node lib sorts by filename then structureId.
    // Let's assume order doesn't crash finder for now, or sort properly.
    let mut sorted_entries = entries;
    sorted_entries.sort_by(|a, b| {
        // Naive sort: filename, then id
        a.filename.cmp(&b.filename).then(a.structure_id.cmp(&b.structure_id))
    });
    
    // Construct the "modified" buffer (which holds the record tree block)
    // ds-store.js: var modified = new Buffer(3840)
    let mut modified = vec![0u8; 3840];
    let mut current_pos = 0;
    
    // Write header: P=0, count
    // ds-store.js: modified.writeUInt32BE(P, 0); modified.writeUInt32BE(count, 4)
    let mut cursor = std::io::Cursor::new(&mut modified);
    cursor.write_u32::<BigEndian>(0)?;
    cursor.write_u32::<BigEndian>(sorted_entries.len() as u32)?;
    current_pos += 8;
    
    for entry in &sorted_entries {
        let b = entry.to_bytes();
        cursor.write_all(&b)?;
        current_pos += b.len();
    }
    
    // Write data to store_data
    // Note: Node's ds-store implementation does NOT write count to file offset 76.
    // It writes count to the ROOT block's structure (which is implemented in the 'modified' buffer).
    // So we should NOT modify store_data header directly.
    
    // Overwrite at 4100
    // store_data is typically 6148 bytes.
    // We copy 'modified' (3840 bytes) into 4100.
    // 4100 + 3840 = 7940. We might need to extend store_data.
    let end_pos = 4100 + modified.len();
    if store_data.len() < end_pos {
        store_data.resize(end_pos, 0);
    }
    
    // Copy modified buffer
    store_data[4100..end_pos].copy_from_slice(&modified);
    
    tokio::fs::write(path, store_data).await?;
    
    Ok(())
}
