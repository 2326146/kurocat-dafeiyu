//! 系统音频监听：判断「现在是不是在放音乐」，用于给大肥鱼戴上耳机、飘 🎵。
//!
//! macOS 上用 CoreAudio 查询**默认输出设备**的
//! `kAudioDevicePropertyDeviceIsRunningSomewhere`：
//! 只要有任何 App 往这个设备送音频，它就会是 true。
//! 这是公开 API，不需要任何权限，也不用去检测是哪个播放器。
//!
//! 两个防抖（否则提示音、歌间空隙会让耳机疯狂戴上又摘下）：
//! - 连续出声 **≥ 5 秒** 才判定「在放音乐」
//! - 停止出声 **≥ 2 秒** 才判定「结束了」

use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter};

/// 连续出声多久才算真的在放音乐
const ON_DELAY: Duration = Duration::from_secs(5);
/// 停止出声多久才算真的停了
const OFF_DELAY: Duration = Duration::from_secs(2);
/// 轮询间隔（音频状态变化不频繁，700ms 足够，几乎不耗 CPU）
const POLL: Duration = Duration::from_millis(700);

/// 前端监听的事件名
pub const EVENT_MUSIC_STATE: &str = "kuro-music-state";

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::c_void;

    /// 'gone' —— 已对照 AudioToolbox 头文件实测校验（不是 'srme'）
    const DEVICE_IS_RUNNING_SOMEWHERE: u32 = 0x676F_6E65;
    /// 'dOut'
    const DEFAULT_OUTPUT_DEVICE: u32 = 0x644F_7574;
    /// 'glob'
    const SCOPE_GLOBAL: u32 = 0x676C_6F62;
    /// kAudioObjectSystemObject
    const SYSTEM_OBJECT: u32 = 1;

    #[repr(C)]
    struct PropertyAddress {
        selector: u32,
        scope: u32,
        element: u32,
    }

    #[link(name = "CoreAudio", kind = "framework")]
    extern "C" {
        fn AudioObjectGetPropertyData(
            object_id: u32,
            address: *const PropertyAddress,
            qualifier_size: u32,
            qualifier: *const c_void,
            io_size: *mut u32,
            out_data: *mut c_void,
        ) -> i32;
    }

    fn default_output_device() -> Option<u32> {
        let addr = PropertyAddress {
            selector: DEFAULT_OUTPUT_DEVICE,
            scope: SCOPE_GLOBAL,
            element: 0,
        };
        let mut device: u32 = 0;
        let mut size: u32 = std::mem::size_of::<u32>() as u32;
        let status = unsafe {
            AudioObjectGetPropertyData(
                SYSTEM_OBJECT,
                &addr,
                0,
                std::ptr::null(),
                &mut size,
                &mut device as *mut u32 as *mut c_void,
            )
        };
        if status == 0 && device != 0 {
            Some(device)
        } else {
            None
        }
    }

    /// 默认输出设备上是否有任何音频在跑
    pub fn is_output_running() -> bool {
        let Some(device) = default_output_device() else {
            return false;
        };
        let addr = PropertyAddress {
            selector: DEVICE_IS_RUNNING_SOMEWHERE,
            scope: SCOPE_GLOBAL,
            element: 0,
        };
        let mut running: u32 = 0;
        let mut size: u32 = std::mem::size_of::<u32>() as u32;
        let status = unsafe {
            AudioObjectGetPropertyData(
                device,
                &addr,
                0,
                std::ptr::null(),
                &mut size,
                &mut running as *mut u32 as *mut c_void,
            )
        };
        status == 0 && running != 0
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    /// 其他平台暂时恒定为「没有在放」
    pub fn is_output_running() -> bool {
        false
    }
}

/// 起一个后台线程轮询音频状态，变化时通过事件通知前端
pub fn spawn(app: AppHandle) {
    let _ = std::thread::Builder::new()
        .name("kuro-audio-monitor".into())
        .spawn(move || {
            let mut on_since: Option<Instant> = None;
            let mut off_since: Option<Instant> = Some(Instant::now());
            let mut active = false;

            loop {
                let running = platform::is_output_running();
                let now = Instant::now();

                if running {
                    off_since = None;
                    let since = *on_since.get_or_insert(now);
                    if !active && now.duration_since(since) >= ON_DELAY {
                        active = true;
                        let _ = app.emit(EVENT_MUSIC_STATE, true);
                    }
                } else {
                    on_since = None;
                    let since = *off_since.get_or_insert(now);
                    if active && now.duration_since(since) >= OFF_DELAY {
                        active = false;
                        let _ = app.emit(EVENT_MUSIC_STATE, false);
                    }
                }

                std::thread::sleep(POLL);
            }
        });
}
