//! 本地文件查找：直接调系统索引，完全绕开模型。
//! macOS 用 Spotlight（`mdfind`），Windows 暂不支持（没有等价的即时索引）。
//!
//! 隐私边界：**只返回文件名和路径，绝不读取文件内容**。

use std::process::Command;

#[derive(serde::Serialize)]
pub struct FileHit {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
}

const MAX_HITS: usize = 12;
/// 最多参与 AND 的分词数，太多会让查询变慢
const MAX_TOKENS: usize = 6;
/// 先取这么多候选再排序，避免一次拉回上万条
const MAX_CANDIDATES: usize = 400;

fn basename(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

/// 把查询拆成若干个「必须同时出现」的片段。
/// 中文按单字拆（"兔采购" → 兔 / 采 / 购），英文数字按词拆。
/// 单字比双字更宽松：文件叫「兔实验一期采购清单」时，"兔采购" 才能命中。
fn tokenize(q: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut latin = String::new();
    for ch in q.chars() {
        if ch.is_ascii_alphanumeric() {
            latin.push(ch);
            continue;
        }
        if !latin.is_empty() {
            out.push(std::mem::take(&mut latin));
        }
        if ch.is_whitespace() || ch.is_ascii_punctuation() {
            continue;
        }
        out.push(ch.to_string());
    }
    if !latin.is_empty() {
        out.push(latin);
    }
    out.dedup();
    out.truncate(MAX_TOKENS);
    out
}

fn escapes(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// 按文件名模糊查找。查询会被拆成多段，要求文件名同时包含全部分段。
#[tauri::command]
pub fn find_files(query: String) -> Result<Vec<FileHit>, String> {
    let q = query.trim();
    if q.is_empty() {
        return Ok(Vec::new());
    }
    if !cfg!(target_os = "macos") {
        return Err("文件查找目前只支持 macOS".into());
    }

    let toks = tokenize(q);
    if toks.is_empty() {
        return Ok(Vec::new());
    }

    // 每一段都要求出现在文件名里，段与段之间是 AND。
    // 用 kMDItemFSName 而不是 mdfind -name：后者只做全词匹配，中文基本查不到。
    let clauses: Vec<String> = toks
        .iter()
        .map(|t| format!("kMDItemFSName == \"*{}*\"cd", escapes(t)))
        .collect();
    let expr = clauses.join(" && ");

    crate::chat_follow::log_search(&format!("search tokens={:?} expr={}", toks, expr));
    crate::chat_follow::log_search(&format!(
        "search env LANG={:?} LC_ALL={:?} LC_CTYPE={:?} PATH={:?}",
        std::env::var("LANG").ok(),
        std::env::var("LC_ALL").ok(),
        std::env::var("LC_CTYPE").ok(),
        std::env::var("PATH").ok()
    ));

    let run = |args: &[&str]| -> (bool, usize, String) {
        match Command::new("mdfind").args(args).output() {
            Ok(o) => (
                o.status.success(),
                o.stdout.len(),
                String::from_utf8_lossy(&o.stderr).trim().to_string(),
            ),
            Err(e) => (false, 0, format!("spawn error: {e}")),
        }
    };

    // 先按分词 AND 查
    let (ok, len, err) = run(&[expr.as_str()]);
    crate::chat_follow::log_search(&format!(
        "mdfind expr ok={} stdout_bytes={} stderr={:?}",
        ok, len, err
    ));
    if !ok {
        return Err(format!("mdfind 执行失败：{err}"));
    }

    // 再试一次「把整串当一个词」的 -name 查询，作为对照
    let (ok2, len2, err2) = run(&["-name", q]);
    crate::chat_follow::log_search(&format!(
        "mdfind -name ok={} stdout_bytes={} stderr={:?}",
        ok2, len2, err2
    ));

    // 真正用的是分词查询，重新执行一次拿输出
    let out = Command::new("mdfind")
        .arg(&expr)
        .output()
        .map_err(|e| format!("调用 mdfind 失败：{e}"))?;
    if !out.status.success() {
        return Err("mdfind 执行失败".into());
    }

    let text = String::from_utf8_lossy(&out.stdout);
    let mut hits: Vec<FileHit> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter(|l| !l.starts_with("/System/") && !l.starts_with("/private/"))
        .take(MAX_CANDIDATES)
        .map(|l| FileHit {
            name: basename(l),
            path: l.to_string(),
            is_dir: std::path::Path::new(l).is_dir(),
        })
        .collect();

    let lower_q = q.to_lowercase();
    hits.sort_by_key(|h| {
        let name = h.name.to_lowercase();
        // 名字里原样包含整串的排最前
        let not_exact = !name.contains(&lower_q);
        // 用户目录优先（外置盘 /Volumes 也算用户的东西，不额外降权）
        let sys_dir = h.path.starts_with("/Library/") || h.path.starts_with("/Applications/");
        (not_exact, sys_dir, h.name.chars().count())
    });
    hits.dedup_by(|a, b| a.path == b.path);
    let total = hits.len();
    hits.truncate(MAX_HITS);
    crate::chat_follow::log_search(&format!("search result lines={} kept={}", total, hits.len()));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_chinese_splits_per_char() {
        assert_eq!(tokenize("兔采购"), vec!["兔", "采", "购"]);
    }

    #[test]
    fn tokenize_mixed() {
        assert_eq!(tokenize("bmmncs 采购"), vec!["bmmncs", "采", "购"]);
    }

    #[test]
    fn tokenize_spaces_ignored() {
        assert_eq!(tokenize("  兔  采购 "), vec!["兔", "采", "购"]);
    }

    #[test]
    fn tokenize_empty() {
        assert!(tokenize("   ").is_empty());
    }

    #[test]
    fn query_is_anded() {
        let toks = tokenize("兔采购");
        let clauses: Vec<String> = toks
            .iter()
            .map(|t| format!("kMDItemFSName == \"*{}*\"cd", escapes(t)))
            .collect();
        assert_eq!(
            clauses.join(" && "),
            "kMDItemFSName == \"*兔*\"cd && kMDItemFSName == \"*采*\"cd && kMDItemFSName == \"*购*\"cd"
        );
    }
}
