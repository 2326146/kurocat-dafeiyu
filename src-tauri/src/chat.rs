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

fn chat_history_path() -> PathBuf {
    crate::config::app_data_dir().join("chat-history.json")
}

const TEMPLATE: &str = r#"{
  "providers": [
    {
      "id": "deepseek",
      "name": "DeepSeek",
      "model": "deepseek-chat",
      "endpoint": "https://api.deepseek.com/chat/completions",
      "key": "",
      "temperature": 0.8
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

// ---------------- 聊天历史 ----------------

#[tauri::command]
pub fn load_chat_history() -> Result<Vec<ChatMessage>, String> {
    let path = chat_history_path();
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("读取历史失败：{e}"))?;
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(&text).map_err(|e| format!("历史文件格式错误：{e}"))
}

#[tauri::command]
pub fn save_chat_history(turns: Vec<ChatMessage>) -> Result<(), String> {
    let path = chat_history_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let json = serde_json::to_string_pretty(&turns).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| format!("写入历史失败：{e}"))
}

#[tauri::command]
pub fn clear_chat_history() -> Result<(), String> {
    let path = chat_history_path();
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| format!("清空历史失败：{e}"))?;
    }
    Ok(())
}

// ---------------- 流式对话 ----------------

/// 起一个后台线程跑 curl 并逐行解析 SSE，增量通过事件推给前端。
#[tauri::command]
pub fn send_chat_stream(
    app: AppHandle,
    provider_id: String,
    messages: Vec<ChatMessage>,
    request_id: String,
) -> Result<(), String> {
    let p = resolve_provider(&provider_id)?;

    let body = serde_json::json!({
        "model": p.model,
        "messages": messages,
        "temperature": p.temperature.unwrap_or(0.8),
        "stream": true,
    });
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

    let body = serde_json::json!({
        "model": p.model,
        "messages": messages,
        "temperature": p.temperature.unwrap_or(0.8),
        "stream": false,
    });
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
