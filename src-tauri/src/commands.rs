//! Tauri commands：前端调用的薄包装，对应 Python 版 HTTP API。
//!
//! 阶段 1 覆盖：get_status / get_accounts / delete_account / oauth_start /
//! oauth_status / import_local。

use serde::Serialize;
use serde_json::{json, Value};

use tauri::Emitter;
use wb_switch_core::modules::{
    account, auth_file, checkin, codebuddy_cli, codebuddy_cn_ide, codebuddy_ide,
    codebuddy_ide_session, codebuddy_ide_session_sync, credit_usage, credits, error_log,
    export_import, jetbrains, limits, notifications, oauth, process, rate_limit_events,
    rate_limit_hook, refresh, rotate, session, switch, token_stats, travel, update,
    variant::WbVariant, vscode_ext, vscode_session, vscode_session_sync,
};

#[derive(Serialize)]
pub struct AppStatus {
    running: bool,
    auth_file: String,
    current: Option<Value>,
    app_path: String,
    version: String,
    variant: String,
}

/// GET /api/status —— WorkBuddy 运行状态 + 当前账号。
///
/// `variant` 缺省国内版：不传参数时行为与改造前逐字一致（只多返回 `variant` 字段）。
#[tauri::command]
pub async fn get_status(variant: Option<String>) -> Result<AppStatus, String> {
    let variant = WbVariant::parse(variant.as_deref());
    // Windows 的运行状态检测会启动 tasklist 子进程。同步 command 默认在
    // Tauri 主线程执行，标题栏拖拽期间一旦焦点事件触发状态刷新，就会阻塞
    // 原生窗口消息循环。放入 blocking 线程，保持窗口移动与 IPC 查询解耦。
    tauri::async_runtime::spawn_blocking(move || build_app_status(variant))
        .await
        .map_err(|error| format!("查询应用状态失败: {error}"))
}

fn build_app_status(variant: WbVariant) -> AppStatus {
    let auth = auth_file::read_auth_file(variant);
    let current = auth.as_ref().map(|a| {
        let acct = a.get("account").cloned().unwrap_or_else(|| json!({}));
        json!({
            "uid": account::display_value(&acct, "uid"),
            "nickname": account::display_value(&acct, "nickname"),
            "email": account::display_value(&acct, "email"),
        })
    });
    AppStatus {
        running: process::is_workbuddy_running(variant),
        auth_file: auth_file::auth_file_path(variant)
            .to_string_lossy()
            .to_string(),
        current,
        app_path: auth_file::workbuddy_app_path(variant)
            .to_string_lossy()
            .to_string(),
        version: update::APP_VERSION.to_string(),
        variant: variant.as_str().to_string(),
    }
}

/// GET /api/accounts —— 账号列表（account_meta，不含 token）。
///
/// 返回全部档位的账号；每行 meta 自带 `variant`，由前端按当前档位过滤。
#[tauri::command]
pub fn get_accounts() -> Value {
    let metas: Vec<Value> = account::load_accounts()
        .iter()
        .map(account::account_meta)
        .collect();
    json!({ "accounts": metas })
}

/// GET /api/codebuddy-cli/status —— CodeBuddy CLI helper 轮换状态（不含 token）。
///
/// async + spawn_blocking：状态检测可能执行 ps / helper 定位等子进程，
/// 避免在账号页挂载刷新时阻塞主线程造成页面卡顿。
#[tauri::command]
pub async fn get_codebuddy_cli_status() -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(codebuddy_cli::status)
        .await
        .map_err(|error| format!("查询 CodeBuddy CLI 状态失败: {error}"))
}

/// POST /api/codebuddy-cli/install-helper —— 显式安装/升级 CLI helper。
#[tauri::command]
pub async fn install_codebuddy_cli_helper() -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(codebuddy_cli::install_helper)
        .await
        .map_err(|e| e.to_string())?
}

/// POST /api/codebuddy-cli/switch —— 只切换 CodeBuddy CLI，不重启 WorkBuddy。
///
/// 任何切换都会**先关闭正在运行的 CodeBuddy CLI 再写状态**：key 是进程级快照，
/// 不关进程就会出现"新默认账号 + 旧进程仍持旧 key"的窗口。当前 CLI 会话因此会中断。
///
/// `close_running_cli` 入参**已废弃**（保留接受但忽略），仅为不破坏既有调用方。
///
/// async + spawn_blocking：切换会用登录 shell 定位 node 并执行 apiKeyHelper
/// 校验账号（子进程无超时），同步 command 会阻塞主线程造成 UI 卡顿。
#[tauri::command(rename_all = "camelCase")]
pub async fn switch_codebuddy_cli_account(
    account_id: String,
    close_running_cli: Option<bool>,
) -> Result<Value, String> {
    if account_id.trim().is_empty() {
        return Err("缺少 accountId".to_string());
    }
    let _ = close_running_cli;
    tauri::async_runtime::spawn_blocking(move || codebuddy_cli::switch_active_account(&account_id))
        .await
        .map_err(|e| e.to_string())?
}

/// GET /api/codebuddy-cn-ide/status —— CodeBuddy IDE 安装/运行/当前账号。
///
/// async + spawn_blocking：状态检测会跑 ps / mdfind 等子进程（mdfind 可能
/// 耗时数秒），账号页每次挂载都会刷新，若在主线程执行会造成页面卡顿。
#[tauri::command]
pub async fn get_codebuddy_cn_ide_status() -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(codebuddy_cn_ide::status)
        .await
        .map_err(|error| format!("查询 CodeBuddy IDE 状态失败: {error}"))
}

/// POST /api/codebuddy-cn-ide/switch —— 注入凭证并可选重启 CodeBuddy CN IDE。
///
/// `copySessions` 非空时，切换前先把勾选的会话复制到目标账号（默认沿用会话 id，
/// 目标已有同 id 时才重随机）并登记关联；`syncSelections` 非空时，再把关联会话的
/// 新增内容同步过去（只同步不复制同样可用）。两者都为空时行为与纯切换逐字一致。
///
/// async + spawn_blocking：切换会关闭并重启 CodeBuddy CN，可能阻塞数十秒，
/// 与 WorkBuddy 切换同理，若在同步 command（主线程）执行会卡死整个 UI。
#[tauri::command(rename_all = "camelCase")]
pub async fn switch_codebuddy_cn_ide_account(
    account_id: String,
    restart: Option<bool>,
    copy_sessions: Option<Vec<vscode_session::CopyItem>>,
    sync_selections: Option<Value>,
) -> Result<Value, String> {
    if account_id.trim().is_empty() {
        return Err("缺少 accountId".to_string());
    }
    let restart = restart.unwrap_or(true);
    let items = copy_sessions.unwrap_or_default();
    // 入参形状由 core 校验（缺 groupId / previewToken / mode 一律拒绝）；这里只做透传。
    let sync_selections = session::parse_sync_selections(sync_selections.as_ref())?;
    tauri::async_runtime::spawn_blocking(move || {
        if items.is_empty() && sync_selections.is_empty() {
            codebuddy_cn_ide::switch_account(&account_id, restart)
        } else {
            codebuddy_ide_session::switch_codebuddy_cn_ide_with_copy(
                &account_id,
                restart,
                &items,
                &sync_selections,
            )
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

/// GET /api/codebuddy-cn-ide/sessions —— 列出当前 IDE 账号可复制的会话。
///
/// async + spawn_blocking：会扫描 IDE 会话目录（可能较大）并读取本机登录 secret，
/// 避免阻塞主线程。未登录/未安装时返回空列表而非报错（供前端渲染空态）。
#[tauri::command]
pub async fn list_codebuddy_ide_sessions() -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(codebuddy_ide_session::list_current_codebuddy_ide_sessions)
        .await
        .map_err(|error| format!("列出 CodeBuddy IDE 会话失败: {error}"))
}

/// POST /api/codebuddy-cn-ide/session-links —— 预览「当前 IDE 账号 → 目标账号」可同步的关联会话。
///
/// 只读：每组的 `defaultChecked` 与 `availableModes` 是前端勾选权限的唯一来源。
/// async + spawn_blocking：会扫描会话目录并读取正文，避免阻塞 UI。
#[tauri::command(rename_all = "camelCase")]
pub async fn codebuddy_ide_session_links_preview(
    target_account_id: String,
) -> Result<Value, String> {
    if target_account_id.trim().is_empty() {
        return Err("缺少 targetAccountId".to_string());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let target = account::find_account(&target_account_id).ok_or("目标账号不存在")?;
        codebuddy_ide_session_sync::links_preview(&target)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// POST /api/codebuddy-cn-ide/detect —— 读取本机 CN IDE 当前登录并尝试匹配账号库。
///
/// async + spawn_blocking：会通过 Keychain/secret 读取子进程，避免阻塞主线程。
#[tauri::command]
pub async fn detect_codebuddy_cn_ide_account() -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(codebuddy_cn_ide::detect_current_account)
        .await
        .map_err(|e| e.to_string())?
}

/// GET /api/vscode-ext/status —— VS Code CodeBuddy 扩展安装/运行/当前账号。
///
/// async + spawn_blocking：状态检测会跑 tasklist/ps 等子进程，账号页每次挂载都会
/// 刷新，若在主线程执行会造成页面卡顿。
#[tauri::command]
pub async fn get_vscode_ext_status() -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(vscode_ext::status)
        .await
        .map_err(|error| format!("查询 VS Code CodeBuddy 插件状态失败: {error}"))
}

/// GET /api/vscode-ext/sessions —— 列出当前 VS Code 扩展账号可复制的会话。
///
/// async + spawn_blocking：会扫描扩展数据目录（可能较大）并读取本地状态文件，
/// 避免阻塞主线程。未登录/未安装时返回空列表而非报错（供前端渲染空态）。
#[tauri::command]
pub async fn list_vscode_sessions() -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(|| match vscode_ext::active_ext_uid() {
        Some(uid) => vscode_session::list_vscode_sessions(&uid),
        None => json!({ "sourceUid": Value::Null, "sessions": [], "skipped": 0 }),
    })
    .await
    .map_err(|error| format!("列出 VS Code CodeBuddy 插件会话失败: {error}"))
}

/// POST /api/vscode-ext/switch —— 注入凭证到 VS Code CodeBuddy 扩展。
///
/// `copySessions` 非空时，切换前先把勾选的会话复制到目标账号（新 id，加法）并登记关联；
/// `syncSelections` 非空时，再把关联会话的新增内容同步过去（只同步不复制同样可用）。
/// `restart` 缺省 true：VS Code 正在运行时由后端「优雅退出 → 写入 → 重新打开」，
/// 传 false 则退回「请先完全退出 VS Code」的手动模式。
///
/// async + spawn_blocking：读写 state.vscdb + DPAPI 解密 + 等待编辑器退出 + 会话目录
/// 复制都可能阻塞，避免卡 UI。
#[tauri::command(rename_all = "camelCase")]
pub async fn switch_vscode_ext_account(
    account_id: String,
    restart: Option<bool>,
    copy_sessions: Option<Vec<vscode_session::CopyItem>>,
    sync_selections: Option<Value>,
) -> Result<Value, String> {
    if account_id.trim().is_empty() {
        return Err("缺少 accountId".to_string());
    }
    let restart = restart.unwrap_or(true);
    let items = copy_sessions.unwrap_or_default();
    // 入参形状由 core 校验（缺 groupId / previewToken / mode 一律拒绝）；这里只做透传。
    let sync_selections = session::parse_sync_selections(sync_selections.as_ref())?;
    tauri::async_runtime::spawn_blocking(move || {
        // 复制与同步都没勾选时才退回纯切换路径（不再由 `copySessions` 单独决定）。
        if items.is_empty() && sync_selections.is_empty() {
            vscode_ext::switch_account(&account_id, restart)
        } else {
            vscode_session::switch_vscode_ext_with_copy(
                &account_id,
                restart,
                &items,
                &sync_selections,
            )
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

/// POST /api/vscode-ext/session-links —— 预览「当前插件账号 → 目标账号」可同步的关联会话。
///
/// 只读：返回 `supported / storeStatus / groups`，其中每组的 `defaultChecked` 与
/// `availableModes` 是前端勾选权限的唯一来源，前端不得自行扩大。
/// async + spawn_blocking：会扫描扩展数据目录并读取会话正文，避免阻塞 UI。
#[tauri::command(rename_all = "camelCase")]
pub async fn vscode_session_links_preview(target_account_id: String) -> Result<Value, String> {
    if target_account_id.trim().is_empty() {
        return Err("缺少 targetAccountId".to_string());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let target = account::find_account(&target_account_id).ok_or("目标账号不存在")?;
        vscode_session_sync::links_preview(&target)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn get_codebuddy_ide_status() -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(codebuddy_ide::status)
        .await
        .map_err(|error| format!("查询 CodeBuddy IDE 状态失败: {error}"))
}

/// GET /api/jetbrains/status —— JetBrains IDE（IDEA / PyCharm）CodeBuddy 插件状态。
///
/// async + spawn_blocking：状态检测会跑 CIM / tasklist 等子进程，账号页每次挂载
/// 都会刷新，若在主线程执行会造成页面卡顿/闪窗。
#[tauri::command]
pub async fn get_jetbrains_status() -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(jetbrains::status)
        .await
        .map_err(|error| format!("查询 JetBrains IDE 状态失败: {error}"))
}

/// POST /api/jetbrains/switch —— 注入凭证到 JetBrains IDE 的 CodeBuddy 插件。
///
/// `restart` 缺省 true：IDE 正在运行时由后端「优雅退出 → 写入 → 重新打开」，
/// 传 false 则退回「请先完全退出 IDE」的手动模式。
/// `configDirs` 可选：目标配置目录名列表（如 ["PyCharm2026.2"]），缺省 / 空数组
/// = 全部装了插件的 IDE；非空时只写所选目录、只关闭/重开这些目录的运行实例。
///
/// async + spawn_blocking：等待 IDE 退出 + 读写 secret-storage.xml 都可能阻塞。
#[tauri::command(rename_all = "camelCase")]
pub async fn switch_jetbrains_account(
    account_id: String,
    restart: Option<bool>,
    config_dirs: Option<Vec<String>>,
) -> Result<Value, String> {
    if account_id.trim().is_empty() {
        return Err("缺少 accountId".to_string());
    }
    let dirs = config_dirs.filter(|list| !list.is_empty());
    tauri::async_runtime::spawn_blocking(move || {
        jetbrains::switch_account(&account_id, restart.unwrap_or(true), dirs.as_deref())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// POST /api/jetbrains/detect —— 读取本机 JetBrains IDE 插件当前登录并尝试匹配账号库。
///
/// async + spawn_blocking：会读多份 secret-storage.xml，避免阻塞主线程。
#[tauri::command]
pub async fn detect_jetbrains_account() -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(jetbrains::detect_current_account)
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command(rename_all = "camelCase")]
pub async fn switch_codebuddy_ide_account(
    account_id: String,
    restart: Option<bool>,
) -> Result<Value, String> {
    if account_id.trim().is_empty() {
        return Err("缺少 accountId".to_string());
    }
    tauri::async_runtime::spawn_blocking(move || {
        codebuddy_ide::switch_account(&account_id, restart.unwrap_or(true))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// POST /api/vscode-ext/detect —— 读取本机 VS Code 扩展当前登录并尝试匹配账号库。
///
/// async + spawn_blocking：会通过 Safe Storage 读取子进程，避免阻塞主线程。
#[tauri::command]
pub async fn detect_vscode_ext_account() -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(vscode_ext::detect_current_account)
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn detect_codebuddy_ide_account() -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(codebuddy_ide::detect_current_account)
        .await
        .map_err(|e| e.to_string())?
}

/// DELETE /api/delete —— 删除账号。
#[tauri::command]
pub fn delete_account(account_id: String) -> Result<Value, String> {
    let mut accounts = account::load_accounts();
    let before = accounts.len();
    accounts.retain(|a| a.get("id").and_then(|v| v.as_str()) != Some(account_id.as_str()));
    if accounts.len() == before {
        return Err("账号不存在".to_string());
    }
    account::save_accounts(&accounts).map_err(|e| e.to_string())?;
    Ok(json!({ "ok": true }))
}

/// POST /api/oauth/start —— 发起 OAuth 扫码登录（`variant` 缺省国内版）。
#[tauri::command]
pub async fn oauth_start(variant: Option<String>) -> Result<Value, String> {
    oauth::oauth_start(WbVariant::parse(variant.as_deref())).await
}

/// GET /api/oauth/status —— 轮询采集结果（档位取发起时记录，无需传参）。
#[tauri::command]
pub async fn oauth_status(login_id: String) -> Value {
    oauth::oauth_poll(&login_id).await
}

/// POST /api/import-local —— 导入本机当前账号（`variant` 缺省国内版）。
#[tauri::command]
pub fn import_local(variant: Option<String>) -> Result<Value, String> {
    account::import_local(WbVariant::parse(variant.as_deref()))
        .map(|acc| json!({ "ok": true, "account": acc }))
}

// ---------------------------------------------------------------------------
// 导出 / 导入账号
// ---------------------------------------------------------------------------

/// POST /api/export-accounts —— 按账号 id 列表导出完整记录（含 token）。
#[tauri::command]
pub fn export_accounts(account_ids: Vec<String>) -> Result<Value, String> {
    export_import::export_accounts(&account_ids)
        .map(|records| json!({ "ok": true, "accounts": records }))
}

/// POST /api/export-accounts-to-path —— 把勾选账号的完整记录写入用户选择的路径（保存对话框产物）。
#[tauri::command]
pub fn export_accounts_to_path(account_ids: Vec<String>, path: String) -> Result<Value, String> {
    export_import::export_accounts_to_path(&account_ids, &path)
        .map(|path| json!({ "ok": true, "path": path }))
}

/// POST /api/import/preview —— 解析导入文件并返回脱敏预览（含文件内索引）。
#[tauri::command]
pub fn preview_import_accounts(file_text: String) -> Result<Value, String> {
    export_import::preview_accounts(&file_text)
}

/// POST /api/import —— 按选中索引把账号导入账号库，返回导入/跳过/覆盖计数。
#[tauri::command]
pub fn import_accounts(file_text: String, indexes: Vec<usize>) -> Result<Value, String> {
    let result = export_import::import_accounts(&file_text, &indexes)?;
    Ok(json!({
        "ok": true,
        "imported": result.imported,
        "skipped": result.skipped,
        "overwritten": result.overwritten,
    }))
}

/// 打开系统设置授权面板。默认「完全磁盘访问」（该 anchor 各版本均有效）；
/// 传 `target="app_management"` 尝试「App 管理」（macOS 15+，部分版本不支持深链）。
///
/// 使用 macOS 13+ 深链接格式（`com.apple.settings.PrivacySecurity.extension?Privacy_*`）。
#[tauri::command]
pub fn open_permission_settings(target: Option<String>) -> Result<(), String> {
    let t = target.unwrap_or_else(|| "all_files".to_string());
    let url = match t.as_str() {
        "app_management" => {
            "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_AppManagement"
        }
        _ => {
            "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_AllFiles"
        }
    };
    let _ = std::process::Command::new("open").arg(url).spawn();
    Ok(())
}

/// 权限自检：尝试在认证文件目录写/删探针文件，确认完全磁盘访问等授权是否生效。
///
/// 探针放在该档位的登录态文件旁边（两档位同目录，`variant` 缺省国内版）。
#[tauri::command]
pub fn check_auth_permission(variant: Option<String>) -> Value {
    let variant = WbVariant::parse(variant.as_deref());
    let path = auth_file::auth_file_path(variant);
    // 由档位路径派生探针名，避免再写一份档位相关的文件名。
    let probe = path.with_extension("info.probe");
    match std::fs::write(&probe, "probe") {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            json!({ "ok": true, "variant": variant.as_str(), "message": "认证目录可写，权限正常" })
        }
        Err(e) => json!({
            "ok": false,
            "variant": variant.as_str(),
            "error": e.to_string(),
            "dir": path.parent().map(|p| p.to_string_lossy().to_string()),
            "hint": "请在 系统设置→隐私与安全性 中授权：优先「App 管理」开启 wb-switch，若没有则去「完全磁盘访问」把 wb-switch 拖进去；授权后需重启 App 生效",
        }),
    }
}

/// 在 Finder 中显示当前 App（便于拖拽到「完全磁盘访问」授权框）。
#[tauri::command]
pub fn reveal_app_in_finder() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let _ = std::process::Command::new("open")
        .arg("-R")
        .arg(&exe)
        .spawn();
    Ok(())
}

/// POST /api/switch —— 切换账号（备份 → 关进程 → 恢复/复制/同步会话 → 写认证 → 重启）。
///
/// `syncSelections` 与 HTTP 端同形（`[{groupId, previewToken, mode}]`）。
///
/// async + spawn_blocking：切换中关闭/启动 WorkBuddy 会阻塞数十秒，
/// 若在同步 command（主线程）执行会卡死整个 UI（loading 遮罩无法渲染）。
#[tauri::command(rename_all = "camelCase")]
pub async fn switch_account(
    app: tauri::AppHandle,
    account_id: String,
    restart: Option<bool>,
    share_sessions: Option<bool>,
    copy_session_ids: Option<Vec<String>>,
    sync_selections: Option<Value>,
) -> Result<Value, String> {
    if account_id.trim().is_empty() {
        return Err("缺少 accountId".to_string());
    }
    let restart = restart.unwrap_or(true);
    let share_sessions = share_sessions.unwrap_or(false);
    let copy_ids = copy_session_ids.unwrap_or_default();
    // 入参形状由 core 校验（缺 groupId / previewToken / mode 一律拒绝）；这里只做透传，
    // 不在命令层做业务判定。
    let sync_selections = session::parse_sync_selections(sync_selections.as_ref())?;
    let progress: switch::ProgressFn = Box::new(move |message| {
        let _ = app.emit("switch-progress", json!({ "message": message }));
    });
    tauri::async_runtime::spawn_blocking(move || {
        switch::switch_account(
            Some(&progress),
            &account_id,
            restart,
            share_sessions,
            &copy_ids,
            &sync_selections,
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

/// GET /api/sessions —— 当前账号的会话列表（`variant` 缺省国内版）。
#[tauri::command]
pub fn list_sessions(variant: Option<String>) -> Value {
    let variant = WbVariant::parse(variant.as_deref());
    match session::current_user_uid(variant) {
        Some(uid) => json!({
            "sessions": session::list_sessions_for_user(variant, &uid),
            "current": uid,
        }),
        None => json!({"sessions": [], "current": Value::Null}),
    }
}

/// POST /api/sessions/copy —— 把勾选会话复制到指定账号（路径 B）。
#[tauri::command(rename_all = "camelCase")]
pub async fn copy_sessions(
    target_account_id: String,
    session_ids: Vec<String>,
) -> Result<Value, String> {
    if target_account_id.trim().is_empty() {
        return Err("缺少 targetAccountId".to_string());
    }
    if session_ids.is_empty() {
        return Err("缺少 sessionIds".to_string());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let target = account::find_account(&target_account_id).ok_or("目标账号不存在")?;
        // 档位取目标账号自身（copy_sessions_for_switch 内部判定）。
        session::copy_sessions_for_switch(&target, &session_ids)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 预览「当前账号 → 目标账号」的关联会话同步项（桌面端 command）。
///
/// 只读：返回 `supported / storeStatus / groups`，其中每组的 `defaultChecked` 与
/// `availableModes` 是前端勾选权限的唯一来源，前端不得自行扩大。
/// `variant` 缺省取目标账号自身档位：来源 uid 从该档位的登录态读取，目标与来源必须在
/// 同一档位内比较（与 `copy_sessions` 的档位约定一致）。与 `POST /api/session-links/preview` 同形。
#[tauri::command(rename_all = "camelCase")]
pub async fn session_links_preview(
    target_account_id: String,
    variant: Option<String>,
) -> Result<Value, String> {
    if target_account_id.trim().is_empty() {
        return Err("缺少 targetAccountId".to_string());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let target = account::find_account(&target_account_id).ok_or("目标账号不存在")?;
        let variant = variant
            .as_deref()
            .map(|raw| WbVariant::parse(Some(raw)))
            .unwrap_or_else(|| account::variant_of(&target));
        session::session_links_preview(variant, &target)
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---------------------------------------------------------------------------
// 阶段 3：签到 + token 刷新
// ---------------------------------------------------------------------------

/// GET /api/checkin/status —— 查询单账号签到状态（档位取账号自身）。
#[tauri::command]
pub async fn get_checkin_status(account_id: String) -> Result<Value, String> {
    let acc = account::find_account(&account_id).ok_or("账号不存在")?;
    let mut status = checkin::get_checkin_status_for_display(&acc).await;
    // 结果行带档位，前端按当前档位过滤时无需再查账号。
    status["variant"] = json!(account::variant_of(&acc).as_str());
    Ok(status)
}

/// POST /api/credits —— 查询单账号积分资源及到期时间。
#[tauri::command]
pub async fn get_credit_expiry(account_id: String) -> Result<Value, String> {
    let acc = account::find_account(&account_id).ok_or("账号不存在")?;
    Ok(credits::get_credit_expiry(&acc).await)
}

/// GET /api/credits/stats —— 本地快照与官方请求用量统计。
/// `refresh = true` 时才重新请求官方用量；默认读缓存。
#[tauri::command]
pub async fn get_credit_statistics(refresh: Option<bool>) -> Value {
    credit_usage::get_statistics(refresh.unwrap_or(false)).await
}

#[tauri::command]
pub async fn get_token_statistics(days: Option<i64>) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || token_stats::get_statistics(days))
        .await
        .map_err(|error| format!("扫描 Token 统计失败: {error}"))
}

/// GET /api/rate-limits —— 模型限额台账（全部账号当前受限的模型与官方恢复时刻）。
///
/// 不接收档位参数：扫描本身就是全局的（两档位各扫一遍）。扫描读取本机日志文件，
/// 放 blocking 线程避免阻塞主线程；无受限模型时返回空数组，不是错误。
#[tauri::command]
pub async fn get_rate_limits() -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(limits::get_rate_limits)
        .await
        .map_err(|error| format!("扫描模型限额失败: {error}"))
}

/// GET /api/rate-limits/hook-status —— hook 安装状态（脚本 + 三处客户端配置逐项结果）。
///
/// 读三个 `settings.json` 与一个脚本文件，放 blocking 线程避免占用主线程。
#[tauri::command]
pub async fn get_rate_limit_hook_status() -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(with_hook_runtime_fields)
        .await
        .map_err(|error| format!("查询限额 hook 状态失败: {error}"))
}

/// POST /api/rate-limits/install-hook —— 安装 hook（幂等，写前备份；同时清除「卸载过」标记）。
#[tauri::command]
pub async fn install_rate_limit_hook() -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let result = rate_limit_hook::install_hook();
        // 扫描范围随安装结果变化（只对未注册的来源扫日志），缓存必须作废。
        limits::invalidate_scan_cache();
        result.map(|_| with_hook_runtime_fields())
    })
    .await
    .map_err(|error| error.to_string())?
}

/// POST /api/rate-limits/uninstall-hook —— 卸载 hook（移除注册条目，尽量逐字节还原）。
///
/// 卸载即用户拒绝自动接入：`install_hook` / `hookOptOut` 的置位在 core 里与安装逻辑同处，
/// 两个宿主（桌面端 / webui）共用同一语义。
#[tauri::command]
pub async fn uninstall_rate_limit_hook() -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let result = rate_limit_hook::uninstall_hook();
        limits::invalidate_scan_cache();
        result.map(|_| with_hook_runtime_fields())
    })
    .await
    .map_err(|error| error.to_string())?
}

/// hook 状态 + 运行期字段（最近一次 hook 事件时刻）。
fn with_hook_runtime_fields() -> Value {
    let mut status = rate_limit_hook::hook_status();
    status["lastEventAt"] = json!(rate_limit_events::last_event_at());
    status
}

/// GET /api/rate-limits/config —— 限额监听开关。
#[tauri::command]
pub fn get_rate_limit_config() -> Value {
    crate::modules::config::load_rate_limit_config()
}

/// POST /api/rate-limits/config —— 保存限额监听开关。
///
/// 走 `limits::save_rate_limit_config`：`scanIdeLogs` 变化时作废扫描缓存（下一次按当前
/// 来源范围重算），否则关掉 IDE 扫描后下一次还会按旧缓存把两个 IDE 扫一遍。
#[tauri::command]
pub fn save_rate_limit_config(config: Value) -> Result<Value, String> {
    limits::save_rate_limit_config(&config).map_err(|e| e.to_string())?;
    Ok(crate::modules::config::load_rate_limit_config())
}

/// POST /api/checkin —— 单账号立即签到。
#[tauri::command]
pub async fn checkin(account_id: String) -> Result<Value, String> {
    let acc = account::find_account(&account_id).ok_or("账号不存在")?;
    Ok(checkin::checkin_account(&acc).await)
}

/// POST /api/checkin/all —— 全部账号立即签到（每个账号按自身档位）。
/// `variant` 缺省为 `None`（全部档位，保持原行为）；显式传入时只处理该档位。
/// 关闭自动签到的账号逐账号返回 skipped 原因（设置页与托盘同样遵守）。
#[tauri::command]
pub async fn checkin_all(variant: Option<String>) -> Value {
    let variant = variant.as_deref().map(|raw| WbVariant::parse(Some(raw)));
    checkin::run_checkin_all(variant).await
}

/// GET /api/checkin/config —— 自动签到配置。
#[tauri::command]
pub fn get_auto_checkin_config() -> Value {
    crate::modules::config::load_checkin_config()
}

/// POST /api/checkin/config —— 保存自动签到配置。
#[tauri::command]
pub fn save_auto_checkin_config(config: Value) -> Result<Value, String> {
    crate::modules::config::save_checkin_config(&config).map_err(|e| e.to_string())?;
    Ok(crate::modules::config::load_checkin_config())
}

/// GET /api/checkin/logs —— 签到日志（每行带 `variant`，便于前端按档位过滤）。
#[tauri::command]
pub fn get_checkin_logs() -> Value {
    json!({ "logs": checkin::load_checkin_logs_with_variant() })
}

// ---------------------------------------------------------------------------
// 派猫猫旅行
// ---------------------------------------------------------------------------

/// GET /api/travel/status —— 查询单账号今日旅行状态标签。
///
/// 档位取账号自身；不支持成长中心的档位（国际版）由 core 返回
/// `{"status":"skipped","reason":"unsupported_variant"}` 形态，宿主原样透传，
/// 不把它吞成「未旅行」。
#[tauri::command]
pub async fn get_travel_status(account_id: String) -> Result<Value, String> {
    account::find_account(&account_id).ok_or("账号不存在")?;
    travel::reconcile_due_travel(Some(account_id.as_str())).await;
    Ok(travel::travel_display(&account_id))
}

/// GET /api/travel/config —— 自动旅行配置。
#[tauri::command]
pub fn get_auto_travel_config() -> Value {
    crate::modules::config::load_travel_config()
}

/// POST /api/travel/config —— 保存自动旅行配置。开启时立刻跑一轮派发/领取。
#[tauri::command]
pub fn save_auto_travel_config(config: Value) -> Result<Value, String> {
    crate::modules::config::save_travel_config(&config).map_err(|e| e.to_string())?;
    let saved = crate::modules::config::load_travel_config();
    if saved.get("enabled").and_then(Value::as_bool) == Some(true) {
        tauri::async_runtime::spawn(async {
            let _ = travel::run_travel_cycle().await;
            let _ = travel::run_travel_claim_cycle().await;
        });
    }
    Ok(saved)
}

// ---------------------------------------------------------------------------
// 自动轮换（CodeBuddy CLI）
// ---------------------------------------------------------------------------

/// GET /api/rotate/config —— 自动轮换配置。
#[tauri::command]
pub fn get_auto_rotate_config() -> Value {
    crate::modules::config::load_auto_rotate_config()
}

/// POST /api/rotate/config —— 保存自动轮换配置。
#[tauri::command]
pub fn save_auto_rotate_config(config: Value) -> Result<Value, String> {
    crate::modules::config::save_auto_rotate_config(&config).map_err(|e| e.to_string())?;
    Ok(crate::modules::config::load_auto_rotate_config())
}

/// GET /api/rotate/status —— 轮换状态（配置 + 上次检查/切换）。
#[tauri::command]
pub fn rotate_status() -> Value {
    rotate::rotate_status()
}

/// POST /api/rotate/run —— 手动触发一次轮换检查。
///
/// 返回体里的 `notify`（若因存活门控被推迟且未超当日预算）由宿主投递系统通知；
/// 无头 server 只返回该字段，不投递。
#[tauri::command]
pub async fn run_rotate(app: tauri::AppHandle) -> Value {
    let result = rotate::run_rotate_cycle().await;
    crate::deliver_rotate_notify(&app, &result);
    result
}

/// GET /api/rotate/logs —— 最近轮换日志。
#[tauri::command]
pub fn get_rotate_logs() -> Value {
    json!({ "logs": rotate::rotate_logs() })
}

/// POST /api/refresh-token —— 单账号刷新 token。
#[tauri::command]
pub async fn refresh_account_token(account_id: String) -> Result<Value, String> {
    let acc = account::find_account(&account_id).ok_or("账号不存在")?;
    let fresh = refresh::refresh_account_token(acc).await;
    Ok(account::account_meta(&fresh))
}

// ---------------------------------------------------------------------------
// 阶段 4：自动更新
// ---------------------------------------------------------------------------

/// GET /api/update/config —— 更新源配置（owner/repo/token）。
#[tauri::command]
pub fn get_github_config() -> Value {
    update::load_github_config()
}

/// POST /api/update/config —— 保存更新源配置。
#[tauri::command]
pub fn save_github_config(config: Value) -> Result<Value, String> {
    update::save_github_config(&config).map_err(|e| e.to_string())?;
    Ok(update::load_github_config())
}

/// GET /api/update/check —— 检查 GitHub Releases 是否有新版本。
/// force=true 时绕过缓存强制刷新（设置页手动检查）。
///
/// 内部走统一更新状态机（`update_service::check`）：检查结果同时写入快照并 emit
/// `update-state`，托盘菜单与前端弹窗因此始终显示同一阶段；返回值结构与改造前一致。
#[tauri::command]
pub async fn check_update(
    app: tauri::AppHandle,
    proxy: Option<String>,
    force: Option<bool>,
) -> Value {
    crate::update_service::check(&app, proxy.as_deref(), force.unwrap_or(false)).await
}

/// GET /api/update/state —— 当前更新状态快照（前端首屏初始化 + 事件丢失兜底）。
#[tauri::command]
pub fn update_state() -> crate::update_service::UpdateSnapshot {
    crate::update_service::snapshot()
}

/// POST /api/update/download —— 启动更新包下载。
///
/// 异步：立即返回，进度与阶段经 `update-state` 事件推送、托盘 tooltip 实时显示。
#[tauri::command]
pub fn update_download(app: tauri::AppHandle) -> Result<(), String> {
    crate::update_service::start_download(&app)
}

/// POST /api/update/restart —— 安装已下载的更新包并重启应用。
///
/// 安装时机在用户点「重启以完成升级」时：Windows 安装器要求应用退出才能完成安装，
/// macOS 安装可能触发 `/Applications` 写授权，都不适合在下载完成时静默执行。
#[tauri::command]
pub async fn update_restart(app: tauri::AppHandle) -> Result<(), String> {
    crate::update_service::restart(&app).await
}

/// 启动当前应用的新进程并退出旧进程，用于更新安装完成后的立即重启。
#[tauri::command]
pub fn relaunch_app(_app: tauri::AppHandle) -> Result<(), String> {
    relaunch_app_inner(&_app)
}

/// 重启实现：命令与更新服务共用（保留单实例交棒与 `--hidden` 剔除）。
pub(crate) fn relaunch_app_inner<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
) -> Result<(), String> {
    let executable = std::env::current_exe().map_err(|e| format!("无法定位应用程序: {e}"))?;
    // 更新重启是普通启动路径；不要把系统自启专用参数带给新进程。
    let args = std::env::args_os().skip(1).filter(|arg| {
        #[cfg(desktop)]
        {
            should_forward_relaunch_arg(arg.as_os_str())
        }
        #[cfg(not(desktop))]
        {
            true
        }
    });
    // 先放弃单例身份（删除 socket，并在 macOS 上释放 flock）再交棒：否则新进程
    // 可能在旧 listener / 锁消失前连上或抢锁失败，出现「旧进程已退、新进程也退出」
    // 而应用彻底消失。
    #[cfg(desktop)]
    tauri_plugin_single_instance::destroy(_app);
    #[cfg(target_os = "macos")]
    crate::instance_lock::release(_app);
    match std::process::Command::new(executable).args(args).spawn() {
        Ok(_) => std::process::exit(0),
        Err(e) => {
            // 已经放弃单例身份：要么把锁拿回来继续跑，要么退出。
            // 不允许「无锁继续运行」（否则之后再启动就会双开）。
            #[cfg(target_os = "macos")]
            if !crate::instance_lock::reacquire(_app) {
                std::process::exit(0);
            }
            Err(format!("启动应用失败: {e}"))
        }
    }
}

// ---------------------------------------------------------------------------
// 开机自启（仅桌面端；webui 不提供同名接口）
// ---------------------------------------------------------------------------

/// GET /api/launch-at-login —— 查询系统当前的开机自启注册状态。
///
/// 以 tauri-plugin-autostart 的 OS 状态为唯一事实来源，不另存本地布尔值。
#[tauri::command]
pub fn get_launch_at_login_enabled(_app: tauri::AppHandle) -> Result<bool, String> {
    #[cfg(desktop)]
    {
        use tauri_plugin_autostart::ManagerExt;
        _app.autolaunch()
            .is_enabled()
            .map_err(|e| format!("查询开机自启状态失败：{e}"))
    }
    #[cfg(not(desktop))]
    {
        Err("当前平台不支持开机自启".to_string())
    }
}

#[cfg(desktop)]
fn should_forward_relaunch_arg(arg: &std::ffi::OsStr) -> bool {
    arg != std::ffi::OsStr::new(crate::tray::SILENT_STARTUP_ARG)
}

#[cfg(all(test, desktop))]
mod relaunch_tests {
    use super::should_forward_relaunch_arg;
    use std::ffi::OsStr;

    #[test]
    fn update_relaunch_drops_only_the_exact_silent_startup_arg() {
        assert!(!should_forward_relaunch_arg(OsStr::new("--hidden")));
        assert!(should_forward_relaunch_arg(OsStr::new("--hidden-x")));
        assert!(should_forward_relaunch_arg(OsStr::new("x--hidden")));
        assert!(should_forward_relaunch_arg(OsStr::new("--debug")));
    }
}

/// POST /api/launch-at-login —— 注册 / 移除系统开机自启，并回读权威状态。
///
/// 回读结果与请求值不一致时按失败处理并返回当前真实状态，避免假装设置成功。
#[tauri::command]
pub fn set_launch_at_login_enabled(_app: tauri::AppHandle, enabled: bool) -> Result<bool, String> {
    #[cfg(desktop)]
    {
        use tauri_plugin_autostart::ManagerExt;
        let autostart = _app.autolaunch();
        let action = if enabled { "开启" } else { "关闭" };
        let result = if enabled {
            autostart.enable()
        } else {
            autostart.disable()
        };
        if let Err(e) = result {
            return Err(format!("{action}开机自启失败：{e}"));
        }
        let authoritative = autostart
            .is_enabled()
            .map_err(|e| format!("开机自启设置后回读状态失败：{e}"))?;
        if authoritative != enabled {
            return Err(format!(
                "{action}开机自启未生效（系统当前状态：{}），请稍后重试",
                if authoritative {
                    "已开启"
                } else {
                    "未开启"
                }
            ));
        }
        Ok(authoritative)
    }
    #[cfg(not(desktop))]
    {
        let _ = enabled;
        Err("当前平台不支持开机自启".to_string())
    }
}

// ---------------------------------------------------------------------------
// 错误日志（前端崩溃 / 未捕获错误落盘）
// ---------------------------------------------------------------------------

/// 记录一条错误日志（`kind` 白名单：frontend_crash / frontend_unhandled / backend）。
///
/// 只落盘、不返回失败：目录只读、磁盘满等写入失败由 core 静默降级（`let _ =`），
/// 绝不让「记日志」反过来打断前端主流程。
#[tauri::command]
pub async fn log_error(kind: String, message: String, detail: Option<String>) {
    error_log::record(&kind, &message, detail.as_deref().unwrap_or_default());
}

/// 错误日志文件路径（设置页展示用）。
#[tauri::command]
pub fn get_error_log_path() -> String {
    error_log::error_log_path().to_string_lossy().to_string()
}

/// 在文件管理器中定位错误日志；日志尚未生成时改为定位所在目录。
///
/// 走 tauri-plugin-opener 的 Rust API（不依赖前端 capability）；reveal 内部会
/// canonicalize，路径不存在会直接报错，所以这里按「文件 → 目录」逐级回退。
#[tauri::command]
pub fn reveal_error_log(app: tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;

    let path = error_log::error_log_path();
    // reveal 会 canonicalize，目标不存在就直接失败。日志还没生成时改为定位目录；
    // 目录也不存在（还没写过任何错误）就先建出来，避免按钮第一次点就失败。
    let target = if path.exists() {
        path
    } else {
        match path.parent() {
            Some(dir) => {
                let _ = std::fs::create_dir_all(dir);
                if dir.exists() {
                    dir.to_path_buf()
                } else {
                    path
                }
            }
            None => path,
        }
    };
    app.opener()
        .reveal_item_in_dir(target)
        .map_err(|error| format!("打开日志位置失败: {error}"))
}

// ---------------------------------------------------------------------------
// 通知存档（toast 事后可查）
// ---------------------------------------------------------------------------

/// 记录一条应用内提示；前端所有 toast 都会同步写一份，失败不影响提示本身。
#[tauri::command]
pub async fn record_notification(
    level: String,
    title: String,
    description: Option<String>,
) -> Result<(), String> {
    notifications::record(&level, &title, description.as_deref())
}

/// 读取最近的通知（新的在前，最多 100 条）。
#[tauri::command]
pub async fn list_notifications() -> Result<Value, String> {
    Ok(json!({ "items": notifications::list()? }))
}

/// 清空通知存档。
#[tauri::command]
pub async fn clear_notifications() -> Result<(), String> {
    notifications::clear()
}
