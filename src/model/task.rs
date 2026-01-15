use serde::{Deserialize, Serialize};
use crate::model::state::TaskState;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: i64,
    pub start_time: String,
    pub end_time: Option<String>,
    pub branch_name: String,
    pub oem_name: String,
    pub commit_id: String,
    pub pkg_flag: String,
    pub is_signed: bool,
    pub is_increment: bool,
    pub storage_path: String,
    pub installer: String,
    pub state: TaskState,
    pub server: String,
    #[serde(default)]
    pub parent_id: Option<i64>,  // 父任务ID，None表示是父任务
    #[serde(default)]
    pub architecture: Option<String>,  // 架构信息
    #[serde(default)]
    pub build_log: Option<String>,  // 构建日志
    #[serde(default)]
    pub installer_format: Option<String>,  // 安装包格式：dmg 或 pkg
}

#[derive(Debug, Deserialize)]
pub struct CreateTask {
    pub branch: String,
    pub oem_name: String,
    pub commit_id: String,
    pub pkg_flag: String,
    pub is_increment: bool,
    pub is_signed: bool,
    pub server: String,
    pub parent_id: Option<i64>,  // 父任务ID
    pub architecture: Option<String>,  // 架构信息
    pub installer_format: Option<String>,  // 安装包格式：dmg 或 pkg
}

#[derive(Debug, Deserialize)]
pub struct UpdateTask {
    pub id: i64,
    pub commit_id: Option<String>,
    pub end_time: Option<String>,
    pub storage_path: Option<String>,
    pub installer: Option<String>,
    pub state: Option<TaskState>,
}

#[derive(Debug, Deserialize)]
pub struct DeleteTask {
    pub task_id: i64,
}

impl Default for Task {
    fn default() -> Self {
        Self {
            id: 0,
            start_time: String::new(),
            end_time: None,
            branch_name: String::new(),
            oem_name: String::new(),
            commit_id: String::new(),
            pkg_flag: String::new(),
            is_signed: false,
            is_increment: false,
            storage_path: String::new(),
            installer: String::new(),
            state: TaskState::Pending,
            server: String::new(),
            parent_id: None,
            architecture: None,
            build_log: None,
            installer_format: None,
        }
    }
}

