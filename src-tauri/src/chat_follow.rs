//! 聊天窗跟随桌宠：偏移量 + 明确时机重建，不靠窗口事件推断。

use tauri::{AppHandle, Manager};


#[derive(Default)]
struct ChatFollowState {
    /// 聊天窗相对桌宠的偏移 (chat - pet)
    offset: Option<(i32, i32)>,
    /// 聊天窗可见时它的上一帧位置，仅用于日志
    last_seen: Option<(i32, i32)>,
}

static CHAT_FOLLOW: std::sync::OnceLock<std::sync::Mutex<ChatFollowState>> =
    std::sync::OnceLock::new();
static CHAT_MOVE_HOOKED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn state() -> &'static std::sync::Mutex<ChatFollowState> {
    CHAT_FOLLOW.get_or_init(|| std::sync::Mutex::new(ChatFollowState::default()))
}

/// 追加一行跟随日志，超过 200KB 就重开，避免无限增长。
pub(crate) fn log_search(msg: &str) {
    log_line(msg);
}

fn log_line(msg: &str) {
    let dir = crate::config::app_data_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("follow.log");
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > 200 * 1024 {
            let _ = std::fs::remove_file(&path);
        }
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        use std::io::Write;
        let _ = writeln!(f, "{}", msg);
    }
}

/// 把窗口位置夹进它所在显示器的可见范围内（只在开窗时用）。
#[allow(dead_code)]
fn clamp_window_to_monitor(win: &tauri::WebviewWindow, x: i32, y: i32) -> (i32, i32) {
    let Ok(Some(mon)) = win.current_monitor() else {
        return (x, y);
    };
    let Ok(size) = win.outer_size() else {
        return (x, y);
    };
    let mp = mon.position();
    let ms = mon.size();
    let margin = 8;
    let min_x = mp.x + margin;
    let max_x = mp.x + ms.width as i32 - size.width as i32 - margin;
    let min_y = mp.y + margin;
    let max_y = mp.y + ms.height as i32 - size.height as i32 - margin;
    (
        x.clamp(min_x.min(max_x), max_x.min(min_x)),
        y.clamp(min_y.min(max_y), max_y.min(min_y)),
    )
}

/// 用「聊天窗此刻的真实位置 - 桌宠此刻的真实位置」重建偏移。
/// 只在三个明确时刻调用：开窗、用户拖完聊天窗、前端调整完高度。
fn rebuild(app: &AppHandle, reason: &str) {
    let Some(chat) = app.get_webview_window("chat") else {
        return;
    };
    let Some(main) = app.get_webview_window("main") else {
        return;
    };
    if let (Ok(cp), Ok(mp)) = (chat.outer_position(), main.outer_position()) {
        let offset = (cp.x - mp.x, cp.y - mp.y);
        let mut st = state().lock().unwrap();
        st.offset = Some(offset);
        st.last_seen = Some((cp.x, cp.y));
        log_line(&format!(
            "rebuild({reason}): pet=({},{}) chat=({},{}) offset=({},{})",
            mp.x, mp.y, cp.x, cp.y, offset.0, offset.1
        ));
    }
}

/// 聊天窗的移动事件只做记录，绝不拿来推断偏移。
/// （之前就是靠它区分「程序移动/用户拖动」，但事件顺序不可靠，
///   每次误判都会把偏移削掉一段，最后聊天窗卡在屏幕左上角。）
pub(crate) fn attach_logger(chat: &tauri::WebviewWindow) {
    if CHAT_MOVE_HOOKED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    chat.on_window_event(move |event| {
        if let tauri::WindowEvent::Moved(pos) = event {
            let mut st = state().lock().unwrap();
            st.last_seen = Some((pos.x, pos.y));
        }
    });
}

/// 桌宠移动时按固定偏移把聊天窗带过去。
/// 不做屏幕夹取：夹取会把「没跟上」变成「永久偏移」，绕着拖几圈就漂了。
/// 顶出桌面也没关系，她走回来就自动归位。
pub(crate) fn install(app: AppHandle) {
    let Some(main) = app.get_webview_window("main") else {
        return;
    };
    let app_ev = app.clone();
    main.on_window_event(move |event| {
        let tauri::WindowEvent::Moved(pos) = event else {
            return;
        };
        let pet = (pos.x, pos.y);
        let Some(chat) = app_ev.get_webview_window("chat") else {
            return;
        };
        if !chat.is_visible().unwrap_or(false) {
            state().lock().unwrap().offset = None;
            return;
        }
        let offset = state().lock().unwrap().offset;
        let Some((ox, oy)) = offset else {
            rebuild(&app_ev, "lazy");
            return;
        };
        let _ = chat.set_position(tauri::PhysicalPosition::new(pet.0 + ox, pet.1 + oy));
    });
}

/// 前端调用：用户拖完聊天窗 / 调整完高度后，重建偏移。
#[tauri::command]
pub fn sync_offset_from_frontend(app: AppHandle) {
    rebuild(&app, "frontend");
}

/// 开窗后由 lib.rs 写入初始偏移。
pub(crate) fn set_offset(offset: (i32, i32)) {
    state().lock().unwrap().offset = Some(offset);
}

/// 启动时记录「当前运行的二进制是哪一次编译的」。
/// 之前多次因为「改了代码但跑的还是旧进程」白折腾，这一行可以一锤定音。
pub(crate) fn log_startup() {
    let exe = std::env::current_exe().ok();
    let meta = exe.as_ref().and_then(|p| std::fs::metadata(p).ok());
    let mtime = meta
        .as_ref()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    log_line(&format!(
        "==== start pid={} exe_mtime_unix={} exe={}",
        std::process::id(),
        mtime,
        exe.map(|p| p.to_string_lossy().to_string()).unwrap_or_default()
    ));
}
