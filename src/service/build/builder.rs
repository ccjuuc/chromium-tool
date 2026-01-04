use std::path::Path;
use std::process::Command;
use anyhow::{Context, Result};
use crate::config::AppConfig;
use crate::model::build::BuildRequest;

#[cfg(target_os = "windows")]
mod os {
    pub const SHELL: [&str; 2] = ["cmd.exe", "/c"];
    pub const IDE: &str = "vs2022";
}

#[cfg(target_os = "macos")]
mod os {
    pub const SHELL: [&str; 2] = ["sh", "-c"];
    pub const IDE: &str = "xcode";
}

#[cfg(target_os = "linux")]
mod os {
    pub const SHELL: [&str; 2] = ["sh", "-c"];
    pub const IDE: &str = "";
}

#[derive(Clone)]
pub struct ProjectBuilder {
    pub(crate) config: AppConfig,
}

impl ProjectBuilder {
    pub fn new(config: AppConfig) -> Self {
        Self { config }
    }
    
    pub async fn clean(
        &self,
        src_path: &Path,
        out_dir: &str,
        is_increment: bool,
    ) -> Result<()> {
        let dst_dir = src_path.join(out_dir);
        
        tracing::info!("🧹 清理模式: {}", if is_increment { "增量构建（保留输出目录）" } else { "完整构建（清理输出目录）" });
        
        if !is_increment && dst_dir.exists() {
            tracing::info!("🗑️  删除输出目录: {}", dst_dir.display());
            tokio::fs::remove_dir_all(&dst_dir).await?;
            tracing::info!("✅ 输出目录已删除");
        } else if is_increment {
            tracing::info!("⏭️  增量构建，保留输出目录: {}", dst_dir.display());
        } else {
            tracing::info!("ℹ️  输出目录不存在，无需删除: {}", dst_dir.display());
        }
        
        // 清理配置的路径
        if !self.config.clean.path.is_empty() {
            tracing::info!("🧹 清理配置路径:");
            for path in &self.config.clean.path {
                let clean_path = src_path.join(path);
                if clean_path.exists() {
                    if clean_path.is_file() {
                        tracing::info!("  🗑️  删除文件: {}", clean_path.display());
                        tokio::fs::remove_file(&clean_path).await?;
                    } else {
                        tracing::info!("  🗑️  删除目录: {}", clean_path.display());
                        tokio::fs::remove_dir_all(&clean_path).await?;
                    }
                } else {
                    tracing::info!("  ⏭️  路径不存在，跳过: {}", clean_path.display());
                }
            }
        } else {
            tracing::info!("ℹ️  无配置清理路径");
        }
        
        if !self.config.clean.out_path.is_empty() {
            tracing::info!("🧹 清理输出路径:");
            for path in &self.config.clean.out_path {
                let clean_path = src_path.join(out_dir).join(path);
                if clean_path.exists() {
                    if clean_path.is_file() {
                        tracing::info!("  🗑️  删除文件: {}", clean_path.display());
                        tokio::fs::remove_file(&clean_path).await?;
                    } else {
                        tracing::info!("  🗑️  删除目录: {}", clean_path.display());
                        tokio::fs::remove_dir_all(&clean_path).await?;
                    }
                } else {
                    tracing::info!("  ⏭️  路径不存在，跳过: {}", clean_path.display());
                }
            }
        } else {
            tracing::info!("ℹ️  无输出清理路径");
        }
        
        Ok(())
    }
    
    pub async fn generate(
        &self,
        src_path: &Path,
        out_dir: &str,
        request: &BuildRequest,
    ) -> Result<()> {
        let mut args = vec![];
        
        // 添加平台默认参数
        if let Ok(gn_args) = self.config.get_gn_default_args() {
            args.extend(gn_args.iter().cloned());
        }
        
        // 添加 target_cpu（根据架构）
        if let Some(arch) = request.architectures.first() {
            match arch.as_str() {
                "x64" => args.push("target_cpu=\\\"x64\\\"".to_string()),
                "x86" => args.push("target_cpu=\\\"x86\\\"".to_string()),
                "arm64" => args.push("target_cpu=\\\"arm64\\\"".to_string()),
                "arm" => args.push("target_cpu=\\\"arm\\\"".to_string()),
                _ => {
                    // 如果没有匹配的架构，根据 is_x64 推断
                    if request.is_x64 {
                        args.push("target_cpu=\\\"x64\\\"".to_string());
                    }
                }
            }
        } else if request.is_x64 {
            // 如果没有架构信息，使用 is_x64
            args.push("target_cpu=\\\"x64\\\"".to_string());
        }
        
        // 添加 OEM 参数
        if !request.oem_name.is_empty() {
            let oem = request.oem_name.split('=').nth(1).unwrap_or("normal");
            if oem != "snow" {
                let prefix = request.oem_name.split('=').nth(0).unwrap_or("current_xn_brand");
                args.push(format!("{}=\\\"{}\\\"", prefix, oem));
            }
        }
        
        // 添加自定义参数
        if let Some(custom_args) = &request.custom_args {
            args.extend(custom_args.iter().cloned());
        }
        
        // 执行 gn gen
        let ide_args = if os::IDE.is_empty() {
            "".to_string()
        } else {
            format!("--ide={}", os::IDE)
        };
        
        // 验证工作目录是否存在
        if !src_path.exists() {
            return Err(anyhow::anyhow!(
                "工作目录不存在: {}",
                src_path.display()
            ));
        }
        
        if !src_path.is_dir() {
            return Err(anyhow::anyhow!(
                "工作路径不是目录: {}",
                src_path.display()
            ));
        }
        
        let gn_args_str = args.join(" ");
        let gn_command = if os::IDE.is_empty() {
            format!("gn gen {} --args=\"{}\"", out_dir, gn_args_str)
        } else {
            format!("gn gen {} --args=\"{}\" {}", out_dir, gn_args_str, ide_args)
        };
        
        tracing::info!("执行命令: {} (参数: {})", gn_command, gn_args_str);
        
        let start_time = std::time::Instant::now();
        let output = Command::new(os::SHELL[0])
            .arg(os::SHELL[1])
            .arg(&gn_command)
            .current_dir(src_path)
            .output()
            .context("Failed to execute gn gen")?;
        
        let duration = start_time.elapsed();
        let exit_code = output.status.code().unwrap_or(-1);
        
        // 打印命令输出
        let stdout_str = if !output.stdout.is_empty() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            tracing::info!("✅ 标准输出:\n{}", stdout);
            Some(stdout.to_string())
        } else {
            None
        };
        
        let stderr_str = if !output.stderr.is_empty() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if output.status.success() {
                tracing::warn!("⚠️  标准错误（警告）:\n{}", stderr);
            } else {
                tracing::error!("❌ 标准错误:\n{}", stderr);
            }
            Some(stderr.to_string())
        } else {
            None
        };
        
        tracing::info!("⏱️  执行时间: {:.2} 秒", duration.as_secs_f64());
        tracing::info!("🔢 退出码: {}", exit_code);
        
        if !output.status.success() {
            tracing::error!("❌ gn gen 执行失败");
            
            // 构建详细的错误信息
            let error_msg = if let Some(stderr) = &stderr_str {
                if !stderr.trim().is_empty() {
                    stderr.clone()
                } else if let Some(stdout) = &stdout_str {
                    // 如果 stderr 为空，尝试从 stdout 提取错误信息
                    stdout.clone()
                } else {
                    format!("命令执行失败，但未捕获到错误输出。退出码: {}", exit_code)
                }
            } else if let Some(stdout) = &stdout_str {
                // stderr 为空，使用 stdout
                stdout.clone()
            } else {
                format!("命令执行失败，但未捕获到任何输出。退出码: {}", exit_code)
            };
            
            return Err(anyhow::anyhow!(
                "gn gen failed with exit code {}: {}\n执行命令: {}\n工作目录: {}",
                exit_code,
                error_msg,
                gn_command,
                src_path.display()
            ));
        }
        
        tracing::debug!("gn gen 执行成功");
        
        Ok(())
    }
}

