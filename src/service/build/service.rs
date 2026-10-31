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
            oem_name: String::new(),  // 已删除 OEM 配置
            commit_id: request.commit_id.clone().unwrap_or_default(),
            pkg_flag,
            is_increment: request.is_increment,
            is_signed: request.is_signed,
            server: request.server.clone(),
            parent_id: None,
            architecture,
            installer_format: request.installer_format.clone(),
            request_json: serde_json::to_string(&request).ok(),
        };
        
        let task_id = task_repo.create(&create_task).await?;
        
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
        let task_repo_for_fail = task_repo_clone_owned.clone(); // 为错误处理克隆一份
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
            
            // 检查任务是否被取消（通过检查取消标志）
            let was_cancelled = cancelled_flag_for_check.load(std::sync::atomic::Ordering::SeqCst);

            // 任务完成后，记录日志
            if let Err(e) = &result {
                tracing::error!("任务 #{} 执行失败: {:?}", task_id, e);
                let final_state = if was_cancelled {
                    crate::model::state::TaskState::Cancelled
                } else {
                    crate::model::state::TaskState::Failed
                };
                if let Err(update_err) = task_repo_for_fail.update_state(task_id, final_state, None).await {
                    tracing::error!("更新任务 #{} 状态失败: {:?}", task_id, update_err);
                }
                // 如果是子任务失败，同步更新父任务状态为失败（若没有其他子任务仍在运行）
                if let Ok(task) = task_repo_for_fail.find_by_id(task_id).await {
                    if let Some(parent_id) = task.parent_id {
                        let _ = task_repo_for_fail.update_state(parent_id, final_state, None).await;
                    }
                }
            }
            
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
            oem_name: String::new(),  // 已删除 OEM 配置
            commit_id: request.commit_id.clone().unwrap_or_default(),
            pkg_flag,
            is_increment: request.is_increment,
            is_signed: request.is_signed,
            server: request.server.clone(),
            parent_id: Some(parent_id),  // 设置父任务ID
            architecture,  // 设置架构信息
            installer_format: request.installer_format.clone(),
            request_json: serde_json::to_string(&request).ok(),
        };
        
        let task_id = task_repo.create(&create_task).await?;
        
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
    
    /// 执行组合步骤（仅用于父任务）
    pub async fn execute_combine_step(
        &self,
        parent_id: i64,
        request: BuildRequest,
        task_repo: TaskRepository,
        config: Arc<AppConfig>,
    ) -> Result<()> {
        use std::path::Path;
        
        let src_path = Path::new(config.get_src_path()?);
        
        tracing::info!("🔗 开始执行组合步骤，父任务 #{}", parent_id);
        
        // 执行组合
        self.installer.combine_universal_pkg(src_path, &request.architectures).await?;
        
        // 更新状态为 build installer
        task_repo.update_state(parent_id, crate::model::state::TaskState::BuildingInstaller, None).await?;
        
        // 生成 universal pkg
        let universal_out_dir = "out/Release_universal";
        self.installer.build_installer(src_path, universal_out_dir, request.installer_format.as_deref()).await?;
        
        // 备份 universal 产物
        let (storage_path, installer_json) = self
            .backup_manager
            .backup_build_artifacts(src_path, universal_out_dir)
            .await
            .unwrap_or_default();

        // 更新任务状态为成功
        let end_time = time::format_date_time()?;
        let commit_id = request.commit_id.unwrap_or_default();
        task_repo.update_completion(
            parent_id,
            &end_time,
            &storage_path,
            &installer_json,
            if commit_id.is_empty() { None } else { Some(&commit_id) },
        ).await?;
        
        tracing::info!("✅ 组合步骤完成，父任务 #{}", parent_id);
        Ok(())
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
    backup_manager: BackupManager,
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
    tracing::info!("🖥️  平台: {}", request.platform);
    tracing::info!("📁 源码路径: {}", src_path.display());
    tracing::info!("📋 ====== 前端传递的完整配置 ======");
    tracing::info!("   commit_id: {:?}", request.commit_id);
    tracing::info!("   pkg_flag: {}", request.pkg_flag);
    tracing::info!("   is_update: {}", request.is_update);
    tracing::info!("   is_x64: {}", request.is_x64);
    tracing::info!("   architectures: {:?}", request.architectures);
    tracing::info!("   is_increment: {}", request.is_increment);
    tracing::info!("   is_signed: {}", request.is_signed);
    tracing::info!("   server: {}", request.server);
    tracing::info!("   custom_args: {:?}", request.custom_args);
    tracing::info!("   emails: {:?}", request.emails);
    tracing::info!("   installer_format: {:?}", request.installer_format);
    tracing::info!("📋 ================================");
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
    let mut storage_path = String::new();
    let mut installer_json = String::new();
    
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
                // 检查是否是子任务，如果是子任务且是 macOS 平台，则跳过 installer（组合任务会在父任务中执行）
                let task = task_repo.find_by_id(task_id).await?;
                if task.parent_id.is_some() && request.platform == "macos" {
                    // 这是 macOS 的子任务，跳过 installer，等待父任务的组合步骤（注意不要提前 return 退出整个 do_build）
                    tracing::info!("⏭️  子任务跳过 installer（macOS 组合任务将在父任务中执行）");
                    Ok(())
                } else {
                    installer.build_installer(src_path, &out_dir, request.installer_format.as_deref()).await
                }
            },
            "combine" => {
                // 组合步骤：仅用于父任务，组合多个架构的 app 并生成 universal pkg
                if request.platform != "macos" {
                    return Err(anyhow::anyhow!("组合任务仅支持 macOS"));
                }
                
                if request.architectures.len() < 2 {
                    return Err(anyhow::anyhow!("组合任务需要至少2个架构"));
                }
                
                // 检查所有子任务是否都完成了 build chrome
                let task = task_repo.find_by_id(task_id).await?;
                if task.parent_id.is_some() {
                    return Err(anyhow::anyhow!("组合步骤只能在父任务中执行"));
                }
                
                // 获取所有子任务
                let children = task_repo.get_child_tasks(task_id).await?;
                if children.len() < 2 {
                    return Err(anyhow::anyhow!("组合任务需要至少2个子任务"));
                }
                
                // 检查所有子任务是否都完成了 build chrome
                let all_completed = children.iter().all(|child| {
                    matches!(
                        child.state,
                        crate::model::state::TaskState::Combining |
                        crate::model::state::TaskState::BuildingInstaller |
                        crate::model::state::TaskState::Signing |
                        crate::model::state::TaskState::BackingUp |
                        crate::model::state::TaskState::Success
                    )
                });
                
                if !all_completed {
                    return Err(anyhow::anyhow!("等待所有子任务完成 build chrome"));
                }
                
                // 执行组合
                installer.combine_universal_pkg(src_path, &request.architectures).await
            },
            "backup" => {
                let task = task_repo.find_by_id(task_id).await?;
                if task.parent_id.is_some() && request.platform == "macos" {
                    tracing::info!("⏭️  macOS 子任务跳过单独备份（将在父任务合并后统一备份）");
                    Ok(())
                } else {
                    match backup_manager.backup_build_artifacts(src_path, &out_dir).await {
                        Ok((sp, ij)) => {
                            storage_path = sp;
                            installer_json = ij;
                            Ok(())
                        }
                        Err(e) => {
                            tracing::warn!("⚠️  备份构建产物失败: {}", e);
                            Ok(())
                        }
                    }
                }
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
        
        // 如果是子任务且刚完成 build chrome，检查是否可以开始组合
        let task = task_repo.find_by_id(task_id).await?;
        if let Some(parent_id) = task.parent_id {
            // 这是子任务，检查是否刚完成 build chrome
            if step.step_type == "ninja" && step.target.as_deref() == Some("chrome") {
                // 检查所有子任务是否都完成了 build chrome
                if let Ok(all_completed) = task_repo.all_children_completed_chrome(parent_id, task_id).await {
                    if all_completed {
                        // 所有子任务都完成了 build chrome，启动父任务的组合步骤
                        tracing::info!("✅ 所有子任务完成 build chrome，准备启动组合步骤");
                        
                        // 获取父任务信息及所有子任务的架构列表
                        if let Ok(parent_task) = task_repo.find_by_id(parent_id).await {
                            let children = task_repo.get_child_tasks(parent_id).await.unwrap_or_default();
                            let parent_archs: Vec<String> = children
                                .iter()
                                .filter_map(|c| c.architecture.clone())
                                .collect();

                            // 检查是否是 macOS 平台
                            let platform = if request.platform == "macos" {
                                "macos"
                            } else if request.server.contains("macos") || request.server.contains("193") {
                                "macos"
                            } else {
                                request.platform.as_str()
                            };
                            
                            if platform == "macos" && parent_archs.len() >= 2 {
                                // 构建父任务的 BuildRequest（使用父任务汇总的所有子任务架构列表）
                                let parent_request = BuildRequest {
                                    branch: parent_task.branch_name.clone(),
                                    commit_id: if parent_task.commit_id.is_empty() { None } else { Some(parent_task.commit_id) },
                                    pkg_flag: parent_task.pkg_flag.clone(),
                                    is_increment: parent_task.is_increment,
                                    is_x64: false,
                                    architectures: parent_archs,
                                    platform: "macos".to_string(),
                                    is_signed: parent_task.is_signed,
                                    server: parent_task.server.clone(),
                                    custom_args: None,
                                    is_update: false,
                                    emails: request.emails.clone(),
                                    installer_format: parent_task.installer_format.or_else(|| request.installer_format.clone()),
                                };
                                
                                // 启动父任务的组合步骤
                                let build_service_clone = BuildService {
                                    config: config.clone(),
                                    builder: builder.clone(),
                                    compiler: compiler.clone(),
                                    installer: installer.clone(),
                                    backup_manager: backup_manager.clone(),
                                    email_sender: email_sender.clone(),
                                    ws_manager: ws_manager.clone(),
                                };
                                
                                // 异步启动父任务的组合步骤（不阻塞当前任务）
                                let task_repo_clone = task_repo.clone();
                                let config_clone = config.clone();
                                tokio::spawn(async move {
                                    // 等待一小段时间，确保所有子任务状态已更新
                                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                                    
                                    // 更新父任务状态为 combining
                                    let task_repo_for_update = task_repo_clone.clone();
                                    if let Err(e) = task_repo_for_update.update_state(parent_id, crate::model::state::TaskState::Combining, None).await {
                                        tracing::error!("更新父任务状态失败: {}", e);
                                        return;
                                    }
                                    
                                    // 执行组合步骤
                                    let task_repo_for_combine = task_repo_clone.clone();
                                    let task_repo_for_fail: TaskRepository = task_repo_clone.clone();
                                    if let Err(e) = build_service_clone.execute_combine_step(
                                        parent_id,
                                        parent_request,
                                        task_repo_for_combine,
                                        config_clone,
                                    ).await {
                                        tracing::error!("组合步骤执行失败: {}", e);
                                        let _ = task_repo_for_fail.update_state(parent_id, crate::model::state::TaskState::Failed, None).await;
                                    }
                                });
                            }
                        }
                    }
                }
            }
        }
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
        &storage_path,
        &installer_json,
        Some(&commit_id),
    ).await?;

    // 如果是非 macOS 的多架构子任务且所有兄弟子任务均已成功，同步将父任务标记为成功
    if let Ok(task) = task_repo.find_by_id(task_id).await {
        if let Some(parent_id) = task.parent_id {
            if request.platform != "macos" {
                if let Ok(children) = task_repo.get_child_tasks(parent_id).await {
                    if !children.is_empty() && children.iter().all(|c| c.state == crate::model::state::TaskState::Success) {
                        let _ = task_repo.update_completion(
                            parent_id,
                            &end_time,
                            &storage_path,
                            &installer_json,
                            Some(&commit_id),
                        ).await;
                    }
                }
            }
        }
    }
    
    tracing::info!("🎉 =========================================");
    tracing::info!("🎉 构建任务 #{} 完成！", task_id);
    tracing::info!("🎉 =========================================");
    tracing::info!("⏱️  总耗时: {:.2} 秒 ({:.2} 分钟)", 
        total_duration.as_secs_f64(),
        total_duration.as_secs_f64() / 60.0);
    tracing::info!("📅 完成时间: {}", end_time);
    tracing::info!("═══════════════════════════════════════════════════════\n");
    
    // 发送邮件通知（如果有邮箱列表）
    if let Some(emails) = &request.emails {
        if !emails.is_empty() {
            let emails_str = emails.join(",");
            if let Err(e) = email_sender.send_notification(
                task_id,
                &request,
                Some(&emails_str),
            ).await {
                tracing::warn!("Failed to send email: {:?}", e);
            }
        }
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

