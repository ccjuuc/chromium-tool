use std::path::Path;
use std::sync::Arc;
use anyhow::Result;
use crate::config::AppConfig;
use crate::model::build::BuildRequest;
use crate::repository::task::TaskRepository;
use crate::service::build::{ProjectBuilder, Compiler, InstallerBuilder};
use crate::service::backup::BackupManager;
use crate::service::email::EmailSender;
use crate::service::task::TaskManager;
use crate::util::{git, time};
use crate::model::task::CreateTask;

#[derive(Clone)]
pub struct BuildService {
    config: Arc<AppConfig>,
    builder: ProjectBuilder,
    compiler: Compiler,
    installer: InstallerBuilder,
    backup_manager: BackupManager,
    email_sender: EmailSender,
    ws_manager: Option<crate::api::ws::WsManager>,
}

impl BuildService {
    pub fn new(config: AppConfig) -> Self {
        let config_arc = Arc::new(config.clone());
        Self {
            config: config_arc.clone(),
            builder: ProjectBuilder::new(config.clone()),
            compiler: Compiler::new(config.clone()),
            installer: InstallerBuilder::new(config.clone()),
            backup_manager: BackupManager::new(config.clone()),
            email_sender: EmailSender::new(config),
            ws_manager: None,
        }
    }
    
    pub fn with_ws_manager(mut self, ws_manager: crate::api::ws::WsManager) -> Self {
        self.ws_manager = Some(ws_manager);
        self
    }
    
    /// 创建任务但不启动（保持 pending 状态，用于排队）
    pub async fn create_build_task(
        &self,
        request: BuildRequest,
        task_repo: &TaskRepository,
    ) -> Result<i64> {
        let oem = request.oem_name
            .split('=')
            .nth(1)
            .unwrap_or_default()
            .to_string();
        
        // 在 pkg_flag 中包含架构信息
        let mut pkg_flag = request.pkg_flag.clone();
        if let Some(arch) = request.architectures.first() {
            if !pkg_flag.is_empty() {
                pkg_flag = format!("{} [{}]", pkg_flag, arch);
            } else {
                pkg_flag = format!("[{}]", arch);
            }
        }
        
        let architecture = request.architectures.first().cloned();
        let create_task = CreateTask {
            branch: request.branch.clone(),
            oem_name: oem.clone(),
            commit_id: request.commit_id.clone().unwrap_or_default(),
            pkg_flag,
            is_increment: request.is_increment,
            is_signed: request.is_signed,
            server: request.server.clone(),
            parent_id: None,
            architecture,
        };
        
        let task_id = task_repo.create(&create_task).await?;
        // 确保状态为 pending（数据库默认状态）
        task_repo.update_state(task_id, crate::model::state::TaskState::Pending, None).await?;
        
        Ok(task_id)
    }
    
    #[allow(dead_code)]
    pub async fn start_build(
        &self,
        request: BuildRequest,
        task_manager: TaskManager,
        task_repo: Arc<TaskRepository>,
        app_state: Option<Arc<crate::api::AppState>>,
    ) -> Result<i64> {
        // 创建任务
        let task_id = self.create_build_task(request.clone(), task_repo.as_ref()).await?;
        
        // 启动异步构建
        self.start_pending_task(task_id, request, task_manager, task_repo, app_state).await?;
        
        Ok(task_id)
    }
    
    /// 启动一个 pending 任务
    pub async fn start_pending_task(
        &self,
        task_id: i64,
        request: BuildRequest,
        task_manager: TaskManager,
        task_repo: Arc<TaskRepository>,
        on_complete: Option<Arc<crate::api::AppState>>,
    ) -> Result<()> {
        // 在启动前，再次检查任务状态，确保任务没有被删除或标记为失败
        match task_repo.find_by_id(task_id).await {
            Ok(task) => {
                // 如果任务已经被标记为失败、取消或被删除，不启动
                if matches!(task.state, crate::model::state::TaskState::Failed | crate::model::state::TaskState::Cancelled) {
                    tracing::warn!("⚠️  任务 #{} 已被标记为失败或取消，跳过启动", task_id);
                    eprintln!("⚠️  任务 #{} 已被标记为失败或取消，跳过启动", task_id);
                    return Err(anyhow::anyhow!("Task {} has been marked as failed or cancelled", task_id));
                }
            },
            Err(e) => {
                tracing::warn!("⚠️  无法获取任务 #{} 的信息: {}，可能已被删除，跳过启动", task_id, e);
                eprintln!("⚠️  无法获取任务 #{} 的信息: {}，可能已被删除，跳过启动", task_id, e);
                return Err(anyhow::anyhow!("Task {} not found or has been deleted: {}", task_id, e));
            }
        }
        
        // 更新状态为 start build
        task_repo.update_state(task_id, crate::model::state::TaskState::StartBuild, None).await?;
        
        // 启动异步构建
        let config_clone = self.config.clone();
        let request_clone = request.clone();
        let builder_clone = self.builder.clone();
        let compiler_clone = self.compiler.clone();
        let installer_clone = self.installer.clone();
        let backup_clone = self.backup_manager.clone();
        let email_clone = self.email_sender.clone();
        
        let task_repo_clone_owned = (*task_repo).clone();
        let ws_manager_clone = self.ws_manager.clone();
        let server = request.server.clone();
        let app_state = on_complete;
        
        // 创建取消标志（在 start_task 之前创建，确保可以被 cancel_task 找到）
        let cancelled_flag = task_manager.create_cancelled_flag(task_id);
        let cancelled_flag_for_check = cancelled_flag.clone();
        
        task_manager.start_task(task_id, cancelled_flag.clone(), async move {
            let result = do_build(
                config_clone,
                request_clone,
                task_repo_clone_owned,
                task_id,
                builder_clone,
                compiler_clone,
                installer_clone,
                backup_clone,
                email_clone,
                ws_manager_clone,
                Some(cancelled_flag),
            ).await;
            
            // 任务完成后，记录日志
            if let Err(e) = &result {
                tracing::error!("任务 #{} 执行失败: {:?}", task_id, e);
            }
            
            // 检查任务是否被取消（通过检查取消标志）
            let was_cancelled = cancelled_flag_for_check.load(std::sync::atomic::Ordering::Relaxed);
            
            // 如果任务被取消，不启动下一个 pending 任务
            if was_cancelled {
                tracing::info!("任务 #{} 已被取消，跳过启动下一个 pending 任务", task_id);
            } else if let Some(state) = app_state {
                // 只有在任务未被取消的情况下，才启动下一个 pending 任务
                let state_clone = state.clone();
                let server_clone = server.clone();
                tokio::spawn(async move {
                    // 等待一小段时间，确保当前任务状态已更新
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    state_clone.start_next_pending_task(server_clone).await;
                });
            }
            
            result
        }).await?;
        
        Ok(())
    }
    
    // 创建子任务（不启动，状态为 pending）
    pub async fn create_child_task(
        &self,
        request: BuildRequest,
        parent_id: i64,
        task_repo: &TaskRepository,
    ) -> Result<i64> {
        let oem = request.oem_name
            .split('=')
            .nth(1)
            .unwrap_or_default()
            .to_string();
        
        // 在 pkg_flag 中包含架构信息
        let mut pkg_flag = request.pkg_flag.clone();
        let architecture = request.architectures.first().cloned();
        if let Some(arch) = &architecture {
            if !pkg_flag.is_empty() {
                pkg_flag = format!("{} [{}]", pkg_flag, arch);
            } else {
                pkg_flag = format!("[{}]", arch);
            }
        }
        
        let create_task = CreateTask {
            branch: request.branch.clone(),
            oem_name: oem.clone(),
            commit_id: request.commit_id.clone().unwrap_or_default(),
            pkg_flag,
            is_increment: request.is_increment,
            is_signed: request.is_signed,
            server: request.server.clone(),
            parent_id: Some(parent_id),  // 设置父任务ID
            architecture,  // 设置架构信息
        };
        
        let task_id = task_repo.create(&create_task).await?;
        
        // 确保任务状态为 pending（数据库默认状态）
        task_repo.update_state(task_id, crate::model::state::TaskState::Pending, None).await?;
        
        Ok(task_id)
    }
    
    // 启动子任务（状态变为 start build）
    pub async fn start_child_task(
        &self,
        task_id: i64,
        request: BuildRequest,
        task_manager: TaskManager,
        task_repo: Arc<TaskRepository>,
    ) -> Result<()> {
        // 在启动前，再次检查任务状态，确保任务没有被删除或标记为失败/取消
        match task_repo.find_by_id(task_id).await {
            Ok(task) => {
                // 如果任务已经被标记为失败、取消或被删除，不启动
                if matches!(task.state, crate::model::state::TaskState::Failed | crate::model::state::TaskState::Cancelled) {
                    tracing::warn!("⚠️  子任务 #{} 已被标记为失败或取消，跳过启动", task_id);
                    eprintln!("⚠️  子任务 #{} 已被标记为失败或取消，跳过启动", task_id);
                    return Err(anyhow::anyhow!("Child task {} has been marked as failed or cancelled", task_id));
                }
            },
            Err(e) => {
                tracing::warn!("⚠️  无法获取子任务 #{} 的信息: {}，可能已被删除，跳过启动", task_id, e);
                eprintln!("⚠️  无法获取子任务 #{} 的信息: {}，可能已被删除，跳过启动", task_id, e);
                return Err(anyhow::anyhow!("Child task {} not found or has been deleted: {}", task_id, e));
            }
        }
        
        // 更新状态为 start build
        task_repo.update_state(task_id, crate::model::state::TaskState::StartBuild, None).await?;
        
        // 启动异步构建
        let config_clone = self.config.clone();
        let request_clone = request.clone();
        let task_repo_clone_owned = (*task_repo).clone();
        let builder_clone = self.builder.clone();
        let compiler_clone = self.compiler.clone();
        let installer_clone = self.installer.clone();
        let backup_clone = self.backup_manager.clone();
        let email_clone = self.email_sender.clone();
        
        let ws_manager_clone = self.ws_manager.clone();
        
        // 创建取消标志（在 start_task 之前创建，确保可以被 cancel_task 找到）
        let cancelled_flag = task_manager.create_cancelled_flag(task_id);
        
        task_manager.start_task(task_id, cancelled_flag.clone(), async move {
            do_build(
                config_clone,
                request_clone,
                task_repo_clone_owned,
                task_id,
                builder_clone,
                compiler_clone,
                installer_clone,
                backup_clone,
                email_clone,
                ws_manager_clone,
                Some(cancelled_flag),
            ).await
        }).await?;
        
        Ok(())
    }
    
    #[allow(dead_code)]
    pub async fn start_build_with_parent(
        &self,
        request: BuildRequest,
        parent_id: i64,
        task_manager: TaskManager,
        task_repo: Arc<TaskRepository>,
    ) -> Result<i64> {
        let task_id = self.create_child_task(request.clone(), parent_id, task_repo.as_ref()).await?;
        self.start_child_task(task_id, request, task_manager, task_repo).await?;
        Ok(task_id)
    }
}

async fn do_build(
    config: Arc<AppConfig>,
    request: BuildRequest,
    task_repo: TaskRepository,
    task_id: i64,
    builder: ProjectBuilder,
    compiler: Compiler,
    installer: InstallerBuilder,
    _backup_manager: BackupManager,
    email_sender: EmailSender,
    ws_manager: Option<crate::api::ws::WsManager>,
    cancelled_flag: Option<Arc<std::sync::atomic::AtomicBool>>,
) -> Result<()> {
    let src_path = Path::new(config.get_src_path()?);
    let build_start_time = std::time::Instant::now();
    
    tracing::info!("🚀 =========================================");
    tracing::info!("🚀 开始构建任务 #{}", task_id);
    tracing::info!("🚀 =========================================");
    tracing::info!("📦 分支: {}", request.branch);
    tracing::info!("🏷️  OEM: {}", request.oem_name);
    tracing::info!("🖥️  平台: {}", request.platform);
    tracing::info!("📁 源码路径: {}", src_path.display());
    tracing::info!("═══════════════════════════════════════════════════════\n");
    
    // 生成输出目录名称
    let out_dir = generate_out_dir(&config, &request)?;
    tracing::info!("📂 输出目录: {}\n", out_dir);
    
    // 获取配置的构建步骤（根据架构）
    let architecture = request.architectures.first().map(|s| s.as_str());
    let build_steps = config.get_build_steps(architecture);
    if let Some(arch) = architecture {
        tracing::info!("🏗️  构建架构: {}\n", arch);
    }
    let total_steps = build_steps.len();
    let mut commit_id = String::new();
    
    // 遍历执行每个构建步骤
    for (index, step) in build_steps.iter().enumerate() {
        // 在每个步骤开始前检查取消标志
        if let Some(flag) = &cancelled_flag {
            if flag.load(std::sync::atomic::Ordering::Relaxed) {
                tracing::warn!("⚠️  任务 #{} 已取消，停止执行后续步骤", task_id);
                eprintln!("⚠️  任务 #{} 已取消，停止执行后续步骤", task_id);
                return Err(anyhow::anyhow!("Task cancelled"));
            }
        }
        
        let step_num = index + 1;
        
        // 检查跳过条件
        if should_skip_step(&step, &request) {
            tracing::info!("⏭️  步骤 {}/{}: 跳过 {}（条件不满足）\n", step_num, total_steps, step.name);
            continue;
        }
        
        // 更新任务状态
        if let Some(state_str) = &step.state {
            if let Some(state) = crate::model::state::TaskState::from_str(state_str) {
                task_repo.update_state(task_id, state, None).await?;
            }
        }
        
        tracing::info!("步骤 {}/{}: {}", step_num, total_steps, step.name);
        
        // 再次检查取消标志（在步骤执行前）
        if let Some(flag) = &cancelled_flag {
            if flag.load(std::sync::atomic::Ordering::Relaxed) {
                tracing::warn!("任务 #{} 已取消，停止执行步骤: {}", task_id, step.name);
                return Err(anyhow::anyhow!("Task cancelled"));
            }
        }
        
        let step_start = std::time::Instant::now();
        
        // 根据步骤类型执行相应操作
        let step_result = match step.step_type.as_str() {
            "git" => {
                match step.target.as_deref() {
                    Some("update") => {
                        git::update_code(
                            src_path,
                            &request.branch,
                            request.commit_id.as_deref(),
                        ).await
                    },
                    Some("get_commit_id") => {
                        let id = git::get_commit_id(src_path).await?;
                        commit_id = id.clone();
                        tracing::info!("✅ Commit ID: {}\n", commit_id);
                        
                        // 在第一次获取 commit_id 时，立即更新父任务和所有子任务的 commit_id
                        if let Err(e) = task_repo.update_family_commit_id(task_id, &commit_id).await {
                            tracing::warn!("⚠️  更新父子任务 commit_id 失败: {}", e);
                        }
                        
                        // 更新当前任务的状态
                        if let Some(state_str) = &step.state {
                            if let Some(state) = crate::model::state::TaskState::from_str(state_str) {
                                task_repo.update_state(task_id, state, Some(&commit_id)).await?;
                            }
                        }
                        Ok(())
                    },
                    _ => {
                        tracing::warn!("⚠️  未知的 git 操作: {:?}", step.target);
                        Ok(())
                    }
                }
            },
            "clean" => {
                builder.clean(src_path, &out_dir, request.is_increment).await
            },
            "gn_gen" => {
                builder.generate(src_path, &out_dir, &request).await
            },
            "ninja" => {
                if let Some(target) = &step.target {
                    compiler.build_targets(src_path, &out_dir, &[target], &step.name, Some(task_id), Some(&task_repo), ws_manager.as_ref(), cancelled_flag.clone()).await
                } else {
                    Ok(())
                }
            },
            "installer" => {
                installer.build_installer(src_path, &out_dir).await
            },
            "backup" => {
                // TODO: 实现备份逻辑
                tracing::info!("⏭️  备份功能待实现");
                Ok(())
            },
            _ => {
                tracing::warn!("⚠️  未知的步骤类型: {}", step.step_type);
                Ok(())
            }
        };
        
        // 检查步骤执行结果，如果被取消则立即返回
        match step_result {
            Err(e) if e.to_string().contains("cancelled") => {
                tracing::warn!("⚠️  步骤 {} 被取消", step.name);
                eprintln!("⚠️  步骤 {} 被取消", step.name);
                return Err(e);
            },
            Err(e) => return Err(e),
            Ok(()) => {},
        }
        
        // 步骤完成后再次检查取消标志
        if let Some(flag) = &cancelled_flag {
            if flag.load(std::sync::atomic::Ordering::Relaxed) {
                tracing::warn!("⚠️  任务 #{} 已取消，停止执行后续步骤", task_id);
                eprintln!("⚠️  任务 #{} 已取消，停止执行后续步骤", task_id);
                return Err(anyhow::anyhow!("Task cancelled"));
            }
        }
        
        let step_duration = step_start.elapsed();
        tracing::debug!("{} 完成，耗时: {:.2} 秒", step.name, step_duration.as_secs_f64());
    }
    
    // 确保有 commit_id
    if commit_id.is_empty() {
        commit_id = git::get_commit_id(src_path).await?;
    }
    
    // 更新任务状态为成功
    let end_time = time::format_date_time()?;
    let total_duration = build_start_time.elapsed();
    task_repo.update_completion(
        task_id,
        &end_time,
        "",
        "",
        Some(&commit_id),
    ).await?;
    
    tracing::info!("🎉 =========================================");
    tracing::info!("🎉 构建任务 #{} 完成！", task_id);
    tracing::info!("🎉 =========================================");
    tracing::info!("⏱️  总耗时: {:.2} 秒 ({:.2} 分钟)", 
        total_duration.as_secs_f64(),
        total_duration.as_secs_f64() / 60.0);
    tracing::info!("📅 完成时间: {}", end_time);
    tracing::info!("═══════════════════════════════════════════════════════\n");
    
    // 发送邮件
    let email = request.email.clone();
    if let Err(e) = email_sender.send_notification(
        task_id,
        &request,
        email.as_deref(),
    ).await {
        tracing::warn!("Failed to send email: {:?}", e);
    }
    
    Ok(())
}

/// 生成输出目录名称
/// 根据构建参数和架构生成类似 out/Release、out/Release_x64、out/Release_arm64、release64 等目录名
fn generate_out_dir(config: &AppConfig, request: &BuildRequest) -> Result<String> {
    // 检查是否为 debug 构建
    let is_debug = if let Ok(gn_args) = config.get_gn_default_args() {
        gn_args.iter().any(|arg| arg.contains("is_debug=true"))
    } else {
        false
    };
    
    // 构建目录名称
    let build_type = if is_debug { "Debug" } else { "Release" };
    
    // 根据架构生成 CPU 后缀（架构是必选的，直接拼接）
    let cpu_suffix = request.architectures
        .first()
        .map(|arch| format!("_{}", arch))
        .unwrap_or_default();
    
    // 根据平台和配置生成目录名
    let os = std::env::consts::OS;
    let out_dir = match os {
        "macos" | "linux" => {
            // macOS 和 Linux 使用 out/Release、out/Release_x64、out/Release_arm64 等
            if cpu_suffix.is_empty() {
                format!("out/{}", build_type)
            } else {
                format!("out/{}{}", build_type, cpu_suffix)
            }
        },
        "windows" => {
            // Windows 可能使用 release64 或 out/Release_x64
            if request.is_x64 && !is_debug && cpu_suffix == "_x64" {
                "release64".to_string()
            } else if cpu_suffix.is_empty() {
                format!("out/{}", build_type)
            } else {
                format!("out/{}{}", build_type, cpu_suffix)
            }
        },
        _ => {
            // 默认格式
            if cpu_suffix.is_empty() {
                format!("out/{}", build_type)
            } else {
                format!("out/{}{}", build_type, cpu_suffix)
            }
        }
    };
    
    Ok(out_dir)
}

/// 检查是否应该跳过步骤
fn should_skip_step(step: &crate::config::BuildStep, request: &BuildRequest) -> bool {
    if let Some(skip_if) = &step.skip_if {
        // 解析跳过条件，格式如 "is_update=false", "target_os=macos"
        if skip_if.contains("is_update=") {
            let should_update = skip_if.contains("is_update=false");
            return should_update && !request.is_update;
        }
        // 可以添加更多条件判断
    }
    false
}

// Clone 实现已移到各自的模块中

