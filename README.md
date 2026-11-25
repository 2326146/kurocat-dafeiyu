# 🐳 大肥鱼桌面宠物

> 基于开源项目 **[KuroCat](https://github.com/qianyiai/kurocat)**（MIT）的二次开发版本。
> 原版是一只跟着你敲键盘的桌面小黑猫；这个版本把主角换成了「大肥鱼（鲸鱼娘）」，
> 并加入了急眼表情、珍珠白耳机、聊天气泡和多模型对话。

系统要求：macOS 10.15+（Apple Silicon / Intel）· Windows 10+

---

## 这个版本加了什么

| 功能 | 说明 |
|---|---|
| 😤 **急眼表情** | 0.8 秒内连敲 7 下以上会「急眼」，头上冒 💢，停手 2 秒后消气。用独立的短窗口检测，不影响原版 2.8 秒的腮红/开心情绪 |
| 🎧 **珍珠白耳机** | 系统有音频输出超过 5 秒就戴上耳机、飘 🎵，停 2 秒后摘下。用 macOS CoreAudio 判断，**任何播放器都适用，不需要额外权限** |
| 💬 **聊天气泡** | 她右脸颊外侧有一个三点小气泡，鼠标靠近会停住方便点中，点开是聊天卡片 |
| 📐 **自适应卡片** | 高度随内容自动生长，封顶在 16:9 手机比例，超出后内部滚动 |
| 🗂️ **多会话历史** | 左侧抽屉列出最近 50 个会话，可切换、新建、删除；历史保存在本地 |
| ⚡ **流式输出** | 回复逐字出现，不是等一大段回来才显示 |
| 🤖 **多模型** | 支持任意 OpenAI 兼容接口（DeepSeek / OpenAI / 通义 / Kimi / Ollama…），顶部下拉切换 |

另外：**已关闭上游自动更新**，否则上游发版时会把这里的定制全部覆盖。

---

## 安装

### macOS

下载 `.dmg`，打开后把 KuroCat 拖进「应用程序」。

首次打开若提示「无法验证开发者」或「已损坏」（本版本使用自签名，未经 Apple 公证）：

```bash
xattr -cr /Applications/KuroCat.app
```

然后到 **系统设置 → 隐私与安全性**，在 **辅助功能** 和 **输入监控** 里都勾上 KuroCat。
这两个权限是全局监听键鼠必需的 —— 不勾她就不会跟着你打字。

> **权限列表里出现多条同名 `KuroCat` 怎么办？**
> macOS 的授权是绑定**代码签名**的，不是绑定路径。装过多个构建（比如另外下一个测试版）
> 就会留下多条记录，而且列表里只看名字分不清。
> 判断方法：在「辅助功能」里选中某一条，看右侧显示的路径 ——
> **指向 `/Applications/` 的才是日常使用的那个**，其余可以点 `−` 删掉。
> 如果实在分不清，全删掉再重新添加 `/Applications/KuroCat.app` 最干净。

### Windows

下载 `-setup.exe` 安装。

已在 **Windows 10 22H2 (x64)** 实机验证：MSVC 工具链编译 0 错误，NSIS 安装正常，
桌宠透明显示、全局键鼠响应、「急眼」表情、聊天气泡与聊天窗均工作正常。

> 未覆盖的部分：Win7 特性分支（仅编译通过）、托盘图标外观、OBS 透明源、
> 多显示器与非 100% DPI 下的位置表现。

---

## 配置模型（可选）

宠物本体不联网，不配也能用（只是不能聊天）。

首次打开聊天窗，她会把配置文件路径告诉你。macOS 上在：

```
~/Library/Application Support/kuro-cat-pet/providers.json
```

把对应条目的 `"key": ""` 填上自己的 API Key：

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

任何 OpenAI 兼容接口都能加 —— **加一条配置即可，不用改代码**。

> API Key **不在本仓库中**，也不会打进安装包。它只在运行时由 Rust 侧读取，
> 前端拿到的只是「是否已配置」这个布尔值。

---

## 本地构建

前置：Node 18+、Rust 工具链。

```bash
npm install
npm run tauri:build -- --bundles app
```

产物在 `src-tauri/target/release/bundle/macos/KuroCat.app`。

常见问题：如果 `npm ci` 之后 vite 报找不到 esbuild 二进制，执行
`node node_modules/esbuild/install.js`（npm 11/12 会拦截依赖的安装脚本）。

---

## 发布

推一个 `v*` 标签，或手动触发 Actions，`.github/workflows/build-release.yml` 会在
macOS（Intel + Apple Silicon）和 Windows 上各构建安装包，并挂到 Draft Release。

手动触发只上传 Actions artifact、不创建 Release —— 适合先验证 Windows 能不能编过。

---

## 致谢

这个项目几乎全部的基础工作来自下面这些人和项目，非常感谢：

**代码基础**

- **[qianyiai/kurocat](https://github.com/qianyiai/kurocat)** — 整套桌宠框架：Tauri v2 + Rust 全局键鼠钩子、
  Canvas 渲染引擎、OBS 透明源、MCP 服务、多平台打包。没有它就没有这个版本。
- 上游又是受 **[Bongo-Cat-Mver](https://github.com/MMmmmoko/Bongo-Cat-Mver)** 启发而作。

**角色素材**

- **[PC2005-cloud/dsh-pet](https://github.com/PC2005-cloud/dsh-pet)** — 「大肥鱼（鲸鱼娘）」的原始素材
- **[QCYTSN/dsh-dafeiyu](https://github.com/QCYTSN/dsh-dafeiyu)** — 衍生整理版本

上面的项目均为 MIT 协议。

**本版本改动**

急眼状态机与 💢 粒子、程序化绘制的珍珠白耳机、音频检测、聊天气泡与抽屉、
多会话存储、流式输出与多模型接入。

---

## 许可

[MIT](./LICENSE)，沿用上游协议。

原版说明已留档：[README-upstream.md](./README-upstream.md)
