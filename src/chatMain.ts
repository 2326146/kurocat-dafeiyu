import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { currentMonitor, getCurrentWindow } from '@tauri-apps/api/window';
import { LogicalSize, PhysicalPosition } from '@tauri-apps/api/dpi';

const win = getCurrentWindow();

const cardEl = document.getElementById('chat-card') as HTMLDivElement;
const headEl = cardEl.querySelector('.chat-head') as HTMLElement;
const logEl = document.getElementById('chat-log') as HTMLDivElement;
const formEl = document.getElementById('chat-form') as HTMLFormElement;
const inputEl = document.getElementById('chat-input') as HTMLTextAreaElement;
const sendEl = formEl.querySelector('.chat-send') as HTMLButtonElement;
const collapseEl = document.getElementById('btn-chat-collapse') as HTMLButtonElement;
const clearEl = document.getElementById('btn-chat-clear') as HTMLButtonElement;
const modelEl = document.getElementById('chat-model') as HTMLSelectElement;

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

const SYSTEM_PROMPT =
  '你是「大肥鱼」，一只住在用户桌面上的鲸鱼娘桌宠，会在用户敲键盘时陪着一起敲。' +
  '说话简短自然，一次一两句，可以用颜文字，但不要长篇大论，不要用列表。';

const MIN_HEIGHT = 176;
const MAX_ASPECT = 16 / 9;
const CHROME_Y = 8 * 2 + 2;
/** 每次请求带上的最大历史轮数 */
const MAX_CONTEXT = 12;
/** 落盘保留的最大条数 */
const MAX_SAVED = 60;
/** 界面上最多渲染的条数 */
const MAX_RENDERED = 30;

/** 对话历史（不含 system，便于直接落盘） */
let history: ChatTurn[] = [];

let sending = false;
let fitting = false;
let pendingFit = false;
let lastFitAt = 0;
let cachedProviders: ProviderInfo[] | null = null;
let configHintShown = false;
let requestSeq = 0;

/** 当前流式请求 */
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
    const maxHeight = Math.round(widthLogical * MAX_ASPECT);

    const headH = headEl.getBoundingClientRect().height;
    const inputH = formEl.getBoundingClientRect().height;

    // 不能用 logEl.scrollHeight：log 是 flex:1，内容少时它会返回当前元素高度，
    // 导致窗口永远缩不回去。改为逐条累加消息实际高度。
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

    // 底边固定：高度增加多少，窗口就往上移多少
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
    if (scrollToBottom) logEl.scrollTop = logEl.scrollHeight;
  } catch {
    // 窗口正在关闭等情况，忽略
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

/** 流式期间用：150ms 内最多量一次，避免每个字都触发窗口重排 */
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

// ---------- 历史持久化 ----------

let saveTimer: number | null = null;

function persistHistory(): void {
  if (saveTimer !== null) window.clearTimeout(saveTimer);
  saveTimer = window.setTimeout(() => {
    saveTimer = null;
    const trimmed = history.slice(-MAX_SAVED);
    void invoke('save_chat_history', { turns: trimmed }).catch(() => {
      // 落盘失败不打断对话
    });
  }, 400);
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
    // 失败的这一轮不写进历史
    history.pop();
    persistHistory();
  } else {
    history.push({ role: 'assistant', content: finalText ?? activeText });
    persistHistory();
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
    });
  } catch (err) {
    finishRequest(null, String(err));
  }
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

// ---------- 事件 ----------

function hideWindow(): void {
  void win.hide();
}

inputEl.addEventListener('input', autoGrow);

inputEl.addEventListener('keydown', (ev) => {
  // 输入法选词期间的 Enter 交给输入法，不能当发送
  if (ev.isComposing || ev.keyCode === 229) return;
  if (ev.key === 'Enter' && !ev.shiftKey) {
    ev.preventDefault();
    void send();
    return;
  }
  if (ev.key === 'Escape') {
    ev.preventDefault();
    hideWindow();
  }
});

formEl.addEventListener('submit', (ev) => {
  ev.preventDefault();
  void send();
});

collapseEl.addEventListener('click', () => hideWindow());

// 两段式确认：不依赖原生 confirm 对话框（WebView 里不一定实现）
let clearArmed = false;
let clearTimer: number | null = null;

function disarmClear(): void {
  clearArmed = false;
  if (clearTimer !== null) window.clearTimeout(clearTimer);
  clearTimer = null;
  clearEl.textContent = '⌫';
  clearEl.title = '清空对话';
}

clearEl.addEventListener('click', async () => {
  if (sending) return;
  if (!clearArmed) {
    clearArmed = true;
    clearEl.textContent = '✓';
    clearEl.title = '再点一次确认清空';
    clearTimer = window.setTimeout(disarmClear, 3000);
    return;
  }
  disarmClear();
  try {
    await invoke('clear_chat_history');
  } catch {
    // 文件本来就不存在时会被忽略
  }
  history.length = 0;
  logEl.innerHTML = '';
  appendMessage('bot', '对话清空了，重新开始吧。');
});

// ---------- 启动 ----------

async function bootstrap(): Promise<void> {
  try {
    const saved = await invoke<ChatTurn[]>('load_chat_history');
    history = saved.filter((t) => t.role === 'user' || t.role === 'assistant');
    for (const turn of history.slice(-MAX_RENDERED)) {
      appendMessage(turn.role === 'user' ? 'user' : 'bot', turn.content);
    }
    if (history.length === 0) {
      appendMessage('bot', '我在呢。想聊点什么？');
    }
  } catch {
    appendMessage('bot', '我在呢。想聊点什么？');
  }

  inputEl.focus();
  autoGrow();
  await loadProviders();

  // 窗口只是 hide 不销毁；每次重新获得焦点时重读 providers.json，
  // 这样填完 key 不必重启 App。
  void win.onFocusChanged(({ payload: focused }) => {
    if (!focused) return;
    cachedProviders = null;
    void loadProviders();
  });
}

void bootstrap();
