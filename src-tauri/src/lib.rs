mod backup;
mod export_staging;
mod export_txn;
mod file_store;
mod restore;
mod session;
mod vault;

use std::path::PathBuf;
use std::time::Duration;

use backup::BackupSummary;
use file_store::{
    DeletedFile, ExportedFile, FileListItem, FolderItem, ImportSummary, VerifyReport,
};
use restore::RestoreSummary;
use session::{AppState, ExitPhase, VaultStatus};
use tauri::{Emitter, Manager};
use vault::VaultError;

const IMPORT_PROGRESS_EVENT: &str = "haven://import-progress";
const BACKUP_PROGRESS_EVENT: &str = "haven://backup-progress";
const RESTORE_PROGRESS_EVENT: &str = "haven://restore-progress";

#[tauri::command]
fn vault_status(state: tauri::State<'_, AppState>) -> VaultStatus {
    state.status()
}

#[tauri::command]
async fn create_vault(
    state: tauri::State<'_, AppState>,
    parent_dir: String,
    password: String,
) -> Result<VaultStatus, VaultError> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        state.create_vault(PathBuf::from(parent_dir), password)
    })
    .await
    .map_err(|_| VaultError::internal())?
}

#[tauri::command]
fn open_vault(state: tauri::State<'_, AppState>, dir: String) -> Result<VaultStatus, VaultError> {
    state.open_vault(PathBuf::from(dir))
}

#[tauri::command]
async fn unlock_vault(
    state: tauri::State<'_, AppState>,
    password: String,
) -> Result<VaultStatus, VaultError> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || state.unlock_vault(password))
        .await
        .map_err(|_| VaultError::internal())?
}

#[tauri::command]
async fn lock_vault(state: tauri::State<'_, AppState>) -> Result<VaultStatus, VaultError> {
    // 锁定需要等待进行中的任务退出，放到阻塞线程执行，避免占用主线程
    // （导入任务通过主线程派发进度事件，阻塞主线程会与锁定相互等待）。
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || state.lock())
        .await
        .map_err(|_| VaultError::internal())?
}

#[tauri::command]
async fn import_files(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    paths: Vec<String>,
) -> Result<ImportSummary, VaultError> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        state.import_files(paths, &|progress| {
            // 事件只包含当前文件名与阶段，用于真实进度展示，不写入日志。
            let _ = app.emit(IMPORT_PROGRESS_EVENT, progress);
        })
    })
    .await
    .map_err(|_| VaultError::internal())?
}

#[tauri::command]
fn cancel_import(state: tauri::State<'_, AppState>) {
    state.request_cancel();
}

#[tauri::command]
async fn list_files(state: tauri::State<'_, AppState>) -> Result<Vec<FileListItem>, VaultError> {
    // 索引解密与 JSON 解析可能较大，放到阻塞线程，避免占用主线程。
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || state.list_files())
        .await
        .map_err(|_| VaultError::internal())?
}

#[tauri::command]
async fn verify_files(state: tauri::State<'_, AppState>) -> Result<VerifyReport, VaultError> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || state.verify_files())
        .await
        .map_err(|_| VaultError::internal())?
}

/// FR-44：备份只复制加密数据，进度事件不含文件名或任何明文内容。
#[tauri::command]
async fn backup_vault(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    target_dir: String,
) -> Result<BackupSummary, VaultError> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        state.backup_vault(PathBuf::from(target_dir), &|progress| {
            let _ = app.emit(BACKUP_PROGRESS_EVENT, progress);
        })
    })
    .await
    .map_err(|_| VaultError::internal())?
}

/// FR-46：验证备份目录的主密码；只读操作，不创建任何目录或文件。
#[tauri::command]
async fn verify_backup_password(
    state: tauri::State<'_, AppState>,
    backup_dir: String,
    password: String,
) -> Result<(), VaultError> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        state.verify_backup_password(PathBuf::from(backup_dir), password)
    })
    .await
    .map_err(|_| VaultError::internal())?
}

/// FR-45/FR-46：恢复只复制备份中的加密数据；进度事件不含文件名或任何明文内容。
/// 密码只经内存传给备份自身的验证逻辑，不落盘、不写日志。
#[tauri::command]
async fn restore_vault(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    backup_dir: String,
    target_dir: String,
    password: String,
) -> Result<RestoreSummary, VaultError> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        state.restore_vault(
            PathBuf::from(backup_dir),
            PathBuf::from(target_dir),
            password,
            &|progress| {
                let _ = app.emit(RESTORE_PROGRESS_EVENT, progress);
            },
        )
    })
    .await
    .map_err(|_| VaultError::internal())?
}

/// 返回原始字节而不是 JSON 数组，避免大图片经 JSON 序列化的额外内存与开销。
#[tauri::command]
async fn preview_image(
    state: tauri::State<'_, AppState>,
    id: String,
) -> Result<tauri::ipc::Response, VaultError> {
    let state = state.inner().clone();
    let bytes = tauri::async_runtime::spawn_blocking(move || state.preview_image(id))
        .await
        .map_err(|_| VaultError::internal())??;
    Ok(tauri::ipc::Response::new(bytes))
}

/// FR-34：与图片预览一致，返回原始字节，避免大 PDF 经 JSON 序列化。
#[tauri::command]
async fn preview_pdf(
    state: tauri::State<'_, AppState>,
    id: String,
) -> Result<tauri::ipc::Response, VaultError> {
    let state = state.inner().clone();
    let bytes = tauri::async_runtime::spawn_blocking(move || state.preview_pdf(id))
        .await
        .map_err(|_| VaultError::internal())??;
    Ok(tauri::ipc::Response::new(bytes))
}

#[tauri::command]
async fn preview_video(
    state: tauri::State<'_, AppState>,
    id: String,
) -> Result<tauri::ipc::Response, VaultError> {
    let state = state.inner().clone();
    let bytes = tauri::async_runtime::spawn_blocking(move || state.preview_video(id))
        .await
        .map_err(|_| VaultError::internal())??;
    Ok(tauri::ipc::Response::new(bytes))
}

#[tauri::command]
async fn preview_thumbnail(
    state: tauri::State<'_, AppState>,
    id: String,
) -> Result<tauri::ipc::Response, VaultError> {
    let state = state.inner().clone();
    let bytes = tauri::async_runtime::spawn_blocking(move || state.preview_thumbnail(id))
        .await
        .map_err(|_| VaultError::internal())??;
    Ok(tauri::ipc::Response::new(bytes))
}

#[tauri::command]
async fn delete_file(
    state: tauri::State<'_, AppState>,
    id: String,
) -> Result<DeletedFile, VaultError> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || state.delete_file(id))
        .await
        .map_err(|_| VaultError::internal())?
}

/// FR-32：只修改索引中的显示名称，不移动或重新加密密文。
#[tauri::command]
async fn set_favorite(
    state: tauri::State<'_, AppState>,
    id: String,
    favorite: bool,
) -> Result<FileListItem, VaultError> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || state.set_favorite(id, favorite))
        .await
        .map_err(|_| VaultError::internal())?
}

#[tauri::command]
async fn rename_file(
    state: tauri::State<'_, AppState>,
    id: String,
    name: String,
) -> Result<FileListItem, VaultError> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || state.rename_file(id, name))
        .await
        .map_err(|_| VaultError::internal())?
}

#[tauri::command]
async fn list_folders(state: tauri::State<'_, AppState>) -> Result<Vec<FolderItem>, VaultError> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || state.list_folders())
        .await
        .map_err(|_| VaultError::internal())?
}

#[tauri::command]
async fn create_folder(
    state: tauri::State<'_, AppState>,
    name: String,
) -> Result<FolderItem, VaultError> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || state.create_folder(name))
        .await
        .map_err(|_| VaultError::internal())?
}

#[tauri::command]
async fn rename_folder(
    state: tauri::State<'_, AppState>,
    id: String,
    name: String,
) -> Result<FolderItem, VaultError> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || state.rename_folder(id, name))
        .await
        .map_err(|_| VaultError::internal())?
}

#[tauri::command]
async fn delete_folder(state: tauri::State<'_, AppState>, id: String) -> Result<(), VaultError> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || state.delete_folder(id))
        .await
        .map_err(|_| VaultError::internal())?
}

#[tauri::command]
async fn move_files(
    state: tauri::State<'_, AppState>,
    ids: Vec<String>,
    folder_id: Option<String>,
) -> Result<Vec<FileListItem>, VaultError> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || state.move_files(ids, folder_id))
        .await
        .map_err(|_| VaultError::internal())?
}

/// 应用自身的配置/数据/缓存目录：任何情况下都不得把导出明文写入其中。
fn app_protected_dirs(app: &tauri::AppHandle) -> Vec<PathBuf> {
    let resolver = app.path();
    [
        resolver.app_config_dir(),
        resolver.app_data_dir(),
        resolver.app_cache_dir(),
        resolver.app_local_data_dir(),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// 导出单个文件到用户通过系统保存对话框选定的路径。
///
/// 返回值只有名称与大小；明文由 Rust 直接写入目标目录，不经过 IPC。
#[tauri::command]
async fn export_file(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    id: String,
    target_path: String,
) -> Result<ExportedFile, VaultError> {
    let state = state.inner().clone();
    let forbidden = app_protected_dirs(&app);
    tauri::async_runtime::spawn_blocking(move || {
        state.export_file(id, PathBuf::from(target_path), forbidden)
    })
    .await
    .map_err(|_| VaultError::internal())?
}

/// 安装显式菜单：macOS 默认菜单的“退出”项会直接终止进程，绕过退出状态机。
/// 这里用自定义菜单项接管 ⌘Q，使其与关闭窗口共用同一套幂等退出流程。
fn install_menu(app: &tauri::AppHandle) -> tauri::Result<()> {
    use tauri::menu::{MenuBuilder, MenuItemBuilder, PredefinedMenuItem, SubmenuBuilder};

    let quit = MenuItemBuilder::with_id("haven-quit", "退出 Haven")
        .accelerator("CmdOrCtrl+Q")
        .build(app)?;
    let app_menu = SubmenuBuilder::new(app, "Haven")
        .item(&PredefinedMenuItem::about(app, None, None)?)
        .separator()
        .item(&PredefinedMenuItem::hide(app, None)?)
        .item(&PredefinedMenuItem::hide_others(app, None)?)
        .item(&PredefinedMenuItem::show_all(app, None)?)
        .separator()
        .item(&quit)
        .build()?;
    let edit_menu = SubmenuBuilder::new(app, "编辑")
        .item(&PredefinedMenuItem::undo(app, None)?)
        .item(&PredefinedMenuItem::redo(app, None)?)
        .separator()
        .item(&PredefinedMenuItem::cut(app, None)?)
        .item(&PredefinedMenuItem::copy(app, None)?)
        .item(&PredefinedMenuItem::paste(app, None)?)
        .item(&PredefinedMenuItem::select_all(app, None)?)
        .build()?;
    let window_menu = SubmenuBuilder::new(app, "窗口")
        .item(&PredefinedMenuItem::minimize(app, None)?)
        .item(&PredefinedMenuItem::maximize(app, None)?)
        .separator()
        .item(&PredefinedMenuItem::close_window(app, None)?)
        .build()?;
    let menu = MenuBuilder::new(app)
        .items(&[&app_menu, &edit_menu, &window_menu])
        .build()?;
    app.set_menu(menu)?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .on_menu_event(|app, event| {
            if event.id() == "haven-quit" {
                let state = app.state::<AppState>();
                if state.exit_phase() == ExitPhase::Running {
                    let handle = app.clone();
                    state.begin_exit(Duration::from_secs(2), move || handle.exit(0));
                }
            }
        })
        .setup(|app| {
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }

            let config_dir = app.path().app_config_dir()?;
            let app_cache_dir = app.path().app_cache_dir()?;
            // 缓存目录创建失败不应阻止应用启动；导出会在解析暂存目录时失败关闭。
            let _ = std::fs::create_dir_all(&app_cache_dir);
            let state = AppState::bootstrap(config_dir.join(session::CONFIG_FILE), app_cache_dir);
            app.manage(state.clone());
            install_menu(app.handle())?;
            // 启动恢复在后台执行，不阻塞窗口显示；进度不可取消，失败静默。
            std::thread::spawn(move || state.recover_exports());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            vault_status,
            create_vault,
            open_vault,
            unlock_vault,
            lock_vault,
            import_files,
            cancel_import,
            list_files,
            verify_files,
            backup_vault,
            restore_vault,
            verify_backup_password,
            preview_image,
            preview_thumbnail,
            preview_video,
            preview_pdf,
            export_file,
            delete_file,
            set_favorite,
            rename_file,
            list_folders,
            create_folder,
            rename_folder,
            delete_folder,
            move_files
        ])
        .on_window_event(|window, event| {
            // PRD Q-05：关闭主窗口即退出应用进程。
            // CloseRequested 与 ExitRequested 共享幂等状态机；这里只启动一次。
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let app = window.app_handle();
                let state = app.state::<AppState>();
                match state.exit_phase() {
                    ExitPhase::Done => {}
                    _ => {
                        api.prevent_close();
                        let handle = app.clone();
                        state.begin_exit(Duration::from_secs(2), move || handle.exit(0));
                    }
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| match event {
            tauri::RunEvent::ExitRequested { api, .. } => {
                // ⌘Q 与关闭窗口走同一路径；重复事件不会重新开启等待。
                let state = app_handle.state::<AppState>();
                match state.exit_phase() {
                    ExitPhase::Running => {
                        api.prevent_exit();
                        let handle = app_handle.clone();
                        state.begin_exit(Duration::from_secs(2), move || handle.exit(0));
                    }
                    ExitPhase::Stopping => api.prevent_exit(),
                    ExitPhase::Done => {}
                }
            }
            tauri::RunEvent::Exit => {
                // 兜底：无论退出流程是否完成，都清理 Rust 侧主密钥。
                app_handle.state::<AppState>().shutdown();
            }
            _ => {}
        });
}
