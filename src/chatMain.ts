import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { currentMonitor, getCurrentWindow } from '@tauri-apps/api/window';
import { LogicalSize, PhysicalPosition } from '@tauri-apps/api/dpi';

const win = getCurrentWindow();

const cardEl = document.getElementById('chat-card') as HTMLDivElement;
const headEl = cardEl.querySelector('.chat-head') as HTMLElement;

// 标题栏可拖动整个聊天窗；按钮和下拉框除外
headEl.addEventListener('pointerdown', (ev) => {
  const target = ev.target as HTMLElement | null;
  if (target && target.closest('button, select')) return;
  ev.preventDefault();
  void win.startDragging()
    .then(() => invoke('sync_offset_from_frontend'))
    .catch(() => {});
});

// 兜底：即使 startDragging 提前返回，松手时也同步一次
window.addEventListener('pointerup', () => {
  void invoke('sync_offset_from_frontend');
});
const logEl = document.getElementById('chat-log') as HTMLDivElement;
const formEl = document.getElementById('chat-form') as HTMLFormElement;
const inputEl = document.getElementById('chat-input') as HTMLTextAreaElement;
const sendEl = formEl.querySelector('.chat-send') as HTMLButtonElement;
const collapseEl = document.getElementById('btn-chat-collapse') as HTMLButtonElement;
const newEl = document.getElementById('btn-chat-new') as HTMLButtonElement;
const modelEl = document.getElementById('chat-model') as HTMLSelectElement;
const hotzoneEl = document.getElementById('chat-hotzone') as HTMLDivElement;
const drawerEl = document.getElementById('chat-drawer') as HTMLElement;
const maskEl = document.getElementById('chat-drawer-mask') as HTMLDivElement;
const drawerListEl = document.getElementById('drawer-list') as HTMLDivElement;

interface ProviderInfo {
  id: string;
  name: string;
  model: string;
  configured: boolean;
}

interface ChatTurn {
  role: 'system' | 'user' | 'assistant';
  content: string;
}

interface Conversation {
  id: string;
  title: string;
  createdAt: number;
  updatedAt: number;
  messages: ChatTurn[];
}

const SYSTEM_PROMPT =
  '你是「大肥鱼」，一只住在用户桌面上的鲸鱼娘桌宠，会在用户敲键盘时陪着一起敲。' +
  '说话简短自然，一次一两句，可以用颜文字，但不要长篇大论，不要用列表。' +
  // 只要求「简短」会让模型把不确定的内容硬编成一句话交差（实测把沙吉斯说成了秘密军组织），
  // 所以这里明确允许在真的需要时展开，并禁止瞎编。
  '如果用户问的是真的想知道答案的问题（历史、常识、专业内容），' +
  '要给出准确、有内容的回答，可以说三到五句 —— 可爱和准确不冲突。' +
  '不确定、不知道的事就直说不知道，不要编。';

const MIN_HEIGHT = 176;
/** 高度上限按聊天卡片自身宽度算（抽屉是覆盖式，不改变窗口宽度） */
const CARD_WIDTH = 268;
const MAX_ASPECT = 16 / 9;
const CHROME_Y = 8 * 2 + 2;
const MAX_CONTEXT = 12;
const MAX_RENDERED = 30;
const TITLE_CHARS = 14;

/** 当前会话的消息（不含 system） */
let history: ChatTurn[] = [];
let conversations: Conversation[] = [];
let currentId: string | null = null;
let currentTitle = '';
let currentCreatedAt = 0;

let sending = false;
let fitting = false;
let pendingFit = false;
let lastFitAt = 0;
let cachedProviders: ProviderInfo[] | null = null;
let configHintShown = false;
let requestSeq = 0;
let saveTimer: number | null = null;

let drawerOpen = false;
let drawerCooldownUntil = 0;
let hotTimer: number | null = null;
let closeTimer: number | null = null;

let activeRequestId: string | null = null;
let activeBubble: HTMLDivElement | null = null;
let activeText = '';

// ---------- 窗口高度自适应 ----------

function appendMessage(role: 'user' | 'bot', text: string, pending = false): HTMLDivElement {
  const el = document.createElement('div');
  el.className = `chat-msg ${role}${pending ? ' pending' : ''}`;
  el.textContent = text;
  logEl.appendChild(el);
  while (logEl.childElementCount > MAX_RENDERED) logEl.removeChild(logEl.firstElementChild!);
  scheduleFit(true);
  return el;
}

async function fitHeight(scrollToBottom: boolean): Promise<void> {
  if (fitting) {
    pendingFit = true;
    return;
  }
  fitting = true;
  try {
    const sf = await win.scaleFactor();
    const inner = await win.innerSize();
    const pos = await win.outerPosition();

    const widthLogical = inner.width / sf;
    const maxHeight = Math.round(Math.min(widthLogical, CARD_WIDTH) * MAX_ASPECT);

    const headH = headEl.getBoundingClientRect().height;
    const inputH = formEl.getBoundingClientRect().height;

    const logStyle = window.getComputedStyle(logEl);
    const padY =
      parseFloat(logStyle.paddingTop || '0') + parseFloat(logStyle.paddingBottom || '0');
    const gap = parseFloat(logStyle.rowGap || '0') || 0;
    const msgs = Array.from(logEl.children) as HTMLElement[];
    const logContentH =
      msgs.reduce((sum, el) => sum + el.offsetHeight, 0) +
      Math.max(0, msgs.length - 1) * gap +
      padY;

    const targetLogical = Math.max(
      MIN_HEIGHT,
      Math.min(maxHeight, Math.ceil(headH + inputH + logContentH + CHROME_Y)),
    );
    const targetPhys = Math.round(targetLogical * sf);
    if (Math.abs(targetPhys - inner.height) <= 1) {
      if (scrollToBottom) logEl.scrollTop = logEl.scrollHeight;
      return;
    }

    const bottom = pos.y + inner.height;
    let nextY = bottom - targetPhys;
    try {
      const monitor = await currentMonitor();
      if (monitor) {
        const top = monitor.position.y + Math.round(8 * sf);
        if (nextY < top) nextY = top;
      }
    } catch {
      // 取不到显示器信息就按原位置走
    }

    await win.setSize(new LogicalSize(widthLogical, targetLogical));
    await win.setPosition(new PhysicalPosition(pos.x, nextY));
    // 窗口为保持底边而移动了，告诉 Rust 重建跟随偏移
    void invoke('sync_offset_from_frontend');
    if (scrollToBottom) logEl.scrollTop = logEl.scrollHeight;
  } catch {
    // 窗口关闭中等情况，忽略
  } finally {
    fitting = false;
    if (pendingFit) {
      pendingFit = false;
      void fitHeight(scrollToBottom);
    }
  }
}

function scheduleFit(scrollToBottom: boolean): void {
  window.requestAnimationFrame(() => void fitHeight(scrollToBottom));
}

function fitSoon(): void {
  const now = performance.now();
  if (now - lastFitAt < 150) return;
  lastFitAt = now;
  scheduleFit(true);
}

function autoGrow(): void {
  inputEl.style.height = 'auto';
  inputEl.style.height = `${Math.min(inputEl.scrollHeight, 62)}px`;
  scheduleFit(false);
}

// ---------- 会话存取 ----------

function shortTitle(text: string): string {
  const chars = Array.from(text.trim());
  return chars.length > TITLE_CHARS ? `${chars.slice(0, TITLE_CHARS).join('')}…` : chars.join('');
}

function persistConversation(): void {
  if (saveTimer !== null) window.clearTimeout(saveTimer);
  saveTimer = window.setTimeout(() => {
    saveTimer = null;
    if (!currentId || history.length === 0) return;
    const conv: Conversation = {
      id: currentId,
      title: currentTitle || shortTitle(history[0]?.content ?? ''),
      createdAt: currentCreatedAt || Date.now(),
      updatedAt: Date.now(),
      messages: history.slice(),
    };
    const idx = conversations.findIndex((c) => c.id === conv.id);
    if (idx >= 0) conversations[idx] = conv;
    else conversations.unshift(conv);
    conversations.sort((a, b) => b.updatedAt - a.updatedAt);
    void invoke('save_conversation', { conv }).catch(() => {
      // 落盘失败不打断对话
    });
    if (drawerOpen) renderDrawer();
  }, 400);
}

function renderLog(): void {
  logEl.innerHTML = '';
  for (const turn of history.slice(-MAX_RENDERED)) {
    const el = document.createElement('div');
    el.className = `chat-msg ${turn.role === 'user' ? 'user' : 'bot'}`;
    el.textContent = turn.content;
    logEl.appendChild(el);
  }
  scheduleFit(true);
}

function startNewConversation(): void {
  if (sending) return;
  currentId = null;
  currentTitle = '';
  currentCreatedAt = 0;
  history = [];
  logEl.innerHTML = '';
  appendMessage('bot', '我在呢。想聊点什么？');
  closeDrawer();
  inputEl.focus();
}

function switchTo(conv: Conversation): void {
  if (sending) return;
  currentId = conv.id;
  currentTitle = conv.title;
  currentCreatedAt = conv.createdAt;
  history = conv.messages.slice();
  renderLog();
  closeDrawer();
  inputEl.focus();
}

async function removeConversation(id: string): Promise<void> {
  conversations = conversations.filter((c) => c.id !== id);
  try {
    await invoke('delete_conversation', { id });
  } catch {
    // 忽略
  }
  if (currentId === id) startNewConversation();
  else renderDrawer();
}

function formatTime(ts: number): string {
  if (!ts) return '';
  const diff = Date.now() - ts;
  if (diff < 60_000) return '刚刚';
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)} 分钟前`;
  if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)} 小时前`;
  const d = new Date(ts);
  return `${d.getMonth() + 1} 月 ${d.getDate()} 日`;
}

function renderDrawer(): void {
  drawerListEl.innerHTML = '';
  if (conversations.length === 0) {
    const empty = document.createElement('div');
    empty.className = 'drawer-empty';
    empty.textContent = '还没有历史对话';
    drawerListEl.appendChild(empty);
    return;
  }
  for (const conv of conversations) {
    const item = document.createElement('button');
    item.type = 'button';
    item.className = `drawer-item${conv.id === currentId ? ' active' : ''}`;

    const title = document.createElement('span');
    title.className = 'drawer-item-title';
    title.textContent = conv.title || '未命名对话';

    const time = document.createElement('span');
    time.className = 'drawer-item-time';
    time.textContent = formatTime(conv.updatedAt);

    const del = document.createElement('button');
    del.type = 'button';
    del.className = 'drawer-item-del';
    del.title = '删除这个对话';
    del.textContent = '×';
    del.addEventListener('click', (ev) => {
      ev.stopPropagation();
      void removeConversation(conv.id);
    });

    item.appendChild(title);
    item.appendChild(time);
    item.appendChild(del);
    item.addEventListener('click', () => switchTo(conv));
    drawerListEl.appendChild(item);
  }
}

// ---------- 抽屉 ----------

function openDrawer(): void {
  if (drawerOpen) return;
  drawerOpen = true;
  renderDrawer();
  drawerEl.classList.add('open');
  maskEl.classList.add('open');
}

function closeDrawer(): void {
  if (!drawerOpen) return;
  drawerOpen = false;
  drawerEl.classList.remove('open');
  maskEl.classList.remove('open');
  // 关闭后短暂忽略热区，避免刚移开又立刻弹出
  drawerCooldownUntil = performance.now() + 400;
}

// ---------- 模型列表 ----------

async function loadProviders(): Promise<ProviderInfo[]> {
  try {
    const list = await invoke<ProviderInfo[]>('list_chat_providers');
    modelEl.innerHTML = '';
    for (const p of list) {
      const opt = document.createElement('option');
      opt.value = p.id;
      opt.textContent = p.configured ? p.name : `${p.name}（未配置）`;
      modelEl.appendChild(opt);
    }
    if ((list.length === 0 || !list.some((p) => p.configured)) && !configHintShown) {
      configHintShown = true;
      const path = await invoke<string>('chat_providers_path');
      appendMessage('bot', `模型还没填 API Key。配置文件在这里：\n${path}`);
    }
    cachedProviders = list;
    return list;
  } catch (err) {
    appendMessage('bot', `读取模型配置失败：${String(err)}`);
    return [];
  }
}

// ---------- 发送（流式） ----------

function finishRequest(finalText: string | null, errorText: string | null): void {
  const bubble = activeBubble;
  activeRequestId = null;
  activeBubble = null;

  if (bubble) {
    bubble.classList.remove('pending');
    if (errorText !== null) {
      bubble.textContent = `⚠️ ${errorText}`;
    } else {
      bubble.textContent = finalText ?? activeText;
    }
  }

  if (errorText !== null) {
    history.pop();
  } else {
    history.push({ role: 'assistant', content: finalText ?? activeText });
  }
  persistConversation();

  if (bubble && errorText === null) {
    addActions(bubble, finalText ?? '');
  }

  activeText = '';
  sending = false;
  sendEl.disabled = false;
  scheduleFit(true);
  inputEl.focus();
}

async function send(): Promise<void> {
  const text = inputEl.value.trim();
  if (!text || sending) return;

  if (!cachedProviders) await loadProviders();
  const providerId = modelEl.value || cachedProviders?.[0]?.id;
  if (!providerId) {
    appendMessage('bot', '还没有可用的模型，先去配置一下吧。');
    return;
  }

  // 第一条消息才把这个会话建档
  if (!currentId) {
    currentId = `conv-${Date.now()}`;
    currentCreatedAt = Date.now();
    currentTitle = shortTitle(text);
  }

  appendMessage('user', text);
  history.push({ role: 'user', content: text });
  inputEl.value = '';
  autoGrow();

  sending = true;
  sendEl.disabled = true;

  activeText = '';
  activeRequestId = `req-${Date.now()}-${++requestSeq}`;
  activeBubble = appendMessage('bot', '……', true);

  const messages: ChatTurn[] = [
    { role: 'system', content: SYSTEM_PROMPT },
    ...history.slice(-MAX_CONTEXT),
  ];

  try {
    await invoke('send_chat_stream', {
      providerId,
      messages,
      requestId: activeRequestId,
      thinking: thinkingOn ? 'enabled' : 'disabled',
    });
  } catch (err) {
    finishRequest(null, String(err));
  }
}

// ---------- 深度思考开关 ----------

const THINK_KEY = 'kuro-chat-thinking';
let thinkingOn = localStorage.getItem(THINK_KEY) === 'on';

const thinkEl = document.getElementById('toggle-thinking') as HTMLButtonElement | null;

function renderThinking(): void {
  if (!thinkEl) return;
  thinkEl.setAttribute('aria-pressed', String(thinkingOn));
  thinkEl.textContent = thinkingOn ? '🧠 深度思考 开' : '🧠 深度思考';
}

if (thinkEl) {
  thinkEl.addEventListener('click', () => {
    thinkingOn = !thinkingOn;
    localStorage.setItem(THINK_KEY, thinkingOn ? 'on' : 'off');
    renderThinking();
  });
}
renderThinking();

// ---------- 「深入」与导出 ----------

let toastEl: HTMLDivElement | null = null;
let toastTimer: number | null = null;

function showToast(msg: string): void {
  if (!toastEl) {
    toastEl = document.createElement('div');
    toastEl.className = 'chat-toast';
    document.body.appendChild(toastEl);
  }
  toastEl.textContent = msg;
  toastEl.classList.add('show');
  if (toastTimer !== null) window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => {
    toastEl?.classList.remove('show');
    toastTimer = null;
  }, 3200);
}

async function goDeeper(): Promise<void> {
  const turns = history.slice(-6);
  if (turns.length === 0) return;
  const body = turns
    .map((t) => (t.role === 'user' ? '我' : '大肥鱼') + '：' + t.content)
    .join('\n\n');
  const payload = '以下是我和桌宠的对话，请帮我更深入地展开：\n\n' + body;
  try {
    await invoke('copy_text', { text: payload });
    await invoke('open_release_page', { url: 'https://chat.deepseek.com/' });
    showToast('对话已复制，粘到输入框即可');
  } catch (err) {
    showToast('操作失败：' + String(err));
  }
}

async function exportConversation(): Promise<void> {
  if (history.length === 0) return;
  try {
    const path = await invoke<string>('export_conversation', {
      title: currentTitle || '对话',
      messages: history,
    });
    showToast('已导出到下载文件夹：' + (path.split('/').pop() ?? path));
  } catch (err) {
    showToast('导出失败：' + String(err));
  }
}

function addActions(bubble: HTMLDivElement, text: string): void {
  // 只有长回答才给「深入」，短回答加了也是噪音
  if (Array.from(text).length < 200) return;
  const row = document.createElement('div');
  row.className = 'chat-actions';

  const deep = document.createElement('button');
  deep.type = 'button';
  deep.className = 'chat-action';
  deep.textContent = '🔍 深入';
  deep.title = '复制这段对话，并打开 DeepSeek 网页版继续';
  deep.addEventListener('click', () => void goDeeper());

  const exp = document.createElement('button');
  exp.type = 'button';
  exp.className = 'chat-action';
  exp.textContent = '📄 导出对话';
  exp.title = '导出成 Markdown 文件，可以拖进网页版当附件';
  exp.addEventListener('click', () => void exportConversation());

  row.append(deep, exp);
  bubble.insertAdjacentElement('afterend', row);
}

// ---------- 流式事件 ----------

void listen<{ requestId: string; delta: string }>('kuro-chat-delta', (ev) => {
  if (!activeRequestId || ev.payload.requestId !== activeRequestId) return;
  activeText += ev.payload.delta;
  if (activeBubble) activeBubble.textContent = activeText;
  fitSoon();
});

void listen<{ requestId: string; text: string }>('kuro-chat-done', (ev) => {
  if (!activeRequestId || ev.payload.requestId !== activeRequestId) return;
  finishRequest(ev.payload.text, null);
});

void listen<{ requestId: string; message: string }>('kuro-chat-error', (ev) => {
  if (!activeRequestId || ev.payload.requestId !== activeRequestId) return;
  finishRequest(null, ev.payload.message);
});

// ---------- 交互事件 ----------

function hideWindow(): void {
  void win.hide();
}

inputEl.addEventListener('input', autoGrow);

inputEl.addEventListener('keydown', (ev) => {
  if (ev.isComposing || ev.keyCode === 229) return;
  if (ev.key === 'Enter' && !ev.shiftKey) {
    ev.preventDefault();
    void send();
    return;
  }
  if (ev.key === 'Escape') {
    ev.preventDefault();
    if (drawerOpen) closeDrawer();
    else hideWindow();
  }
});

formEl.addEventListener('submit', (ev) => {
  ev.preventDefault();
  void send();
});

collapseEl.addEventListener('click', () => hideWindow());
newEl.addEventListener('click', () => startNewConversation());

hotzoneEl.addEventListener('mouseenter', () => {
  if (performance.now() < drawerCooldownUntil) return;
  if (hotTimer !== null) window.clearTimeout(hotTimer);
  hotTimer = window.setTimeout(() => {
    hotTimer = null;
    openDrawer();
  }, 200);
});

hotzoneEl.addEventListener('mouseleave', () => {
  if (hotTimer !== null) {
    window.clearTimeout(hotTimer);
    hotTimer = null;
  }
});

drawerEl.addEventListener('mouseleave', () => {
  if (closeTimer !== null) window.clearTimeout(closeTimer);
  closeTimer = window.setTimeout(() => {
    closeTimer = null;
    closeDrawer();
  }, 150);
});

drawerEl.addEventListener('mouseenter', () => {
  if (closeTimer !== null) {
    window.clearTimeout(closeTimer);
    closeTimer = null;
  }
});

maskEl.addEventListener('click', () => closeDrawer());

// ---------- 启动 ----------

async function bootstrap(): Promise<void> {
  try {
    conversations = await invoke<Conversation[]>('list_conversations');
  } catch {
    conversations = [];
  }
  conversations.sort((a, b) => b.updatedAt - a.updatedAt);

  if (conversations.length > 0) {
    switchTo(conversations[0]);
  } else {
    appendMessage('bot', '我在呢。想聊点什么？');
  }

  inputEl.focus();
  autoGrow();
  await loadProviders();

  void win.onFocusChanged(({ payload: focused }) => {
    if (!focused) {
      closeDrawer();
      return;
    }
    cachedProviders = null;
    void loadProviders();
  });
}

void bootstrap();
