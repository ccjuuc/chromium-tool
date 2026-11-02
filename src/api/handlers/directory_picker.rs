use std::path::PathBuf;

/// The web server does not run an AppKit event loop. On macOS, rfd cannot open
/// a dialog from a Tokio worker thread, so give the native picker its own process.
#[cfg(target_os = "macos")]
const FOLDER_PICKER_SCRIPT: &str = r#"
on run argv
    try
        activate
        if (count of argv) > 0 then
            set chosenFolder to choose folder with prompt "选择 .icon 工作目录" default location (POSIX file (item 1 of argv))
        else
            set chosenFolder to choose folder with prompt "选择 .icon 工作目录"
        end if
        return POSIX path of chosenFolder
    on error number -128
        return ""
    end try
end run
"#;

pub(super) async fn pick_directory(initial: Option<String>) -> Result<Option<PathBuf>, String> {
    let initial = initial.filter(|path| std::path::Path::new(path).is_dir());
    pick_directory_native(initial).await
}

#[cfg(target_os = "macos")]
async fn pick_directory_native(initial: Option<String>) -> Result<Option<PathBuf>, String> {
    let mut command = tokio::process::Command::new("/usr/bin/osascript");
    command
        .arg("-e")
        .arg(FOLDER_PICKER_SCRIPT)
        .arg("--")
        .kill_on_drop(true);
    // Paths are argv values, never interpolated into AppleScript or a shell.
    if let Some(path) = initial {
        command.arg(path);
    }
    let output = command
        .output()
        .await
        .map_err(|error| format!("无法启动 macOS 目录选择器: {}", error))?;
    if !output.status.success() {
        return Err(format!(
            "macOS 目录选择器失败 ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    parse_macos_selection(&output.stdout)
}

#[cfg(target_os = "macos")]
fn parse_macos_selection(stdout: &[u8]) -> Result<Option<PathBuf>, String> {
    let value = std::str::from_utf8(stdout)
        .map_err(|error| format!("macOS 目录选择器返回了无效路径: {}", error))?;
    // osascript appends one newline. Do not trim whitespace from directory names.
    let value = value.strip_suffix('\n').unwrap_or(value);
    if value.is_empty() {
        Ok(None)
    } else {
        Ok(Some(PathBuf::from(value)))
    }
}

#[cfg(not(target_os = "macos"))]
async fn pick_directory_native(initial: Option<String>) -> Result<Option<PathBuf>, String> {
    tokio::task::spawn_blocking(move || {
        let mut dialog = rfd::FileDialog::new().set_title("选择 .icon 工作目录");
        if let Some(path) = initial {
            dialog = dialog.set_directory(path);
        }
        dialog.pick_folder()
    })
    .await
    .map_err(|error| format!("目录选择器启动失败: {}", error))
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::parse_macos_selection;
    use std::path::PathBuf;

    #[test]
    fn cancellation_has_no_selection() {
        assert_eq!(parse_macos_selection(b"\n").unwrap(), None);
    }

    #[test]
    fn preserves_spaces_quotes_and_newlines_in_directory_names() {
        let path = "/tmp/图标 'quoted' \\\nfolder /";
        assert_eq!(
            parse_macos_selection(format!("{}\n", path).as_bytes()).unwrap(),
            Some(PathBuf::from(path))
        );
    }

    #[test]
    fn invalid_utf8_is_an_error() {
        assert!(parse_macos_selection(&[0xff, b'\n']).is_err());
    }
}
