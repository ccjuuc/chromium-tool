use std::path::Path;
use std::process::Command;
use anyhow::{Context, Result};
use crate::config::AppConfig;

#[cfg(target_os = "windows")]
mod os {
    pub const SHELL: [&str; 2] = ["cmd.exe", "/c"];
    pub const INSTALLER_PROJECT: &str = "installer_with_sign";
}

#[cfg(target_os = "macos")]
mod os {
    pub const SHELL: [&str; 2] = ["sh", "-c"];
    pub const INSTALLER_PROJECT: &str = "chrome/installer/mac";
}

#[cfg(target_os = "linux")]
mod os {
    pub const SHELL: [&str; 2] = ["sh", "-c"];
    pub const INSTALLER_PROJECT: &str = "chrome/installer/linux:stable";
}

#[derive(Clone)]
pub struct InstallerBuilder {
    #[allow(dead_code)]
    pub(crate) config: AppConfig,
}

impl InstallerBuilder {
    pub fn new(config: AppConfig) -> Self {
        Self { config }
    }
    
    /// 执行 ninja 命令（支持命令列表）
    async fn run_ninja(
        &self,
        src_path: &Path,
        out_dir: &str,
        targets: &[&str],
        step_name: &str,
    ) -> Result<()> {
        for (index, target) in targets.iter().enumerate() {
            let command = format!("ninja -C {} {}", out_dir, target);
            let step_label = if targets.len() > 1 {
                format!("{} ({}/{})", step_name, index + 1, targets.len())
            } else {
                step_name.to_string()
            };
            
            tracing::info!("═══════════════════════════════════════════════════════");
            tracing::info!("📋 执行命令: {}", command);
            tracing::info!("📁 工作目录: {}", src_path.display());
            tracing::info!("🏷️  步骤: {}", step_label);
            tracing::info!("═══════════════════════════════════════════════════════");
            
            let start_time = std::time::Instant::now();
            let output = Command::new(os::SHELL[0])
                .arg(os::SHELL[1])
                .arg(&command)
                .current_dir(src_path)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .context(format!("Failed to spawn ninja for target: {}", target))?
                .wait_with_output()
                .context(format!("Failed to wait for ninja: {}", target))?;
            
            let duration = start_time.elapsed();
            let exit_code = output.status.code().unwrap_or(-1);
            
            // 打印命令输出
            if !output.stdout.is_empty() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                tracing::info!("✅ 标准输出:\n{}", stdout);
            }
            
            if !output.stderr.is_empty() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                if output.status.success() {
                    tracing::warn!("⚠️  标准错误（警告）:\n{}", stderr);
                } else {
                    tracing::error!("❌ 标准错误:\n{}", stderr);
                }
            }
            
            tracing::info!("⏱️  执行时间: {:.2} 秒", duration.as_secs_f64());
            tracing::info!("🔢 退出码: {}", exit_code);
            
            if !output.status.success() {
                tracing::error!("❌ {} 执行失败", step_label);
                return Err(anyhow::anyhow!(
                    "{} failed with exit code {}: {}",
                    step_label,
                    exit_code,
                    String::from_utf8_lossy(&output.stderr)
                ));
            }
            
            tracing::debug!("{} 执行成功", step_label);
            if index < targets.len() - 1 {
                tracing::info!("⏭️  继续执行下一个目标...\n");
            } else {
                tracing::info!("═══════════════════════════════════════════════════════\n");
            }
        }
        
        Ok(())
    }
    
    pub async fn build_installer(&self, src_path: &Path, out_dir: &str) -> Result<()> {
        self.run_ninja(
            src_path,
            out_dir,
            &[os::INSTALLER_PROJECT],
            "installer build",
        ).await
    }
    
    /// 执行多个安装包构建目标（按顺序执行）
    #[allow(dead_code)] // 保留用于将来支持多个安装包目标的场景
    pub async fn build_installers(
        &self,
        src_path: &Path,
        out_dir: &str,
        targets: &[&str],
    ) -> Result<()> {
        self.run_ninja(src_path, out_dir, targets, "installer build").await
    }
}

