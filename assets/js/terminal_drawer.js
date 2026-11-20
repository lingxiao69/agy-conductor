import { appState, escapeHtml, postIpc } from './main.js';

const KIND_LABELS = {
  System: '系统',
  Thinking: '思考',
  ToolCall: '工具',
  Output: '输出',
  Stderr: '错误',
  Result: '结果',
};

const MAX_CLIENT_LINES = 600;

function formatTermLineHtml(line) {
  const kind = line.kind || 'Output';
  const kindLabel = KIND_LABELS[kind] || kind;
  const alias = line.alias || line.slot_id || 'system';
  return `
    <div class="term-line ${escapeHtml(kind)}">
      <span class="term-time">${escapeHtml(line.timestamp || '')}</span>
      <span class="term-alias">@${escapeHtml(alias)}</span>
      <span class="term-kind ${escapeHtml(kind)}">${escapeHtml(kindLabel)}</span>
      <span class="term-text">${escapeHtml(line.text || '')}</span>
    </div>
  `;
}

function getFilteredLines() {
  const filter = appState.terminalFilterSlotId || 'all';
  const all = Array.isArray(appState.terminal_lines) ? appState.terminal_lines : [];
  if (filter === 'all') return all;
  return all.filter((line) => line.slot_id === filter);
}

function renderFilterChips() {
  const wrap = document.getElementById('terminal-filter-chips');
  if (!wrap) return;

  const filter = appState.terminalFilterSlotId || 'all';
  const chips = [
    `<button type="button" class="term-chip ${filter === 'all' ? 'active' : ''}" data-term-slot="all">全部角色</button>`,
  ];

  for (const slot of appState.slots) {
    const active = filter === slot.slot_id ? 'active' : '';
    const dot =
      slot.state === 'Working'
        ? '<span class="status-dot working" style="width:6px;height:6px"></span>'
        : '';
    chips.push(
      `<button type="button" class="term-chip ${active}" data-term-slot="${escapeHtml(slot.slot_id)}">${dot}@${escapeHtml(slot.alias)}</button>`
    );
  }

  wrap.innerHTML = chips.join('');
  wrap.querySelectorAll('[data-term-slot]').forEach((btn) => {
    btn.addEventListener('click', () => {
      appState.terminalFilterSlotId = btn.getAttribute('data-term-slot') || 'all';
      renderTerminalDrawer();
    });
  });
}

function updateFooterCount(shownCount) {
  const total = Array.isArray(appState.terminal_lines) ? appState.terminal_lines.length : 0;
  const countEl = document.getElementById('terminal-line-count');
  if (countEl) {
    countEl.textContent = `当前显示 ${shownCount} 行 / 缓冲 ${total} 行（上限 ${MAX_CLIENT_LINES} 行）`;
  }
}

export function renderTerminalDrawer() {
  const drawer = document.getElementById('terminal-drawer');
  const viewport = document.getElementById('terminal-viewport');
  if (!drawer || !viewport) return;

  drawer.classList.toggle('hidden', !appState.terminalDrawerOpen);
  drawer.setAttribute('aria-hidden', appState.terminalDrawerOpen ? 'false' : 'true');
  if (!appState.terminalDrawerOpen) return;

  renderFilterChips();

  const lines = getFilteredLines();
  if (lines.length === 0) {
    viewport.innerHTML =
      '<div class="terminal-empty">尚无实时进程输出。启动长期目标后，各 AI 角色的思考摘要、工具调用路径与命令输出将在此实时滚动呈现。</div>';
  } else {
    viewport.innerHTML = lines.map(formatTermLineHtml).join('');
    viewport.scrollTop = viewport.scrollHeight;
  }
  updateFooterCount(lines.length);
}

export function openTerminalDrawer(slotId = null) {
  appState.terminalFilterSlotId = slotId || 'all';
  appState.terminalDrawerOpen = true;
  renderTerminalDrawer();
}

export function closeTerminalDrawer() {
  appState.terminalDrawerOpen = false;
  const drawer = document.getElementById('terminal-drawer');
  if (drawer) {
    drawer.classList.add('hidden');
    drawer.setAttribute('aria-hidden', 'true');
  }
}

export function appendLiveTerminalLine(line) {
  if (!Array.isArray(appState.terminal_lines)) {
    appState.terminal_lines = [];
  }
  appState.terminal_lines.push(line);
  if (appState.terminal_lines.length > MAX_CLIENT_LINES) {
    appState.terminal_lines.splice(0, appState.terminal_lines.length - MAX_CLIENT_LINES);
  }

  if (!appState.terminalDrawerOpen) return;
  const filter = appState.terminalFilterSlotId || 'all';
  if (filter !== 'all' && line.slot_id !== filter) {
    updateFooterCount(getFilteredLines().length);
    return;
  }

  const viewport = document.getElementById('terminal-viewport');
  if (!viewport) return;
  const wasNearBottom =
    viewport.scrollHeight - viewport.scrollTop - viewport.clientHeight < 48;

  const emptyEl = viewport.querySelector('.terminal-empty');
  if (emptyEl) emptyEl.remove();

  viewport.insertAdjacentHTML('beforeend', formatTermLineHtml(line));
  while (viewport.children.length > MAX_CLIENT_LINES) {
    viewport.firstElementChild?.remove();
  }
  if (wasNearBottom) {
    viewport.scrollTop = viewport.scrollHeight;
  }
  updateFooterCount(viewport.children.length);
}

export function clearTerminalLinesUi() {
  appState.terminal_lines = [];
  if (appState.terminalDrawerOpen) {
    renderTerminalDrawer();
  }
}

export function bindTerminalDrawerEvents() {
  const drawer = document.getElementById('terminal-drawer');
  document.getElementById('btn-close-terminal-drawer')?.addEventListener('click', closeTerminalDrawer);
  drawer?.addEventListener('click', (e) => {
    if (e.target === drawer) closeTerminalDrawer();
  });

  document.getElementById('btn-term-clear')?.addEventListener('click', () => {
    postIpc({ cmd: 'clear_terminal_log' });
  });

  document.getElementById('btn-term-ext-live-split')?.addEventListener('click', () => {
    const activeFilter = appState.terminalFilterSlotId || 'all';
    const watchSingle = activeFilter !== 'all';
    postIpc({
      cmd: 'open_terminal',
      slot_id: watchSingle ? activeFilter : null,
      attach_conversation: false,
      split_all: !watchSingle,
      mount_mcp: false,
      watch_live: true,
    });
  });

  document.getElementById('btn-term-ext-interactive')?.addEventListener('click', () => {
    closeTerminalDrawer();
    document.getElementById('cli-modal')?.classList.remove('hidden');
  });

  document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape' && appState.terminalDrawerOpen) {
      closeTerminalDrawer();
    }
  });
}
