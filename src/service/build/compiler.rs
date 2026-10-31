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
    
    /// 终止子进程及其所属的整个进程组
    async fn terminate_child(child: &mut tokio::process::Child) {
        tracing::warn!("⚠️  任务已取消，正在终止 ninja 进程...");
        let pid = child.id();

        #[cfg(unix)]
        {
            if let Some(id) = pid {
                tracing::info!("🛑 尝试终止进程组 {}...", id);
                let _ = std::process::Command::new("kill")
                    .arg("-TERM")
                    .arg(format!("-{}", id))
                    .output();
            }
        }

        if let Err(e) = child.kill().await {
            tracing::warn!("Failed to kill ninja process: {}", e);
        } else {
            tracing::info!("✅ ninja 进程已终止 (PID: {:?})", pid);
        }
    }

    /// 执行 ninja 命令（支持命令列表，实时并发捕获 stdout / stderr 输出）
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
            
            // 使用 tokio::process::Command 实时捕获输出，并启用 kill_on_drop 和独立进程组
            let mut cmd = Command::new(os::SHELL[0]);
            cmd.arg(os::SHELL[1])
                .arg(&command)
                .current_dir(src_path)
                .kill_on_drop(true)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());

            #[cfg(unix)]
            {
                cmd.process_group(0);
            }

            let mut child = cmd
                .spawn()
                .context(format!("Failed to spawn ninja for target: {}", target))?;
            
            let mut stdout_lines = Vec::new();
            let mut stderr_lines = Vec::new();

            let stdout = child.stdout.take().context("Failed to capture ninja stdout")?;
            let stderr = child.stderr.take().context("Failed to capture ninja stderr")?;
            let mut stdout_reader = BufReader::new(stdout).lines();
            let mut stderr_reader = BufReader::new(stderr).lines();
            let mut stdout_done = false;
            let mut stderr_done = false;

            // 并发读取 stdout 与 stderr，防止任一管道缓冲区写满导致死锁，同时每 50ms 轮询取消标志
            while !stdout_done || !stderr_done {
                if let Some(flag) = &cancelled_flag {
                    if flag.load(Ordering::SeqCst) {
                        Self::terminate_child(&mut child).await;
                        return Err(anyhow::anyhow!("Task cancelled"));
                    }
                }

                tokio::select! {
                    res = stdout_reader.next_line(), if !stdout_done => {
                        match res {
                            Ok(Some(line)) => {
                                let line = line.trim_end().to_string();
                                if !line.is_empty() {
                                    let is_progress = {
                                        let trimmed = line.trim_start();
                                        if trimmed.starts_with('[') {
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
                                        stdout_lines.push(line.clone());
                                        tracing::info!("{}", line);
                                    } else {
                                        use std::io::{self, Write};
                                        let _ = io::stderr().write_all(b"\x1b[2K\r");
                                        let _ = io::stderr().write_all(line.as_bytes());
                                        let _ = io::stderr().flush();
                                    }

                                    if let (Some(tid), Some(repo)) = (task_id, task_repo) {
                                        if !is_progress {
                                            let _ = repo.append_build_log(tid, &line).await;
                                        }
                                        if let Some(ws) = ws_manager {
                                            ws.broadcast_log(tid, line.clone(), is_progress);
                                        }
                                    }
                                }
                            }
                            _ => {
                                stdout_done = true;
                            }
                        }
                    }
                    res = stderr_reader.next_line(), if !stderr_done => {
                        match res {
                            Ok(Some(line)) => {
                                let line = line.trim_end().to_string();
                                if !line.is_empty() {
                                    stderr_lines.push(line.clone());
                                    tracing::warn!("{}", line);

                                    if let (Some(tid), Some(repo)) = (task_id, task_repo) {
                                        let log_line = format!("[WARN] {}", line);
                                        let _ = repo.append_build_log(tid, &log_line).await;
                                        if let Some(ws) = ws_manager {
                                            ws.broadcast_log(tid, log_line, false);
                                        }
                                    }
                                }
                            }
                            _ => {
                                stderr_done = true;
                            }
                        }
                    }
                    _ = tokio::time::sleep(std::time::Duration::from_millis(50)), if cancelled_flag.is_some() => {
                        // 定期唤醒以检查循环头部的 cancelled_flag
                    }
                }
            }
            
            // 再次检查取消标志（在等待进程完成前）
            if let Some(flag) = &cancelled_flag {
                if flag.load(Ordering::SeqCst) {
                    Self::terminate_child(&mut child).await;
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

  