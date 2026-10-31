use std::path::Path;
use anyhow::{Context, Result};
use tokio::process::Command;
use crate::util::retry::retry_async;

pub async fn update_code(
    src_path: &Path,
    branch: &str,
    commit_id: Option<&str>,
) -> Result<()> {
    // git stash
    tracing::info!("📋 执行命令: git stash");
    tracing::info!("📁 工作目录: {}", src_path.display());
    let start_time = std::time::Instant::now();
    let stash_output = Command::new("git")
        .arg("stash")
        .current_dir(src_path)
        .output()
        .await
        .context("Failed to stash changes")?;
    let duration = start_time.elapsed();
    let exit_code = stash_output.status.code().unwrap_or(-1);
    
    if !stash_output.stdout.is_empty() {
        tracing::info!("✅ 标准输出:\n{}", String::from_utf8_lossy(&stash_output.stdout));
    }
    if !stash_output.stderr.is_empty() && !stash_output.status.success() {
        tracing::warn!("⚠️  标准错误:\n{}", String::from_utf8_lossy(&stash_output.stderr));
    }
    tracing::info!("⏱️  执行时间: {:.2} 秒, 退出码: {}\n", duration.as_secs_f64(), exit_code);
    
    // git checkout commit_id (if provided)
    if let Some(commit) = commit_id {
        tracing::info!("📋 执行命令: git checkout {}", commit);
        tracing::info!("📁 工作目录: {}", src_path.display());
        let start_time = std::time::Instant::now();
        let checkout_output = Command::new("git")
            .arg("checkout")
            .arg(commit)
            .current_dir(src_path)
            .output()
            .await
            .context("Failed to checkout commit")?;
        let duration = start_time.elapsed();
        let exit_code = checkout_output.status.code().unwrap_or(-1);
        
        if !checkout_output.stdout.is_empty() {
            tracing::info!("✅ 标准输出:\n{}", String::from_utf8_lossy(&checkout_output.stdout));
        }
        if !checkout_output.stderr.is_empty() {
            if checkout_output.status.success() {
                tracing::info!("ℹ️  标准输出:\n{}", String::from_utf8_lossy(&checkout_output.stderr));
            } else {
                tracing::error!("❌ 标准错误:\n{}", String::from_utf8_lossy(&checkout_output.stderr));
                return Err(anyhow::anyhow!(
                    "git checkout {} failed with exit code {}",
                    commit,
                    exit_code
                ));
            }
        }
        tracing::info!("⏱️  执行时间: {:.2} 秒, 退出码: {}\n", duration.as_secs_f64(), exit_code);
    }
    
    // git checkout branch
    tracing::info!("📋 执行命令: git checkout {}", branch);
    tracing::info!("📁 工作目录: {}", src_path.display());
    let start_time = std::time::Instant::now();
    let checkout_output = Command::new("git")
        .arg("checkout")
        .arg(branch)
        .current_dir(src_path)
        .output()
        .await
        .context("Failed to checkout branch")?;
    let duration = start_time.elapsed();
    let exit_code = checkout_output.status.code().unwrap_or(-1);
    
    if !checkout_output.stdout.is_empty() {
        tracing::info!("✅ 标准输出:\n{}", String::from_utf8_lossy(&checkout_output.stdout));
    }
    if !checkout_output.stderr.is_empty() {
        if checkout_output.status.success() {
            tracing::info!("ℹ️  标准输出:\n{}", String::from_utf8_lossy(&checkout_output.stderr));
        } else {
            tracing::error!("❌ 标准错误:\n{}", String::from_utf8_lossy(&checkout_output.stderr));
            return Err(anyhow::anyhow!(
                "git checkout {} failed with exit code {}",
                branch,
                exit_code
            ));
        }
    }
    tracing::info!("⏱️  执行时间: {:.2} 秒, 退出码: {}\n", duration.as_secs_f64(), exit_code);
    
    // git pull with retry
    tracing::info!("📋 执行命令: git pull (带重试)");
    tracing::info!("📁 工作目录: {}", src_path.display());
    let pull_start = std::time::Instant::now();
    retry_async(|| async {
        let output = Command::new("git")
            .arg("pull")
            .current_dir(src_path)
            .output()
            .await?;
        
        let exit_code = output.status.code().unwrap_or(-1);
        if !output.stdout.is_empty() {
            tracing::info!("✅ 标准输出:\n{}", String::from_utf8_lossy(&output.stdout));
        }
        if !output.stderr.is_empty() {
            if output.status.success() {
                tracing::info!("ℹ️  标准输出:\n{}", String::from_utf8_lossy(&output.stderr));
            } else {
                tracing::error!("❌ 标准错误:\n{}", String::from_utf8_lossy(&output.stderr));
            }
        }
        
        if output.status.success() {
            Ok(())
        } else {
            Err(anyhow::anyhow!("Git pull failed with exit code {}", exit_code))
        }
    })
    .await
    .context("Failed to pull changes")?;
    let pull_duration = pull_start.elapsed();
    tracing::info!("⏱️  执行时间: {:.2} 秒\n", pull_duration.as_secs_f64());
    
    Ok(())
}

pub async fn get_commit_id(src_path: &Path) -> Result<String> {
    tracing::info!("📋 执行命令: git rev-parse HEAD");
    tracing::info!("📁 工作目录: {}", src_path.display());
    
    let output = Command::new("git")
        .args(&["rev-parse", "HEAD"])
        .current_dir(src_path)
        .output()
        .await
        .context("Failed to get commit id")?;
    
    let exit_code = output.status.code().unwrap_or(-1);
    
    if !output.status.success() {
        if !output.stderr.is_empty() {
            tracing::error!("❌ 标准错误:\n{}", String::from_utf8_lossy(&output.stderr));
        }
        return Err(anyhow::anyhow!(
            "Failed to get commit id, exit code: {}",
            exit_code
        ));
    }
    
    let commit_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    tracing::info!("✅ Commit ID: {}\n", commit_id);
    
    Ok(commit_id)
}

/// 获取所有分支列表
pub async fn get_branch_list(src_path: &Path) -> Result<Vec<String>> {
    tracing::info!("📋 执行命令: git branch -a");
    tracing::info!("📁 工作目录: {}", src_path.display());
    
    let output = Command::new("git")
        .args(&["branch", "-a"])
        .current_dir(src_path)
        .output()
        .await
        .context("Failed to get branch list")?;
    
    let exit_code = output.status.code().unwrap_or(-1);
    
    if !output.status.success() {
        if !output.stderr.is_empty() {
            tracing::error!("❌ 标准错误:\n{}", String::from_utf8_lossy(&output.stderr));
        }
        return Err(anyhow::anyhow!(
            "Failed to get branch list, exit code: {}",
            exit_code
        ));
    }
    
    let output_str = String::from_utf8_lossy(&output.stdout);
    let branches: Vec<String> = output_str
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            // 跳过远程分支（remotes/）和 HEAD 指针
            if line.starts_with("remotes/") || line.contains("HEAD") {
                return None;
            }
            // 移除 * 标记（当前分支）
            let branch = line.trim_start_matches("*").trim();
            if branch.is_empty() {
                None
            } else {
                Some(branch.to_string())
            }
        })
        .collect();
    
    tracing::info!("✅ 找到 {} 个分支\n", branches.len());
    
    Ok(branches)
}

/// 获取主分支列表（main, master, develop 等）
#[allow(dead_code)]
pub async fn get_main_branches(src_path: &Path) -> Result<Vec<String>> {
    let all_branches = get_branch_list(src_path).await?;
    
    // 优先顺序：main > master > develop
    let priority_branches = vec!["main", "master", "develop"];
    
    let mut main_branches = Vec::new();
    for priority in &priority_branches {
        if all_branches.contains(&priority.to_string()) {
            main_branches.push(priority.to_string());
        }
    }
    
    // 如果没有找到任何主分支，返回所有分支
    if main_branches.is_empty() {
        Ok(all_branches)
    } else {
        Ok(main_branches)
    }
}

