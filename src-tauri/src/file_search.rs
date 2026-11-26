//! 本地文件查找（降级版）：完全绕开模型，直接调系统索引。
//! macOS 用 Spotlight 的 `mdfind`，Windows 暂不支持（没有等价的即时索引）。
//!
//! 隐私边界：**只返回文件名和路径，绝不读取文件内容**。
//! 用户选择的是「整盘可搜」，所以这里不做目录白名单。

use std::process::Command;

#[derive(serde::Serialize)]
pub struct FileHit {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
}

/// 最多返回多少条，太多列表会溢出卡片
const MAX_HITS: usize = 12;

fn basename(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

/// 按文件名模糊查找。query 为空时直接返回空结果。
#[tauri::command]
pub fn find_files(query: String) -> Result<Vec<FileHit>, String> {
    let q = query.trim();
    if q.is_empty() {
        return Ok(Vec::new());
    }
    if !cfg!(target_os = "macos") {
        return Err("文件查找目前只支持 macOS".into());
    }

    // -name：只按文件名匹配，不会去翻文件内容
    let out = Command::new("mdfind")
        .arg("-name")
        .arg(q)
        .output()
        .map_err(|e| format!("调用 mdfind 失败：{e}"))?;

    if !out.status.success() {
        return Err("mdfind 执行失败".into());
    }

    let text = String::from_utf8_lossy(&out.stdout);
    let mut hits: Vec<FileHit> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        // 系统目录噪音很大，整盘搜也不该把这些塞给用户
        .filter(|l| !l.starts_with("/System/") && !l.starts_with("/private/"))
        .map(|l| FileHit {
            name: basename(l),
            path: l.to_string(),
            is_dir: std::path::Path::new(l).is_dir(),
        })
        .collect();

    // 用户自己的目录排在前面，其次名字短的（通常更接近真实想要的东西）
    hits.sort_by_key(|h| {
        let user_first = !h.path.starts_with("/Users/");
        (user_first, h.name.len())
    });
    hits.dedup_by(|a, b| a.path == b.path);
    hits.truncate(MAX_HITS);
    Ok(hits)
}

/// 在访达里定位到该文件（选中并高亮），不打开。
#[tauri::command]
pub fn reveal_path(path: String) -> Result<(), String> {
    if !cfg!(target_os = "macos") {
        return Err("暂只支持 macOS".into());
    }
    let status = Command::new("open")
        .arg("-R")
        .arg(&path)
        .status()
        .map_err(|e| format!("调用 open 失败：{e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("访达打开失败".into())
    }
}
