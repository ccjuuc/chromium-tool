use std::path::Path;
use std::fs;
use std::io::BufRead;
use anyhow::{Context, Result};
use crate::model::id_finder::FileCategories;

pub struct IdFinder;

impl IdFinder {
    /// 生成 message_id（基于消息内容和可选的 meaning）
    pub fn generate_message_id(message: &str, meaning: Option<&str>) -> String {
        let mut fp = Self::fingerprint(message);
        if let Some(meaning) = meaning {
            let fp2 = Self::fingerprint(meaning);
            if fp < 0 {
                fp = fp2 + (fp << 1) + 1;
            } else {
                fp = fp2 + (fp << 1);
            }
        }
        (fp & 0x7fffffffffffffff).to_string()
    }

    /// 计算指纹
    fn fingerprint(input: &str) -> i64 {
        let digest = md5::compute(input);
        let hex128 = format!("{:x}", digest);
        let int64 = u64::from_str_radix(&hex128[..16], 16)
            .unwrap_or(0);
        
        // 检查最高位（符号位）
        if int64 & 0x8000000000000000u64 != 0 {
            // 如果是负数，转换为有符号 i64
            -((!int64 + 1) as i64)
        } else {
            int64 as i64
        }
    }

    /// 从行中提取 ID
    pub fn extract_id(line: &str) -> Option<&str> {
        let id_start = line.find("id=\"")?;
        let id_start = id_start + 4; // Skip past 'id="'
        let id_end = line[id_start..].find('"')? + id_start;
        Some(&line[id_start..id_end])
    }

    /// 从行中提取消息
    pub fn extract_message(line: &str) -> Option<&str> {
        let message_start = line.find('>')?;
        let message_start = message_start + 1;
        let message_end = line[message_start..].find('<')? + message_start;
        Some(&line[message_start..message_end])
    }

    /// 遍历目录查找文件
    pub fn visit_dirs(
        dir: &Path,
        zh_cn_files: &mut Vec<String>,
        en_us_files: &mut Vec<String>,
        en_gb_files: &mut Vec<String>,
        grd_files: &mut Vec<String>,
        grdp_files: &mut Vec<String>,
    ) -> Result<()> {
        if dir.is_dir() {
            for entry in fs::read_dir(dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.is_dir() {
                    Self::visit_dirs(&path, zh_cn_files, en_us_files, en_gb_files, grd_files, grdp_files)?;
                } else {
                    if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                        match file_name {
                            name if name.ends_with("zh-CN.xtb") => {
                                if let Some(path_str) = path.to_str() {
                                    zh_cn_files.push(path_str.to_string());
                                }
                            },
                            name if name.ends_with("en-US.xtb") => {
                                if let Some(path_str) = path.to_str() {
                                    en_us_files.push(path_str.to_string());
                                }
                            },
                            name if name.ends_with("en-GB.xtb") => {
                                if let Some(path_str) = path.to_str() {
                                    en_gb_files.push(path_str.to_string());
                                }
                            },
                            name if name.ends_with(".grd") => {
                                if let Some(path_str) = path.to_str() {
                                    grd_files.push(path_str.to_string());
                                }
                            },
                            name if name.ends_with(".grdp") => {
                                if let Some(path_str) = path.to_str() {
                                    grdp_files.push(path_str.to_string());
                                }
                            },
                            _ => (),
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// 获取或创建文件分类
    pub fn get_file_categories(src_path: &str) -> Result<FileCategories> {
        let src_path = Path::new(src_path);
        let categories_file = src_path.join("find-id-data.json");

        if categories_file.exists() {
            let file = fs::File::open(&categories_file)
                .context("Failed to open categories file")?;
            let categories: FileCategories = serde_json::from_reader(file)
                .context("Failed to parse categories file")?;
            Ok(categories)
        } else {
            let mut zh_cn_files = Vec::new();
            let mut en_us_files = Vec::new();
            let mut en_gb_files = Vec::new();
            let mut grd_files = Vec::new();
            let mut grdp_files = Vec::new();

            Self::visit_dirs(
                src_path,
                &mut zh_cn_files,
                &mut en_us_files,
                &mut en_gb_files,
                &mut grd_files,
                &mut grdp_files,
            )?;

            let categories = FileCategories {
                zh_cn_files,
                en_us_files,
                en_gb_files,
                grd_files,
                grdp_files,
            };

            // 保存到文件
            let file = fs::File::create(&categories_file)
                .context("Failed to create categories file")?;
            serde_json::to_writer(file, &categories)
                .context("Failed to write categories file")?;

            Ok(categories)
        }
    }

    /// 搜索 ID
    pub fn search_ids(search_text: &str, src_path: &str) -> Result<(Vec<String>, Vec<String>, Vec<String>)> {
        let categories = Self::get_file_categories(src_path)?;
        let mut ids = Vec::new();
        let mut messages = Vec::new();

        // 在 zh-CN 文件中搜索
        for file in &categories.zh_cn_files {
            let file_path = Path::new(file);
            if !file_path.exists() {
                continue;
            }
            let file = fs::File::open(file_path)
                .context(format!("Failed to open file: {}", file))?;
            let reader = std::io::BufReader::new(file);
            for line in reader.lines() {
                let line = line.context("Failed to read line")?;
                if line.contains(search_text) {
                    if let Some(id) = Self::extract_id(&line) {
                        ids.push(id.to_string());
                    }
                }
            }
        }

        // 在 en-US/en-GB 文件中查找对应的翻译
        let mut combined_files = categories.en_us_files.clone();
        combined_files.extend(categories.en_gb_files);
        for file in combined_files {
            let file_path = Path::new(&file);
            if !file_path.exists() {
                continue;
            }
            let content = fs::read_to_string(file_path)
                .context(format!("Failed to read file: {}", file))?;
            let translations = content.split("<translation");
            let filtered_items: Vec<_> = translations.filter(|item| {
                ids.iter().any(|id| item.contains(id))
            }).collect();

            for item in filtered_items {
                if let Some(message) = Self::extract_message(item) {
                    messages.push(message.to_string());
                }
            }
        }

        // 在 .grd/.grdp 文件中查找对应的消息定义
        let mut grd_matches = Vec::new();
        let mut combined_grd_files = categories.grd_files.clone();
        combined_grd_files.extend(categories.grdp_files);
        for file in combined_grd_files {
            let file_path = Path::new(&file);
            if !file_path.exists() {
                continue;
            }
            let content = fs::read_to_string(file_path)
                .context(format!("Failed to read file: {}", file))?;
            let translations = content.split("<message");
            let filtered_items: Vec<_> = translations.filter(|item| {
                messages.iter().any(|message| item.contains(message))
            }).collect();

            for item in filtered_items {
                grd_matches.push(item.to_string());
            }
        }

        Ok((ids, messages, grd_matches))
    }
}

