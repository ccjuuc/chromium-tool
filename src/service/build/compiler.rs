use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio::process::Command;
use tokio::io::{AsyncBufReadExt, BufReader};
use anyhow::{Context, Result};
use crate::config::AppConfig;
use crate::repository::task::TaskRepository;
use crate::api::ws::WsManager;

#[cfg(target_os = "windows")]
mod os {
    pub const SHELL: [&str; 2] = ["cmd.exe", "/c"];
}

#[cfg(not(target_os = "windows"))]
mod os {
    pub const SHELL: [&str; 2] = ["sh", "-c"];
}

#[derive(Clone)]
pub struct Compiler {
    #[allow(dead_code)]
    pub(crate) config: AppConfig,
}

impl Compiler {
    pub fn new(config: AppConfig) -> Self {
        Self { config }
    }
    
    /// 执行 ninja 命令（支持命令列表，实时捕获输出）
    async fn run_ninja(
        &self,
        src_path: &Path,
        out_dir: &str,
        targets: &[&str],
        step_name: &str,
        task_id: Option<i64>,
        task_repo: Option<&TaskRepository>,
        ws_manager: Option<&WsManager>,
        cancelled_flag: Option<Arc<std::sync::atomic::AtomicBool>>,
    ) -> Result<()> {
        for (index, target) in targets.iter().enumerate() {
            let command = format!("ninja -C {} {}", out_dir, target);
            let step_label = if targets.len() > 1 {
                format!("{} ({}/{})", step_name, index + 1, targets.len())
            } else {
                step_name.to_string()
            };
            
            tracing::info!("执行命令: {} (步骤: {})", command, step_label);
            
            // 记录日志到数据库并广播到 WebSocket
            if let (Some(tid), Some(repo)) = (task_id, task_repo) {
                let log_line = format!("[{}] 开始执行: {}", step_label, command);
                let _ = repo.append_build_log(tid, &log_line).await;
                if let Some(ws) = ws_manager {
                    ws.broadcast_log(tid, log_line, false);
                }
            }
            
            let start_time = std::time::Instant::now();
            
            // 使用 tokio::process::Command 来实时捕获输出
            let mut child = Command::new(os::SHELL[0])
                .arg(os::SHELL[1])
                .arg(&command)
                .current_dir(src_path)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .context(format!("Failed to spawn ninja for target: {}", target))?;
            
            let mut stdout_lines = Vec::new();
            let mut stderr_lines = Vec::new();
            
            // 实时读取 stdout
            if let Some(stdout) = child.stdout.take() {
                let mut reader = BufReader::new(stdout).lines();
                loop {
                    // 检查取消标志
                    if let Some(flag) = &cancelled_flag {
                        if flag.load(Ordering::Relaxed) {
                            tracing::warn!("⚠️  任务已取消，正在终止 ninja 进程...");
                            eprintln!("⚠️  任务已取消，正在终止 ninja 进程...");
                            
                            // 获取进程 ID（在 kill 之前）
                            let pid = child.id();
                            
                            // 终止子进程及其子进程
                            if let Err(e) = child.kill().await {
                                tracing::warn!("Failed to kill ninja process: {}", e);
                                eprintln!("⚠️  终止 ninja 进程失败: {}", e);
                            } else {
                                tracing::info!("✅ ninja 进程已终止 (PID: {:?})", pid);
                                eprintln!("✅ ninja 进程已终止 (PID: {:?})", pid);
                            }
                            
                            // 尝试终止整个进程组（Unix 系统）
                            #[cfg(unix)]
                            {
                                if let Some(id) = pid {
                                    tracing::info!("🛑 尝试终止进程组 {}...", id);
                                    eprintln!("🛑 尝试终止进程组 {}...", id);
                                    
                                    // 使用 killpg 终止整个进程组
                                    let output = std::process::Command::new("kill")
                                        .arg("-TERM")
                                        .arg(&format!("-{}", id))
                                        .output();
                                    
                                    match output {
                                        Ok(output) if output.status.success() => {
                                            tracing::info!("✅ 进程组 {} 已终止", id);
                                            eprintln!("✅ 进程组 {} 已终止", id);
                                        },
                                        Ok(output) => {
                                            tracing::warn!("⚠️  终止进程组 {} 失败: {:?}", id, output.status);
                                            eprintln!("⚠️  终止进程组 {} 失败", id);
                                        },
                                        Err(e) => {
                                            tracing::warn!("⚠️  无法执行 kill 命令: {}", e);
                                            eprintln!("⚠️  无法执行 kill 命令: {}", e);
                                        }
                                    }
                                }
                            }
                            
                            return Err(anyhow::anyhow!("Task cancelled"));
                        }
                    }
                    
                    match reader.next_line().await {
                        Ok(Some(line)) => {
                            let line = line.trim_end().to_string();
                            if !line.is_empty() {
                                // 检测是否是进度行（格式：[数字/数字] 开头）
                                // 例如：[390/51744] CXX obj/... 或 [1660/37976] COPY ...
                                // 简化匹配：只要以 [数字/数字] 开头就认为是进度行
                                let is_progress = {
                                    let trimmed = line.trim_start();
                                    if trimmed.starts_with('[') {
                                        // 使用正则表达式匹配 [数字/数字] 模式
                                        use regex::Regex;
                                        static PROGRESS_PATTERN: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
                                        let pattern = PROGRESS_PATTERN.get_or_init(|| {
                                            Regex::new(r"^\[\d+/\d+\]").unwrap()
                                        });
                                        pattern.is_match(trimmed)
                                    } else {
                                        false
                                    }
                                };
                                
                                if !is_progress {
                                    // 非进度行：追加到列表并输出到日志
                                    stdout_lines.push(line.clone());
                                    tracing::info!("{}", line);
                                } else {
                                    // 进度行：在同一行刷新显示（使用 \r 回到行首，\x1b[2K 清除整行）
                                    use std::io::{self, Write};
                                    let _ = io::stderr().write_all(b"\x1b[2K\r"); // 清除当前行并回到行首
                                    let _ = io::stderr().write_all(line.as_bytes()); // 输出新内容
                                    let _ = io::stderr().flush(); // 立即刷新
                                }
                                
                                // 保存到数据库并广播到 WebSocket
                                if let (Some(tid), Some(repo)) = (task_id, task_repo) {
                                    if !is_progress {
                                        // 只有非进度行才保存到数据库（避免刷屏）
                                        let _ = repo.append_build_log(tid, &line).await;
                                    }
                                    // 所有行都通过 WebSocket 发送（包括进度行）
                                    if let Some(ws) = ws_manager {
                                        ws.broadcast_log(tid, line.clone(), is_progress);
                                    }
                                }
                            }
                        },
                        Ok(None) => break, // EOF
                        Err(_) => break,   // 读取错误
                    }
                }
            }
            
            // 实时读取 stderr
            if let Some(stderr) = child.stderr.take() {
                let mut reader = BufReader::new(stderr).lines();
                loop {
                    // 检查取消标志
                    if let Some(flag) = &cancelled_flag {
                        if flag.load(Ordering::Relaxed) {
                            tracing::warn!("⚠️  任务已取消，正在终止 ninja 进程...");
                            eprintln!("⚠️  任务已取消，正在终止 ninja 进程...");
                            
                            // 获取进程 ID（在 kill 之前）
                            let pid = child.id();
                            
                            // 终止子进程及其子进程
                            if let Err(e) = child.kill().await {
                                tracing::warn!("Failed to kill ninja process: {}", e);
                                eprintln!("⚠️  终止 ninja 进程失败: {}", e);
                            } else {
                                tracing::info!("✅ ninja 进程已终止 (PID: {:?})", pid);
                                eprintln!("✅ ninja 进程已终止 (PID: {:?})", pid);
                            }
                            
                            // 尝试终止整个进程组（Unix 系统）
                            #[cfg(unix)]
                            {
                                if let Some(id) = pid {
                                    tracing::info!("🛑 尝试终止进程组 {}...", id);
                                    eprintln!("🛑 尝试终止进程组 {}...", id);
                                    
                                    // 使用 killpg 终止整个进程组
                                    let output = std::process::Command::new("kill")
                                        .arg("-TERM")
                                        .arg(&format!("-{}", id))
                                        .output();
                                    
                                    match output {
                                        Ok(output) if output.status.success() => {
                                            tracing::info!("✅ 进程组 {} 已终止", id);
                                            eprintln!("✅ 进程组 {} 已终止", id);
                                        },
                                        Ok(output) => {
                                            tracing::warn!("⚠️  终止进程组 {} 失败: {:?}", id, output.status);
                                            eprintln!("⚠️  终止进程组 {} 失败", id);
                                        },
                                        Err(e) => {
                                            tracing::warn!("⚠️  无法执行 kill 命令: {}", e);
                                            eprintln!("⚠️  无法执行 kill 命令: {}", e);
                                        }
                                    }
                                }
                            }
                            
                            return Err(anyhow::anyhow!("Task cancelled"));
                        }
                    }
                    
                    match reader.next_line().await {
                        Ok(Some(line)) => {
                            let line = line.trim_end().to_string();
                            if !line.is_empty() {
                                stderr_lines.push(line.clone());
                                tracing::warn!("{}", line);
                                
                                // 保存到数据库并广播到 WebSocket
                                if let (Some(tid), Some(repo)) = (task_id, task_repo) {
                                    let log_line = format!("[WARN] {}", line);
                                    let _ = repo.append_build_log(tid, &log_line).await;
                                    if let Some(ws) = ws_manager {
                                        ws.broadcast_log(tid, log_line, false);  // stderr 不是进度行
                                    }
                                }
                            }
                        },
                        Ok(None) => break, // EOF
                        Err(_) => break,   // 读取错误
                    }
                }
            }
            
            // 再次检查取消标志（在等待进程完成前）
            if let Some(flag) = &cancelled_flag {
                if flag.load(Ordering::Relaxed) {
                    tracing::warn!("⚠️  任务已取消，正在终止 ninja 进程...");
                    eprintln!("⚠️  任务已取消，正在终止 ninja 进程...");
                    
                    // 获取进程 ID（在 kill 之前）
                    let pid = child.id();
                    
                    // 终止子进程及其子进程
                    if let Err(e) = child.kill().await {
                        tracing::warn!("Failed to kill ninja process: {}", e);
                        eprintln!("⚠️  终止 ninja 进程失败: {}", e);
                    } else {
                        tracing::info!("✅ ninja 进程已终止 (PID: {:?})", pid);
                        eprintln!("✅ ninja 进程已终止 (PID: {:?})", pid);
                    }
                    
                    // 尝试终止整个进程组（Unix 系统）
                    #[cfg(unix)]
                    {
                        if let Some(id) = pid {
                            tracing::info!("🛑 尝试终止进程组 {}...", id);
                            eprintln!("🛑 尝试终止进程组 {}...", id);
                            
                            // 使用 killpg 终止整个进程组
                            let output = std::process::Command::new("kill")
                                .arg("-TERM")
                                .arg(&format!("-{}", id))
                                .output();
                            
                            match output {
                                Ok(output) if output.status.success() => {
                                    tracing::info!("✅ 进程组 {} 已终止", id);
                                    eprintln!("✅ 进程组 {} 已终止", id);
                                },
                                Ok(output) => {
                                    tracing::warn!("⚠️  终止进程组 {} 失败: {:?}", id, output.status);
                                    eprintln!("⚠️  终止进程组 {} 失败", id);
                                },
                                Err(e) => {
                                    tracing::warn!("⚠️  无法执行 kill 命令: {}", e);
                                    eprintln!("⚠️  无法执行 kill 命令: {}", e);
                                }
                            }
                        }
                    }
                    
                    return Err(anyhow::anyhow!("Task cancelled"));
                }
            }
            
            // 等待进程完成
            let status = child.wait().await
                .context(format!("Failed to wait for ninja: {}", target))?;
            
            let duration = start_time.elapsed();
            let exit_code = status.code().unwrap_or(-1);
            
            tracing::info!("⏱️  执行时间: {:.2} 秒", duration.as_secs_f64());
            tracing::info!("🔢 退出码: {}", exit_code);
            
            if !status.success() {
                let stderr_str = stderr_lines.join("\n");
                // 检查是否是 "unknown target" 错误，如果是则跳过（某些平台可能没有某些目标）
                if stderr_str.contains("unknown target") {
                    tracing::warn!("⚠️  目标 '{}' 不存在，跳过此步骤", target);
                    if let (Some(tid), Some(repo)) = (task_id, task_repo) {
                        let log_line = format!("[{}] 已跳过（目标不存在）", step_label);
                        let _ = repo.append_build_log(tid, &log_line).await;
                        if let Some(ws) = ws_manager {
                            ws.broadcast_log(tid, log_line, false);
                        }
                    }
                    tracing::info!("✅ {} 已跳过（目标不存在）", step_label);
                    continue;  // 跳过这个目标，继续下一个
                }
                
                tracing::error!("❌ {} 执行失败", step_label);
                if let (Some(tid), Some(repo)) = (task_id, task_repo) {
                    let log_line = format!("[{}] 执行失败，退出码: {}", step_label, exit_code);
                    let _ = repo.append_build_log(tid, &log_line).await;
                    if let Some(ws) = ws_manager {
                        ws.broadcast_log(tid, log_line, false);
                    }
                }
                return Err(anyhow::anyhow!(
                    "{} failed with exit code {}: {}",
                    step_label,
                    exit_code,
                    stderr_str
                ));
            }
            
            tracing::debug!("{} 执行成功", step_label);
        }
        
        Ok(())
    }
    
    #[allow(dead_code)]
    pub async fn build_pre_build(
        &self,
        src_path: &Path,
        out_dir: &str,
        task_id: Option<i64>,
        task_repo: Option<&TaskRepository>,
        ws_manager: Option<&WsManager>,
    ) -> Result<()> {
        // 直接尝试构建，如果目标不存在会自动跳过（在 run_ninja 中处理）
        self.run_ninja(src_path, out_dir, &["pre_build"], "pre_build", task_id, task_repo, ws_manager, None).await
    }
    
    #[allow(dead_code)]
    pub async fn build_base(
        &self,
        src_path: &Path,
        out_dir: &str,
        task_id: Option<i64>,
        task_repo: Option<&TaskRepository>,
        ws_manager: Option<&WsManager>,
    ) -> Result<()> {
        if cfg!(target_os = "macos") {
            tracing::info!("ℹ️  macOS 平台跳过 build_base 步骤");
            return Ok(());  // macOS 不需要 build base
        }
        
        self.run_ninja(src_path, out_dir, &["base"], "base build", task_id, task_repo, ws_manager, None).await
    }
    
    #[allow(dead_code)]
    pub async fn build_chrome(
        &self,
        src_path: &Path,
        out_dir: &str,
        task_id: Option<i64>,
        task_repo: Option<&TaskRepository>,
        ws_manager: Option<&WsManager>,
    ) -> Result<()> {
        self.run_ninja(src_path, out_dir, &["chrome"], "chrome build", task_id, task_repo, ws_manager, None).await
    }
    
    /// 执行多个 ninja 目标（按顺序执行）
    #[allow(dead_code)] // 保留用于将来支持多个目标的场景
    pub async fn build_targets(
        &self,
        src_path: &Path,
        out_dir: &str,
        targets: &[&str],
        step_name: &str,
        task_id: Option<i64>,
        task_repo: Option<&TaskRepository>,
        ws_manager: Option<&WsManager>,
        cancelled_flag: Option<Arc<std::sync::atomic::AtomicBool>>,
    ) -> Result<()> {
        self.run_ninja(src_path, out_dir, targets, step_name, task_id, task_repo, ws_manager, cancelled_flag).await
    }
}

  