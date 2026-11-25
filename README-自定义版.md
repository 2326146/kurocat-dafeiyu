# KuroCat · 大肥鱼自定义版

在 [qianyiai/kurocat](https://github.com/qianyiai/kurocat)（MIT）基础上做的个人定制版。
原项目是一只跟着你敲键盘的桌面宠物，这个版本把主角换成了「大肥鱼（鲸鱼娘）」，并加了几项自己的功能。

## 这个版本改了什么

| 功能 | 说明 |
|---|---|
| **急眼表情** | 0.8 秒内连敲 7 下以上会「急眼」，头上冒 💢，停手 2 秒后消气。用的是独立的短窗口检测，不影响原有的腮红/开心情绪 |
| **珍珠白耳机** | 系统有音频输出超过 5 秒就戴上耳机、飘 🎵，停止 2 秒后摘下。用 macOS CoreAudio 判断，任何播放器都适用，不需要权限 |
| **聊天气泡** | 她右脸颊外侧有一个三点小气泡，鼠标靠近会停住方便点中，点开是聊天卡片 |
| **聊天卡片** | 随内容自动长高，封顶在 16:9 手机比例，超出后内部滚动 |
| **多模型对话** | 支持任意 OpenAI 兼容接口（DeepSeek、OpenAI 等），顶部下拉切换；逐字流式输出；历史本地保存 |

## 安装

**macOS（Apple Silicon）**：下载 `.dmg`，把 KuroCat 拖进「应用程序」。

首次打开若提示「无法验证开发者」或「已损坏」：

```bash
xattr -cr /Applications/KuroCat.app
```

然后到「系统设置 → 隐私与安全性」，把 **辅助功能** 和 **输入监控** 里都勾上 KuroCat。
（这两个权限是全局监听键鼠必需的，不勾她就不会跟着你打字。）

**Windows**：下载 `-setup.exe` 安装。

## 配置模型（可选）

宠物本身不需要联网。只有想聊天时才需要配模型。

首次打开聊天窗，她会把配置文件路径告诉你。macOS 上是：

```
~/Library/Application Support/kuro-cat-pet/providers.json
```

把对应条目的 `"key": ""` 填上你自己的 API Key：

```json
{
  "providers": [
    {
      "id": "deepseek",
      "name": "DeepSeek",
      "model": "deepseek-chat",
      "endpoint": "https://api.deepseek.com/chat/completions",
      "key": "sk-你的key",
      "temperature": 0.8
    }
  ]
}
```

任何 OpenAI 兼容接口都能加进来 —— 加一条配置即可，不用改代码。

> **Key 不会进入本仓库，也不会打进二进制。** 它只在运行时由 Rust 侧从上面这个文件读取。

## 本地构建

需要 Node 18+ 与 Rust 工具链：

```bash
npm install
npm run tauri:build -- --bundles app
```

产物在 `src-tauri/target/release/bundle/macos/KuroCat.app`。

## 常见构建问题

**`npm ci` 之后 `vite` 报找不到 esbuild 二进制**

npm 11/12 默认拦截依赖的安装脚本，esbuild 的二进制不会被下载。补一行：

```bash
node node_modules/esbuild/install.js
```

（CI 里用的是 Node 20 自带的 npm 10，不受影响。）

## 发布

推一个 `v*` 标签（或手动触发 Actions），`.github/workflows/build-release.yml` 会在
macOS（Intel + Apple Silicon）和 Windows 上各构建一个安装包，并挂到 Draft Release 上。

## 关于自动更新

**本版本已关闭上游自动更新**（`src-tauri/src/updater.rs` 里的 `FORK_UPDATES_DISABLED`）。
上游发新版时，原逻辑会把应用替换成官方版，本项目的定制会被全部覆盖。

想重新启用：把该常量改成 `false`，并把 `QIANYI_API_LATEST`、`REPO_RELEASES_PAGE`
换成你自己的仓库地址。

## 许可

沿用上游的 MIT 协议。角色素材「大肥鱼（鲸鱼娘）」来自 MIT 开源项目
[PC2005-cloud/dsh-pet](https://github.com/PC2005-cloud/dsh-pet) 与
[QCYTSN/dsh-dafeiyu](https://github.com/QCYTSN/dsh-dafeiyu)。
