use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use db::models::execution_process::{ExecutionContext, ExecutionProcessStatus};
use utils;

use crate::services::config::SoundFile;

/// Service for handling cross-platform notifications including sound alerts and push notifications
#[derive(Debug, Clone)]
pub struct NotificationService {}
use crate::services::config::NotificationConfig;

/// Cache for WSL root path from PowerShell
static WSL_ROOT_PATH_CACHE: OnceLock<Option<String>> = OnceLock::new();

/// Cache for DBus availability check on Linux
static DBUS_AVAILABLE: AtomicBool = AtomicBool::new(true);
static DBUS_CHECK_DONE: AtomicBool = AtomicBool::new(false);

impl NotificationService {
    pub async fn notify_execution_halted(mut config: NotificationConfig, ctx: &ExecutionContext) {
        // If the process was intentionally killed by user, suppress sound
        if matches!(ctx.execution_process.status, ExecutionProcessStatus::Killed) {
            config.sound_enabled = false;
        }

        let title = format!("Task Complete: {}", ctx.task.title);
        let message = match ctx.execution_process.status {
            ExecutionProcessStatus::Completed => format!(
                "✅ '{}' completed successfully\nBranch: {:?}\nExecutor: {}",
                ctx.task.title, ctx.task_attempt.branch, ctx.task_attempt.executor
            ),
            ExecutionProcessStatus::Failed => format!(
                "❌ '{}' execution failed\nBranch: {:?}\nExecutor: {}",
                ctx.task.title, ctx.task_attempt.branch, ctx.task_attempt.executor
            ),
            ExecutionProcessStatus::Killed => format!(
                "🛑 '{}' execution cancelled by user\nBranch: {:?}\nExecutor: {}",
                ctx.task.title, ctx.task_attempt.branch, ctx.task_attempt.executor
            ),
            _ => {
                tracing::warn!(
                    "Tried to notify attempt completion for {} but process is still running!",
                    ctx.task_attempt.id
                );
                return;
            }
        };
        Self::notify(config, &title, &message).await;
    }

    /// Send both sound and push notifications if enabled
    pub async fn notify(config: NotificationConfig, title: &str, message: &str) {
        if config.sound_enabled {
            Self::play_sound_notification(&config.sound_file).await;
        }

        if config.push_enabled {
            Self::send_push_notification(title, message).await;
        }
    }

    /// Play a system sound notification across platforms
    async fn play_sound_notification(sound_file: &SoundFile) {
        let file_path = match sound_file.get_path().await {
            Ok(path) => path,
            Err(e) => {
                tracing::error!("Failed to create cached sound file: {}", e);
                return;
            }
        };

        // Use platform-specific sound notification
        // Note: spawn() calls are intentionally not awaited - sound notifications should be fire-and-forget
        if cfg!(target_os = "macos") {
            let _ = tokio::process::Command::new("afplay")
                .arg(&file_path)
                .spawn();
        } else if cfg!(target_os = "linux") && !utils::is_wsl2() {
            // Try different Linux audio players
            if tokio::process::Command::new("paplay")
                .arg(&file_path)
                .spawn()
                .is_ok()
            {
                // Success with paplay
            } else if tokio::process::Command::new("aplay")
                .arg(&file_path)
                .spawn()
                .is_ok()
            {
                // Success with aplay
            } else {
                // Try system bell as fallback
                let _ = tokio::process::Command::new("echo")
                    .arg("-e")
                    .arg("\\a")
                    .spawn();
            }
        } else if cfg!(target_os = "windows") || (cfg!(target_os = "linux") && utils::is_wsl2()) {
            // Convert WSL path to Windows path if in WSL2
            let file_path = if utils::is_wsl2() {
                if let Some(windows_path) = Self::wsl_to_windows_path(&file_path).await {
                    windows_path
                } else {
                    file_path.to_string_lossy().to_string()
                }
            } else {
                file_path.to_string_lossy().to_string()
            };

            let _ = tokio::process::Command::new("powershell.exe")
                .arg("-c")
                .arg(format!(
                    r#"(New-Object Media.SoundPlayer "{file_path}").PlaySync()"#
                ))
                .spawn();
        }
    }

    /// Send a cross-platform push notification
    async fn send_push_notification(title: &str, message: &str) {
        if cfg!(target_os = "macos") {
            Self::send_macos_notification(title, message).await;
        } else if cfg!(target_os = "linux") && !utils::is_wsl2() {
            Self::send_linux_notification(title, message).await;
        } else if cfg!(target_os = "windows") || (cfg!(target_os = "linux") && utils::is_wsl2()) {
            Self::send_windows_notification(title, message).await;
        }
    }

    /// Send macOS notification using osascript
    async fn send_macos_notification(title: &str, message: &str) {
        let script = format!(
            r#"display notification "{message}" with title "{title}" sound name "Glass""#,
            message = message.replace('"', r#"\""#),
            title = title.replace('"', r#"\""#)
        );

        let _ = tokio::process::Command::new("osascript")
            .arg("-e")
            .arg(script)
            .spawn();
    }

    /// Check if DBus is available on Linux (cached check)
    async fn check_dbus_available() -> bool {
        // Return cached result if already checked
        if DBUS_CHECK_DONE.load(Ordering::Relaxed) {
            return DBUS_AVAILABLE.load(Ordering::Relaxed);
        }
        
        // Check if DISABLE_DBUS_NOTIFICATIONS is set
        if std::env::var("DISABLE_DBUS_NOTIFICATIONS").is_ok() {
            tracing::info!("DBus notifications disabled via DISABLE_DBUS_NOTIFICATIONS environment variable");
            DBUS_AVAILABLE.store(false, Ordering::Relaxed);
            DBUS_CHECK_DONE.store(true, Ordering::Relaxed);
            return false;
        }
        
        // Try a quick DBus availability check with timeout
        let check_result = tokio::time::timeout(
            Duration::from_millis(500),
            tokio::task::spawn_blocking(|| {
                // Simple check: try to connect to session bus
                match std::process::Command::new("dbus-send")
                    .args(&[
                        "--session",
                        "--dest=org.freedesktop.DBus",
                        "--type=method_call",
                        "--print-reply",
                        "/org/freedesktop/DBus",
                        "org.freedesktop.DBus.GetId",
                    ])
                    .output()
                {
                    Ok(output) => output.status.success(),
                    Err(_) => false,
                }
            })
        ).await;
        
        let is_available = match check_result {
            Ok(Ok(available)) => available,
            Ok(Err(e)) => {
                tracing::warn!("DBus check task failed: {}", e);
                false
            }
            Err(_) => {
                tracing::warn!("DBus check timed out - assuming unavailable");
                false
            }
        };
        
        DBUS_AVAILABLE.store(is_available, Ordering::Relaxed);
        DBUS_CHECK_DONE.store(true, Ordering::Relaxed);
        
        if !is_available {
            tracing::info!("DBus not available - Linux desktop notifications will be skipped");
        }
        
        is_available
    }

    /// Send Linux notification using notify-rust
    async fn send_linux_notification(title: &str, message: &str) {
        // Skip if DBus is not available
        if !Self::check_dbus_available().await {
            tracing::debug!("Skipping Linux notification - DBus not available");
            return;
        }
        
        use notify_rust::Notification;

        let title = title.to_string();
        let message = message.to_string();

        // Add timeout to prevent indefinite blocking
        let notification_result = tokio::time::timeout(
            Duration::from_secs(2),
            tokio::task::spawn_blocking(move || {
                if let Err(e) = Notification::new()
                    .summary(&title)
                    .body(&message)
                    .timeout(10000)
                    .show()
                {
                    tracing::error!("Failed to send Linux notification: {}", e);
                    
                    // If we get a DBus error, mark it as unavailable for future calls
                    if e.to_string().contains("DBus") || e.to_string().contains("D-Bus") {
                        DBUS_AVAILABLE.store(false, Ordering::Relaxed);
                        tracing::info!("DBus appears to be unavailable - disabling future notification attempts");
                    }
                }
            })
        ).await;
        
        match notification_result {
            Ok(Ok(_)) => {
                // Success
            }
            Ok(Err(e)) => {
                tracing::error!("Notification task panicked: {}", e);
                DBUS_AVAILABLE.store(false, Ordering::Relaxed);
            }
            Err(_) => {
                tracing::error!("Linux notification timed out after 2 seconds - possible DBus deadlock");
                DBUS_AVAILABLE.store(false, Ordering::Relaxed);
            }
        }
    }

    /// Send Windows/WSL notification using PowerShell toast script
    async fn send_windows_notification(title: &str, message: &str) {
        let script_path = match utils::get_powershell_script().await {
            Ok(path) => path,
            Err(e) => {
                tracing::error!("Failed to get PowerShell script: {}", e);
                return;
            }
        };

        // Convert WSL path to Windows path if in WSL2
        let script_path_str = if utils::is_wsl2() {
            if let Some(windows_path) = Self::wsl_to_windows_path(&script_path).await {
                windows_path
            } else {
                script_path.to_string_lossy().to_string()
            }
        } else {
            script_path.to_string_lossy().to_string()
        };

        let _ = tokio::process::Command::new("powershell.exe")
            .arg("-NoProfile")
            .arg("-ExecutionPolicy")
            .arg("Bypass")
            .arg("-File")
            .arg(script_path_str)
            .arg("-Title")
            .arg(title)
            .arg("-Message")
            .arg(message)
            .spawn();
    }

    /// Get WSL root path via PowerShell (cached)
    async fn get_wsl_root_path() -> Option<String> {
        if let Some(cached) = WSL_ROOT_PATH_CACHE.get() {
            return cached.clone();
        }

        match tokio::process::Command::new("powershell.exe")
            .arg("-c")
            .arg("(Get-Location).Path -replace '^.*::', ''")
            .current_dir("/")
            .output()
            .await
        {
            Ok(output) => {
                match String::from_utf8(output.stdout) {
                    Ok(pwd_str) => {
                        let pwd = pwd_str.trim();
                        tracing::info!("WSL root path detected: {}", pwd);

                        // Cache the result
                        let _ = WSL_ROOT_PATH_CACHE.set(Some(pwd.to_string()));
                        return Some(pwd.to_string());
                    }
                    Err(e) => {
                        tracing::error!("Failed to parse PowerShell pwd output as UTF-8: {}", e);
                    }
                }
            }
            Err(e) => {
                tracing::error!("Failed to execute PowerShell pwd command: {}", e);
            }
        }

        // Cache the failure result
        let _ = WSL_ROOT_PATH_CACHE.set(None);
        None
    }

    /// Convert WSL path to Windows UNC path for PowerShell
    async fn wsl_to_windows_path(wsl_path: &std::path::Path) -> Option<String> {
        let path_str = wsl_path.to_string_lossy();

        // Relative paths work fine as-is in PowerShell
        if !path_str.starts_with('/') {
            tracing::debug!("Using relative path as-is: {}", path_str);
            return Some(path_str.to_string());
        }

        // Get cached WSL root path from PowerShell
        if let Some(wsl_root) = Self::get_wsl_root_path().await {
            // Simply concatenate WSL root with the absolute path - PowerShell doesn't mind /
            let windows_path = format!("{wsl_root}{path_str}");
            tracing::debug!("WSL path converted: {} -> {}", path_str, windows_path);
            Some(windows_path)
        } else {
            tracing::error!(
                "Failed to determine WSL root path for conversion: {}",
                path_str
            );
            None
        }
    }
}
