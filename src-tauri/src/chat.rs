//! 多模型对话接入（流式）+ 聊天历史持久化。
//!
//! 设计要点：
//! - 供应商配置放在应用数据目录的 `providers.json`，**不进仓库、不进二进制**。
//! - Key 只在 Rust 侧读取和使用，前端拿不到。
//! - DeepSeek 与 OpenAI 都是 OpenAI 兼容协议，共用同一条请求路径。
//! - 流式输出用 curl 的 `-N`，逐行解析 SSE，把增量通过事件推给聊天窗。

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

#[derive(Deserialize, Clone)]
pub struct ProviderConfig {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub model: String,
    pub endpoint: String,
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub temperature: Option<f64>,
    /// 思考模式："enabled" | "disabled"。不填则用 API 默认（开启）。
    /// DeepSeek 默认开启思考且 effort=high，桌宠场景会导致回话明显变慢。
    #[serde(default)]
    pub thinking: Option<String>,
}

#[derive(Deserialize)]
struct ProvidersFile {
    providers: Vec<ProviderConfig>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInfo {
    pub id: String,
    pub name: String,
    pub model: String,
    /// Key 与地址都填了才算可用
    pub configured: bool,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct StreamDelta {
    request_id: String,
    delta: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct StreamDone {
    request_id: String,
    text: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct StreamError {
    request_id: String,
    message: String,
}

pub fn providers_path() -> PathBuf {
    crate::config::app_data_dir().join("providers.json")
}

const TEMPLATE: &str = r#"{
  "providers": [
    {
      "id": "deepseek",
      "name": "DeepSeek",
      "model": "deepseek-v4-pro",
      "endpoint": "https://api.deepseek.com/chat/completions",
      "key": "",
      "temperature": 0.8,
      "thinking": "disabled"
    },
    {
      "id": "openai",
      "name": "OpenAI",
      "model": "gpt-4o-mini",
      "endpoint": "https://api.openai.com/v1/chat/completions",
      "key": "",
      "temperature": 0.8
    }
  ]
}
"#;

/// 读配置；文件不存在时写一份模板并返回空列表。
fn load_providers() -> Result<Vec<ProviderConfig>, String> {
    let path = providers_path();
    if !path.exists() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        std::fs::write(&path, TEMPLATE).map_err(|e| format!("写入配置模板失败: {e}"))?;
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("读取配置失败: {e}"))?;
    let parsed: ProvidersFile =
        serde_json::from_str(&text).map_err(|e| format!("providers.json 格式错误: {e}"))?;
    Ok(parsed.providers)
}

fn resolve_provider(provider_id: &str) -> Result<ProviderConfig, String> {
    let providers = load_providers()?;
    if providers.is_empty() {
        return Err(format!(
            "还没有配置模型。请编辑：{}",
            providers_path().to_string_lossy()
        ));
    }
    let p = providers
        .into_iter()
        .find(|x| x.id == provider_id)
        .ok_or_else(|| format!("找不到模型配置：{provider_id}"))?;
    if p.key.trim().is_empty() {
        return Err(format!(
            "「{}」还没填 API Key。打开这个文件填进 key 字段：\n{}",
            p.name,
            providers_path().to_string_lossy()
        ));
    }
    Ok(p)
}

#[tauri::command]
pub fn list_chat_providers() -> Result<Vec<ProviderInfo>, String> {
    let list = load_providers()?;
    Ok(list
        .into_iter()
        .map(|p| ProviderInfo {
            configured: !p.key.trim().is_empty() && !p.endpoint.trim().is_empty(),
            id: p.id,
            name: p.name,
            model: p.model,
        })
        .collect())
}

#[tauri::command]
pub fn chat_providers_path() -> String {
    providers_path().to_string_lossy().to_string()
}

// ---------------- 对话记录（多会话） ----------------

#[derive(Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
    pub messages: Vec<ChatMessage>,
}

#[derive(Deserialize, Serialize, Default)]
struct ConversationsFile {
    conversations: Vec<Conversation>,
}

/// 最多保留的会话数量
const MAX_CONVERSATIONS: usize = 50;

fn conversations_path() -> PathBuf {
    crate::config::app_data_dir().join("conversations.json")
}

fn legacy_history_path() -> PathBuf {
    crate::config::app_data_dir().join("chat-history.json")
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn read_conversations() -> Vec<Conversation> {
    if !conversations_path().exists() {
        migrate_legacy_history();
    }
    let Ok(text) = std::fs::read_to_string(conversations_path()) else {
        return Vec::new();
    };
    serde_json::from_str::<ConversationsFile>(&text)
        .map(|f| f.conversations)
        .unwrap_or_default()
}

fn write_conversations(list: &[Conversation]) -> Result<(), String> {
    let path = conversations_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let payload = ConversationsFile {
        conversations: list.to_vec(),
    };
    let json = serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| format!("写入对话记录失败：{e}"))
}

/// 把旧的单文件历史（扁平数组）迁移成一条会话，原文件改名为 .bak 保留。
fn migrate_legacy_history() {
    let legacy = legacy_history_path();
    if !legacy.exists() {
        return;
    }
    let Ok(text) = std::fs::read_to_string(&legacy) else {
        return;
    };
    let Ok(messages) = serde_json::from_str::<Vec<ChatMessage>>(&text) else {
        return;
    };
    if !messages.is_empty() {
        let title = messages
            .iter()
            .find(|m| m.role == "user")
            .map(|m| m.content.chars().take(14).collect::<String>())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "历史对话".to_string());
        let ts = now_millis();
        let conv = Conversation {
            id: format!("conv-{ts}"),
            title,
            created_at: ts,
            updated_at: ts,
            messages,
        };
        let _ = write_conversations(&[conv]);
    }
    let _ = std::fs::rename(&legacy, legacy.with_extension("json.bak"));
}

#[tauri::command]
pub fn list_conversations() -> Result<Vec<Conversation>, String> {
    let mut list = read_conversations();
    list.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(list)
}

#[tauri::command]
pub fn save_conversation(conv: Conversation) -> Result<(), String> {
    let mut list = read_conversations();
    match list.iter_mut().find(|c| c.id == conv.id) {
        Some(slot) => *slot = conv,
        None => list.push(conv),
    }
    list.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    if list.len() > MAX_CONVERSATIONS {
        list.truncate(MAX_CONVERSATIONS);
    }
    write_conversations(&list)
}

#[tauri::command]
pub fn delete_conversation(id: String) -> Result<(), String> {
    let mut list = read_conversations();
    list.retain(|c| c.id != id);
    write_conversations(&list)
}

// ---------------- 流式对话 ----------------

/// 起一个后台线程跑 curl 并逐行解析 SSE，增量通过事件推给前端。
#[tauri::command]
pub fn send_chat_stream(
    app: AppHandle,
    provider_id: String,
    messages: Vec<ChatMessage>,
    request_id: String,
    // 前端工具条的「深度思考」开关；Some 时覆盖配置里的 thinking
    thinking: Option<String>,
) -> Result<(), String> {
    let p = resolve_provider(&provider_id)?;
    let thinking = thinking.or_else(|| p.thinking.clone());

    let mut body = serde_json::json!({
        "model": p.model,
        "messages": messages,
        "temperature": p.temperature.unwrap_or(0.8),
        "stream": true,
    });
    if let Some(t) = thinking.as_deref() {
        body["thinking"] = serde_json::json!({ "type": t });
    }
    let payload = serde_json::to_vec(&body).map_err(|e| e.to_string())?;

    let mut child = Command::new("curl")
        .args([
            "-sS",
            "-N",
            "--max-time",
            "120",
            "-X",
            "POST",
            &p.endpoint,
            "-H",
            "Content-Type: application/json",
            "-H",
            &format!("Authorization: Bearer {}", p.key.trim()),
            "--data-binary",
            "@-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("无法调用 curl：{e}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(&payload)
            .map_err(|e| format!("发送请求体失败：{e}"))?;
    }

    let stdout = child.stdout.take().ok_or("拿不到 curl 输出")?;
    let stderr = child.stderr.take();

    std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        let mut acc = String::new();
        let mut error_sent = false;

        for raw in reader.lines() {
            let Ok(line) = raw else { break };
            let line = line.trim();
            if line.is_empty() || line.starts_with(':') {
                continue;
            }

            let payload = match line.strip_prefix("data:") {
                Some(rest) => rest.trim(),
                None => {
                    // 非 SSE 行：可能是一整段 JSON 错误体
                    if line.starts_with('{') {
                        if let Some(msg) = extract_error(line) {
                            let _ = app.emit(
                                "kuro-chat-error",
                                StreamError {
                                    request_id: request_id.clone(),
                                    message: format!("接口报错：{msg}"),
                                },
                            );
                            error_sent = true;
                            break;
                        }
                    }
                    continue;
                }
            };

            if payload == "[DONE]" {
                break;
            }
            if let Some(msg) = extract_error(payload) {
                let _ = app.emit(
                    "kuro-chat-error",
                    StreamError {
                        request_id: request_id.clone(),
                        message: format!("接口报错：{msg}"),
                    },
                );
                error_sent = true;
                break;
            }
            if let Some(delta) = extract_delta(payload) {
                if !delta.is_empty() {
                    acc.push_str(&delta);
                    let _ = app.emit(
                        "kuro-chat-delta",
                        StreamDelta {
                            request_id: request_id.clone(),
                            delta,
                        },
                    );
                }
            }
        }

        let _ = child.wait();

        if error_sent {
            return;
        }
        if acc.trim().is_empty() {
            let mut detail = String::new();
            if let Some(mut se) = stderr {
                let _ = se.read_to_string(&mut detail);
            }
            let message = if detail.trim().is_empty() {
                "没有收到任何内容（可能是网络问题，或该模型不支持流式）".to_string()
            } else {
                detail.trim().to_string()
            };
            let _ = app.emit(
                "kuro-chat-error",
                StreamError {
                    request_id,
                    message,
                },
            );
        } else {
            let _ = app.emit(
                "kuro-chat-done",
                StreamDone {
                    request_id,
                    text: acc,
                },
            );
        }
    });

    Ok(())
}

fn extract_error(payload: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(payload).ok()?;
    v.get("error")?
        .get("message")?
        .as_str()
        .map(|s| s.to_string())
}

fn extract_delta(payload: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(payload).ok()?;
    v.get("choices")?
        .get(0)?
        .get("delta")?
        .get("content")?
        .as_str()
        .map(|s| s.to_string())
}

/// 非流式版本，保留作为调试/兜底。
#[tauri::command]
pub fn send_chat(provider_id: String, messages: Vec<ChatMessage>) -> Result<String, String> {
    let p = resolve_provider(&provider_id)?;

    let mut body = serde_json::json!({
        "model": p.model,
        "messages": messages,
        "temperature": p.temperature.unwrap_or(0.8),
        "stream": false,
    });
    if let Some(t) = p.thinking.as_deref() {
        body["thinking"] = serde_json::json!({ "type": t });
    }
    let payload = serde_json::to_vec(&body).map_err(|e| e.to_string())?;

    let mut child = Command::new("curl")
        .args([
            "-sS",
            "--max-time",
            "60",
            "-X",
            "POST",
            &p.endpoint,
            "-H",
            "Content-Type: application/json",
            "-H",
            &format!("Authorization: Bearer {}", p.key.trim()),
            "--data-binary",
            "@-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("无法调用 curl：{e}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(&payload)
            .map_err(|e| format!("发送请求体失败：{e}"))?;
    }

    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();

    if stdout.trim().is_empty() {
        let detail = if stderr.trim().is_empty() {
            "（无输出，可能是网络不通）".to_string()
        } else {
            stderr.trim().to_string()
        };
        return Err(format!("请求失败：{detail}"));
    }

    if let Some(msg) = extract_error(&stdout) {
        return Err(format!("接口报错：{msg}"));
    }

    let value: serde_json::Value = serde_json::from_str(&stdout)
        .map_err(|_| format!("返回内容不是 JSON：{}", &stdout[..stdout.len().min(300)]))?;

    let content = value
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .ok_or_else(|| format!("返回里没有回复内容：{}", &stdout[..stdout.len().min(300)]))?;

    Ok(content.trim().to_string())
}

// ---------------- 「深入」与导出 ----------------

fn downloads_dir() -> PathBuf {
    if cfg!(target_os = "macos") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join("Downloads");
        }
    }
    if cfg!(windows) {
        if let Ok(profile) = std::env::var("USERPROFILE") {
            return PathBuf::from(profile).join("Downloads");
        }
    }
    crate::config::app_data_dir()
}

/// 把一段对话导出成 Markdown 文件，放在下载文件夹，返回完整路径。
#[tauri::command]
pub fn export_conversation(title: String, messages: Vec<ChatMessage>) -> Result<String, String> {
    let dir = downloads_dir();
    let _ = std::fs::create_dir_all(&dir);

    let stamp = now_millis();
    let safe: String = title
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .take(24)
        .collect();
    let name = format!("大肥鱼对话-{}-{}.md", if safe.is_empty() { "未命名" } else { &safe }, stamp);
    let path = dir.join(&name);

    let mut md = String::new();
    md.push_str(&format!("# 大肥鱼对话 · {}\n\n", title));
    md.push_str(&format!("导出时间：{}（Unix {}）\n\n---\n\n", now_millis(), stamp));
    for m in &messages {
        let who = match m.role.as_str() {
            "user" => "我",
            "assistant" => "大肥鱼",
            _ => &m.role,
        };
        md.push_str(&format!("**{}**\n\n{}\n\n---\n\n", who, m.content));
    }
    md.push_str("\n> 由 KuroCat 大肥鱼自定义版导出\n");

    std::fs::write(&path, md).map_err(|e| format!("写入失败：{e}"))?;
    Ok(path.to_string_lossy().to_string())
}

/// 把文本写进系统剪贴板（macOS 用 pbcopy，Windows 用 clip）。
#[tauri::command]
pub fn copy_text(text: String) -> Result<(), String> {
    use std::io::Write;
    let bin = if cfg!(target_os = "macos") { "pbcopy" } else { "clip" };
    let mut child = Command::new(bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("无法调用 {bin}：{e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(text.as_bytes())
            .map_err(|e| format!("写入剪贴板失败：{e}"))?;
    }
    child.wait().map_err(|e| format!("{bin} 执行失败：{e}"))?;
    Ok(())
}
