use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskState {
    #[serde(rename = "pending")]
    Pending,
    
    #[serde(rename = "checkout...")]
    CheckingOut,
    
    #[serde(rename = "start build")]
    StartBuild,
    
    #[serde(rename = "clean...")]
    Cleaning,
    
    #[serde(rename = "gen project")]
    GeneratingProject,
    
    #[serde(rename = "build pre_build")]
    BuildingPreBuild,
    
    #[serde(rename = "build base")]
    BuildingBase,
    
    #[serde(rename = "build chrome")]
    BuildingChrome,
    
    #[serde(rename = "combining")]
    Combining,  // 正在组合多个架构
    
    #[serde(rename = "build installer")]
    BuildingInstaller,
    
    #[serde(rename = "sign")]
    Signing,
    
    #[serde(rename = "backup")]
    BackingUp,
    
    #[serde(rename = "success")]
    Success,
    
    #[serde(rename = "failed")]
    Failed,
    
    #[serde(rename = "cancelled")]
    Cancelled,
}

impl TaskState {
    #[allow(dead_code)]
    pub fn is_terminal(&self) -> bool {
        matches!(self, TaskState::Success | TaskState::Failed | TaskState::Cancelled)
    }
    
    #[allow(dead_code)]
    pub fn can_transition_to(&self, next: TaskState) -> bool {
        // 简化的状态转换规则
        match (self, next) {
            (TaskState::Pending, _) => true,
            (TaskState::CheckingOut, TaskState::StartBuild) => true,
            (TaskState::StartBuild, TaskState::Cleaning) => true,
            (TaskState::Cleaning, TaskState::GeneratingProject) => true,
            (TaskState::GeneratingProject, TaskState::BuildingPreBuild) => true,
            (TaskState::BuildingPreBuild, TaskState::BuildingBase) => true,
            (TaskState::BuildingBase, TaskState::BuildingChrome) => true,
            (TaskState::BuildingChrome, TaskState::Combining) => true,
            (TaskState::BuildingChrome, TaskState::BuildingInstaller) => true,
            (TaskState::Combining, TaskState::BuildingInstaller) => true,
            (TaskState::BuildingInstaller, TaskState::Signing) => true,
            (TaskState::Signing, TaskState::BackingUp) => true,
            (TaskState::BackingUp, TaskState::Success) => true,
            (_, TaskState::Failed) => true,  // 任何状态都可以转换到失败
            (_, TaskState::Cancelled) => true,  // 任何状态都可以转换到取消
            _ => false,
        }
    }
}

impl TaskState {
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskState::Pending => "pending",
            TaskState::CheckingOut => "checkout...",
            TaskState::StartBuild => "start build",
            TaskState::Cleaning => "clean...",
            TaskState::GeneratingProject => "gen project",
            TaskState::BuildingPreBuild => "build pre_build",
            TaskState::BuildingBase => "build base",
            TaskState::BuildingChrome => "build chrome",
            TaskState::Combining => "combining",
            TaskState::BuildingInstaller => "build installer",
            TaskState::Signing => "sign",
            TaskState::BackingUp => "backup",
            TaskState::Success => "success",
            TaskState::Failed => "failed",
            TaskState::Cancelled => "cancelled",
        }
    }
    
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(TaskState::Pending),
            "checkout..." => Some(TaskState::CheckingOut),
            "start build" => Some(TaskState::StartBuild),
            "clean..." => Some(TaskState::Cleaning),
            "gen project" => Some(TaskState::GeneratingProject),
            "build pre_build" => Some(TaskState::BuildingPreBuild),
            "build base" => Some(TaskState::BuildingBase),
            "build chrome" => Some(TaskState::BuildingChrome),
            "combining" => Some(TaskState::Combining),
            "build installer" => Some(TaskState::BuildingInstaller),
            "sign" => Some(TaskState::Signing),
            "backup" => Some(TaskState::BackingUp),
            "success" => Some(TaskState::Success),
            "failed" => Some(TaskState::Failed),
            "cancelled" => Some(TaskState::Cancelled),
            _ => None,
        }
    }
}

