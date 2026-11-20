import { appState, escapeHtml } from './main.js';
import { formatTokenCount } from './billing_view.js';
import { openTerminalDrawer } from './terminal_drawer.js';

const MAX_SIDE_LINES = 300;

const KIND_LABELS = {
  System: '系统',
  Thinking: '思考',
  ToolCall: '工具',
  Output: '输出',
  Stderr: '错误',
  Result: '结果',
};

const ROLE_FULL_LABELS = {
  Architect: '总架构师',
  Coder: '编码程序员',
  Reviewer: '代码审查员',
};

const ROLE_SHORT_LABELS = {
  Architect: '架构',
  Coder: '编码',
  Reviewer: '审查',
};

const STATE_META = {
  Working: ['工作中', 'working'],
  Idle: ['待命', 'idle'],
  QuotaCooling: ['配额冷却', 'warn'],
  Error: ['异常', 'error'],
  AttachedCli: ['已接管', 'warn'],
};

const ROLE_PRIORITY_ORDER = {
  Architect: 0,
  Coder: 1,
  Reviewer: 2,
};

let renderedSlotId = null;

function getEnabledSlots() {
  const all = Array.isArray(appState.slots) ? appState.slots : [];
  const enabled = all.filter((s) => s && s.enabled !== false);
  return enabled.length > 0 ? enabled : all;
}

function pickHighestPriorityWorkingSlot(workingSlots) {
  const sorted = [...workingSlots].sort((a, b) => {
    const pa = Number(a.priority) || 99;
    const pb = Number(b.priority) || 99;
    if (pa !== pb) return pa - pb;
    return (ROLE_PRIORITY_ORDER[a.role] ?? 9) - (ROLE_PRIORITY_ORDER[b.role] ?? 9);
  });
  return sorted[0]?.slot_id || null;
}

export function resolveEffectiveLiveSlotId() {
  const allSlots = Array.isArray(appState.slots) ? appState.slots : [];
  if (
    appState.livePanePinnedSlotId &&
    allSlots.some((s) => s.slot_id === appState.livePanePinnedSlotId)
  ) {
    return appState.livePanePinnedSlotId;
  }
  if (appState.livePanePinnedSlotId) {
    appState.livePanePinnedSlotId = null;
  }

  const enabledSlots = getEnabledSlots();
  const workingSlots = enabledSlots.filter((s) => s.state === 'Working');

  if (workingSlots.length === 1) {
    appState.livePaneAutoSlotId = workingSlots[0].slot_id;
    return appState.livePaneAutoSlotId;
  }

  if (workingSlots.length > 1) {
    if (
      appState.livePaneAutoSlotId &&
      workingSlots.some((s) => s.slot_id === appState.livePaneAutoSlotId)
    ) {
      return appState.livePaneAutoSlotId;
    }
    appState.livePaneAutoSlotId = pickHighestPriorityWorkingSlot(workingSlots);
    return appState.livePaneAutoSlotId;
  }

  if (
    appState.livePaneAutoSlotId &&
    allSlots.some((s) => s.slot_id === appState.livePaneAutoSlotId)
  ) {
    return appState.livePaneAutoSlotId;
  }
  return enabledSlots[0]?.slot_id || null;
}

function compactRoleTabLabel(slot) {
  const roleTag = ROLE_SHORT_LABELS[slot.role] || '角色';
  const rawName = String(slot.display_name || '');
  const parts = rawName.split('·');
  const tail = parts.length > 1 ? parts[parts.length - 1].trim() : slot.alias || '';
  return tail ? `${roleTag}·${tail}` : roleTag;
}

function stripGatewayTag(modelId) {
  return String(modelId || '')
    .replace(/^\[Gateway\]\s*/i, '')
    .trim();
}

function formatSideLineHtml(line) {
  const kind = line.kind || 'Output';
  const kindLabel = KIND_LABELS[kind] || kind;
  return `
    <div class="live-side-line ${escapeHtml(kind)}">
      <span class="live-line-time">${escapeHtml(line.timestamp || '')}</span>
      <span class="live-line-kind ${escapeHtml(kind)}">${escapeHtml(kindLabel)}</span>
      <span class="live-line-text">${escapeHtml(line.text || '')}</span>
    </div>
  `;
}

function renderStatusStripHtml(slot) {
  if (!slot) {
    return '<div class="live-strip-empty">暂无可用编队角色，请先在左侧添加或重置编队。</div>';
  }
  const roleLabel = ROLE_FULL_LABELS[slot.role] || slot.role || '角色';
  const cleanModel = stripGatewayTag(slot.model_id) || '未配置模型';
  const [stateText, stateCls] = STATE_META[slot.state] || ['待命', 'idle'];
  const tokens = formatTokenCount(slot?.usage?.total_tokens || 0);
  const activityRaw = String(slot.current_activity || '').trim();
  const activityText = activityRaw
    ? `${activityRaw} · 已用 ${tokens}`
    : slot.state === 'Working'
      ? `正在执行当前轮次 · 已用 ${tokens}`
      : `当前${stateText} · 已用 ${tokens}`;

  return `
    <div class="live-strip-top">
      <span class="live-strip-role">${escapeHtml(roleLabel)}</span>
      <span class="live-strip-model mono" title="${escapeHtml(slot.model_id || '')}">${escapeHtml(cleanModel)}</span>
      <span class="live-strip-state ${stateCls}">${escapeHtml(stateText)}</span>
    </div>
    <div class="live-strip-activity" title="${escapeHtml(activityText)}">${escapeHtml(activityText)}</div>
  `;
}

function renderHeaderAndTabs(effectiveSlotId, focusedSlot) {
  const dotEl = document.getElementById('live-side-dot');
  const aliasEl = document.getElementById('live-side-alias');
  const followBtn = document.getElementById('btn-live-follow-toggle');
  const tabsWrap = document.getElementById('live-role-tabs');
  const stripEl = document.getElementById('live-role-status-strip');

  const [, dotCls] = focusedSlot ? STATE_META[focusedSlot.state] || ['待命', 'idle'] : ['待命', 'idle'];
  if (dotEl) dotEl.className = `live-side-dot ${dotCls}`;
  if (aliasEl) {
    aliasEl.textContent = focusedSlot ? `@${focusedSlot.alias}` : '@待命';
    aliasEl.title = focusedSlot ? `${focusedSlot.display_name} (${focusedSlot.model_id})` : '';
  }

  const isPinned = Boolean(appState.livePanePinnedSlotId);
  if (followBtn) {
    followBtn.className = `live-follow-pill ${isPinned ? 'pinned' : 'auto'}`;
    followBtn.textContent = isPinned ? '📌 已锁定 · 恢复自动' : '● 自动跟随';
    followBtn.title = isPinned
      ? '已手动锁定查看该角色，点击立即恢复自动跟随正在干活的 AI'
      : '当前自动展示正在干活的角色 AI';
  }

  if (tabsWrap) {
    const enabledSlots = getEnabledSlots();
    tabsWrap.innerHTML = enabledSlots
      .map((slot) => {
        const isActive = slot.slot_id === effectiveSlotId;
        const isTabPinned = slot.slot_id === appState.livePanePinnedSlotId;
        const dot =
          slot.state === 'Working'
            ? '<span class="live-tab-dot working" aria-hidden="true"></span>'
            : slot.state === 'Error'
              ? '<span class="live-tab-dot error" aria-hidden="true"></span>'
              : slot.state === 'QuotaCooling'
                ? '<span class="live-tab-dot warn" aria-hidden="true"></span>'
                : '';
        const cls = [
          'live-role-tab',
          isActive ? 'active' : '',
          isTabPinned ? 'pinned' : '',
        ]
          .filter(Boolean)
          .join(' ');
        const tip = isTabPinned
          ? `${slot.display_name}（已锁定，再次点击恢复自动跟随）`
          : `${slot.display_name}（点击锁定查看此角色）`;
        return `
          <button type="button" role="tab" aria-selected="${isActive ? 'true' : 'false'}"
            class="${cls}" data-live-slot="${escapeHtml(slot.slot_id)}" title="${escapeHtml(tip)}">
            ${dot}<span>${escapeHtml(compactRoleTabLabel(slot))}</span>
          </button>
        `;
      })
      .join('');
  }

  if (stripEl) {
    stripEl.innerHTML = renderStatusStripHtml(focusedSlot);
  }
}

function renderViewportLines(effectiveSlotId, focusedSlot) {
  const viewport = document.getElementById('live-side-viewport');
  if (!viewport) return;

  const all = Array.isArray(appState.terminal_lines) ? appState.terminal_lines : [];
  const filtered = effectiveSlotId
    ? all.filter((line) => line.slot_id === effectiveSlotId).slice(-MAX_SIDE_LINES)
    : [];

  renderedSlotId = effectiveSlotId;
  if (filtered.length === 0) {
    const emptyMsg =
      focusedSlot && focusedSlot.state === 'Working'
        ? `已接单（@${focusedSlot.alias}），等待模型返回实时思考与工具调用流...`
        : focusedSlot
          ? `@${focusedSlot.alias} 当前暂无实时输出。启动目标后，该角色的思考、工具调用与输出将独立呈现于此。`
          : '启动长期目标后，当前正在干活的 AI 角色实时流将自动呈现于此。';
    viewport.innerHTML = `<div class="live-side-empty">${escapeHtml(emptyMsg)}</div>`;
    return;
  }

  viewport.innerHTML = filtered.map(formatSideLineHtml).join('');
  viewport.scrollTop = viewport.scrollHeight;
}

export function renderLiveSidePane(options = {}) {
  const effectiveSlotId = resolveEffectiveLiveSlotId();
  const focusedSlot =
    (appState.slots || []).find((s) => s.slot_id === effectiveSlotId) || null;

  renderHeaderAndTabs(effectiveSlotId, focusedSlot);
  if (options.forceLines || renderedSlotId !== effectiveSlotId) {
    renderViewportLines(effectiveSlotId, focusedSlot);
  }
}

export function updateLiveSidePaneStatus(slot) {
  if (!slot?.slot_id) return;
  const effectiveSlotId = resolveEffectiveLiveSlotId();
  if (slot.slot_id !== effectiveSlotId) return;
  const stripEl = document.getElementById('live-role-status-strip');
  if (stripEl) {
    stripEl.innerHTML = renderStatusStripHtml(slot);
  }
}

export function appendLiveSidePaneLine(line) {
  if (!line || !line.slot_id) return;
  const effectiveSlotId = resolveEffectiveLiveSlotId();
  if (renderedSlotId !== effectiveSlotId) {
    renderLiveSidePane({ forceLines: true });
    return;
  }
  if (line.slot_id !== effectiveSlotId) {
    return;
  }

  const viewport = document.getElementById('live-side-viewport');
  if (!viewport) return;

  const wasNearBottom =
    viewport.scrollHeight - viewport.scrollTop - viewport.clientHeight < 48;
  const emptyEl = viewport.querySelector('.live-side-empty');
  if (emptyEl) emptyEl.remove();

  viewport.insertAdjacentHTML('beforeend', formatSideLineHtml(line));
  while (viewport.children.length > MAX_SIDE_LINES) {
    viewport.firstElementChild?.remove();
  }
  if (wasNearBottom) {
    viewport.scrollTop = viewport.scrollHeight;
  }
}

export function clearLiveSidePaneUi() {
  renderLiveSidePane({ forceLines: true });
}

export function resetLiveSidePaneFollow() {
  appState.livePanePinnedSlotId = null;
  appState.livePaneAutoSlotId = null;
  renderedSlotId = null;
  renderLiveSidePane({ forceLines: true });
}

export function bindLiveSidePaneEvents() {
  document.getElementById('btn-live-follow-toggle')?.addEventListener('click', () => {
    if (appState.livePanePinnedSlotId !== null) {
      appState.livePanePinnedSlotId = null;
      renderLiveSidePane({ forceLines: true });
    }
  });

  document.getElementById('btn-live-expand-drawer')?.addEventListener('click', () => {
    openTerminalDrawer(resolveEffectiveLiveSlotId());
  });

  document.getElementById('live-role-tabs')?.addEventListener('click', (e) => {
    const btn = e.target.closest('[data-live-slot]');
    if (!btn) return;
    const slotId = btn.getAttribute('data-live-slot');
    if (!slotId) return;
    if (appState.livePanePinnedSlotId === slotId) {
      appState.livePanePinnedSlotId = null;
    } else {
      appState.livePanePinnedSlotId = slotId;
    }
    renderLiveSidePane({ forceLines: true });
  });
}
