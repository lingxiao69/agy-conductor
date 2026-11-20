import { appState, escapeHtml, postIpc, showToast } from './main.js';
import { openTerminalDrawer } from './terminal_drawer.js';
import {
  buildSlotBillingHtml,
  formatShortContext,
  renderTeamBillingFooter,
} from './billing_view.js';
import { computeEffectiveSlotSessionMap } from './session_resume_view.js';

const ROLE_ORDER = { Architect: 0, Coder: 1, Reviewer: 2 };
const ROLE_LABELS = {
  Architect: '总架构师',
  Coder: '编码程序员',
  Reviewer: '代码审查员',
};
const FAMILIES = ['All', 'Carbon', 'Neon', 'Hydrogen', 'Argon', 'Flash', 'Pro'];

let cliMenuSlotId = null;

function stateMeta(slot) {
  switch (slot.state) {
    case 'Working':
      return ['working', slot.current_activity || '执行任务中...'];
    case 'QuotaCooling':
      return ['cooling', '额度冷却 · 自动降级中'];
    case 'Paused':
      return ['warn', '已暂停'];
    case 'ExternalCliAttached':
      return ['cli', '外部 CLI 接管中'];
    case 'Error':
      return ['error', '连续异常'];
    default:
      return ['idle', '待命'];
  }
}

function shortModelLabel(modelId) {
  if (!modelId) return '未选择模型';
  return modelId.replace(/^\[Gateway\]\s*/i, '');
}

function saveRosterToBackend() {
  postIpc({
    cmd: 'save_roster',
    slots: appState.slots,
    dispatch_mode: appState.dispatch_mode,
  });
}

export function closeSlotCliMenu() {
  cliMenuSlotId = null;
  document.getElementById('slot-cli-menu')?.classList.add('hidden');
}

function openSlotCliMenu(slot, anchorEl) {
  const menu = document.getElementById('slot-cli-menu');
  const attachBtn = document.getElementById('menu-cli-attach');
  if (!menu || !attachBtn) return;

  cliMenuSlotId = slot.slot_id;
  const hasConv = Boolean(slot.conversation_id);
  attachBtn.disabled = !hasConv;
  attachBtn.textContent = hasConv
    ? `接管当前会话 CLI (${slot.conversation_id.slice(0, 8)}...)`
    : '接管当前会话 CLI（尚无会话记录）';

  const rect = anchorEl.getBoundingClientRect();
  menu.style.left = `${Math.min(window.innerWidth - 250, Math.max(12, rect.left - 140))}px`;
  menu.style.top = `${Math.min(window.innerHeight - 175, rect.bottom + 6)}px`;
  menu.classList.remove('hidden');
}

function getFilteredModels(currentModelId) {
  const fam = appState.modelFilterFamily;
  const q = appState.modelSearchText.trim().toLowerCase();
  const list = appState.models.filter((m) => {
    if (fam !== 'All' && m.family !== fam) return false;
    if (q && !m.id.toLowerCase().includes(q) && !m.display_name.toLowerCase().includes(q)) {
      return false;
    }
    return true;
  });
  if (currentModelId && !list.some((m) => m.id === currentModelId)) {
    return [
      { id: currentModelId, display_name: currentModelId, is_gateway: currentModelId.startsWith('[Gateway]') },
      ...list,
    ];
  }
  return list;
}

function buildInlineEditorHtml(slot) {
  const models = getFilteredModels(slot.model_id);
  const familyChips = FAMILIES.map((f) => {
    const active = appState.modelFilterFamily === f ? 'active' : '';
    const label = f === 'All' ? '全部' : f;
    return `<button type="button" class="chip-btn ${active}" data-fam="${f}">${label}</button>`;
  }).join('');

  const options = models
    .map((m) => {
      const sel = m.id === slot.model_id ? 'selected' : '';
      const label = m.is_gateway
        ? `[GW] ${String(m.display_name || '').replace(/^\[Gateway\]\s*/i, '')}`
        : `[官] ${m.display_name}`;
      return `<option value="${escapeHtml(m.id)}" ${sel}>${escapeHtml(label)}</option>`;
    })
    .join('');

  return `
    <div class="slot-editor" data-editor-slot="${escapeHtml(slot.slot_id)}">
      <div class="editor-row">
        <span>角色职责</span>
        <select class="select-clean edit-role">
          <option value="Architect" ${slot.role === 'Architect' ? 'selected' : ''}>总架构规划师</option>
          <option value="Coder" ${slot.role === 'Coder' ? 'selected' : ''}>编码程序员</option>
          <option value="Reviewer" ${slot.role === 'Reviewer' ? 'selected' : ''}>项目代码审查员</option>
        </select>
      </div>
      <div class="editor-row">
        <span>同角色优先级 (1 最高)</span>
        <div class="stepper">
          <button type="button" class="step-dec">−</button>
          <span class="step-val">P${slot.priority}</span>
          <button type="button" class="step-inc">+</button>
        </div>
      </div>
      <div class="editor-row">
        <span>展示名称</span>
        <input class="input-clean edit-name" style="flex:1;padding:4px 8px" value="${escapeHtml(slot.display_name)}" />
      </div>
      <div class="editor-row">
        <span>@群聊别名</span>
        <input class="input-clean mono edit-alias" style="flex:1;padding:4px 8px" value="${escapeHtml(slot.alias)}" />
      </div>
      <div class="family-chips">${familyChips}</div>
      <input class="input-clean edit-model-search" style="padding:4px 8px;font-size:12px" placeholder="搜索模型名，如 neon / carbon / hydrogen..." value="${escapeHtml(appState.modelSearchText)}" />
      <select class="select-clean edit-model">${options}</select>
      <div class="editor-row" style="margin-top:4px">
        <label class="inline-check">
          <input type="checkbox" class="edit-enabled" ${slot.enabled ? 'checked' : ''} />
          <span>启用该槽位</span>
        </label>
        <div style="display:flex;gap:6px">
          <button type="button" class="btn-danger-soft edit-delete" style="padding:3px 9px;font-size:12px">删除</button>
          <button type="button" class="btn-primary edit-done" style="padding:3px 10px;font-size:12px">完成</button>
        </div>
      </div>
    </div>
  `;
}

function buildSessionChipHtml(slot, effectiveMap) {
  if (slot.conversation_id) {
    const shortId = String(slot.conversation_id).slice(0, 6);
    const peak = Number(slot?.usage?.peak_context_tokens) || 0;
    const ctxSuffix = peak > 0 ? ` · ${formatShortContext(peak)}` : '';
    if (slot.session_reused) {
      return `<div class="slot-session-chip is-reused mono" title="已沿用历史记忆会话：${escapeHtml(slot.conversation_id)}">沿用记忆 · #${escapeHtml(shortId)}${escapeHtml(ctxSuffix)}</div>`;
    }
    return `<div class="slot-session-chip is-fresh mono" title="本轮新创建的活跃会话：${escapeHtml(slot.conversation_id)}">当前会话 · #${escapeHtml(shortId)}${escapeHtml(ctxSuffix)}</div>`;
  }

  const planned = effectiveMap[slot.slot_id];
  if (planned && planned.cid) {
    const shortId = String(planned.cid).slice(0, 6);
    const ctx = Number(planned.session?.context_tokens) || 0;
    const ctxSuffix = ctx > 0 ? ` (${formatShortContext(ctx)})` : '';
    const src = planned.session?.source === 'agy_brain' ? '本机 CLI 记忆' : '轻舟历史记忆';
    return `<div class="slot-session-chip is-reused mono" title="启动后将沿用 ${src}：${escapeHtml(planned.cid)}">沿用记忆 · #${escapeHtml(shortId)}${escapeHtml(ctxSuffix)}</div>`;
  }

  if (planned && planned.skippedUnhealthy) {
    const reason = planned.skippedUnhealthy.unhealthy_reason || '历史上下文体积过大，已自动跳过';
    return `<div class="slot-session-chip is-unhealthy" title="${escapeHtml(reason)}">旧会话已满 · 将开新会话</div>`;
  }

  return `<div class="slot-session-chip is-fresh" title="启动后将为该角色创建全新轻量上下文">全新会话</div>`;
}

export function renderRoster() {
  const container = document.getElementById('roster-list');
  const countEl = document.getElementById('roster-count');
  if (!container) return;

  const sorted = [...appState.slots].sort((a, b) => {
    const ra = ROLE_ORDER[a.role] ?? 9;
    const rb = ROLE_ORDER[b.role] ?? 9;
    if (ra !== rb) return ra - rb;
    return (a.priority || 1) - (b.priority || 1);
  });

  if (countEl) {
    const enabledCount = sorted.filter((s) => s.enabled).length;
    countEl.textContent = `${enabledCount}/${sorted.length}`;
  }

  const effectiveMap = computeEffectiveSlotSessionMap();

  container.innerHTML = sorted
    .map((slot) => {
      const [dotCls, statusText] = stateMeta(slot);
      const isEditing = appState.editingSlotId === slot.slot_id;
      const gwTag = slot.model_id.startsWith('[Gateway]') ? 'Gateway' : '官方';
      const sessionChip = buildSessionChipHtml(slot, effectiveMap);
      const billingHtml = buildSlotBillingHtml(slot);
      const releaseBtn =
        slot.state === 'ExternalCliAttached'
          ? `<button type="button" class="btn-jade-soft-sm btn-release-cli" data-slot="${escapeHtml(slot.slot_id)}" style="margin-top:4px;width:100%">结束接管，恢复自动协同</button>`
          : '';

      return `
        <div class="slot-card ${slot.enabled ? '' : 'disabled'} ${isEditing ? 'active-edit' : ''}" data-slot-id="${escapeHtml(slot.slot_id)}">
          <div class="slot-card-top" data-toggle-edit="${escapeHtml(slot.slot_id)}">
            <span class="role-pill ${slot.role}">${ROLE_LABELS[slot.role] || slot.role}</span>
            <span class="priority-badge">P${slot.priority}</span>
          </div>
          <div class="slot-model-line" data-toggle-edit="${escapeHtml(slot.slot_id)}">
            <span class="slot-name">${escapeHtml(slot.display_name)}</span>
            <span class="btn-chip-tag" style="font-size:10.5px">${gwTag}</span>
          </div>
          <div class="slot-model-sub mono" title="${escapeHtml(slot.model_id)}">${escapeHtml(shortModelLabel(slot.model_id))}</div>
          ${sessionChip}
          <div class="slot-footer">
            <div class="slot-status" title="${escapeHtml(statusText)}">
              <span class="status-dot ${dotCls}"></span>
              <span>${escapeHtml(statusText)}</span>
            </div>
            <button type="button" class="btn-cli-mini" data-cli-slot="${escapeHtml(slot.slot_id)}" title="拉起或接管该模型 CLI">&gt;_</button>
          </div>
          ${billingHtml}
          ${releaseBtn}
          ${isEditing ? buildInlineEditorHtml(slot) : ''}
        </div>
      `;
    })
    .join('');

  bindRosterItemEvents(container);
  renderTeamBillingFooter();
}

function bindRosterItemEvents(container) {
  container.querySelectorAll('[data-toggle-edit]').forEach((el) => {
    el.addEventListener('click', () => {
      const id = el.getAttribute('data-toggle-edit');
      appState.editingSlotId = appState.editingSlotId === id ? null : id;
      appState.modelFilterFamily = 'All';
      appState.modelSearchText = '';
      renderRoster();
    });
  });

  container.querySelectorAll('[data-cli-slot]').forEach((btn) => {
    btn.addEventListener('click', (e) => {
      e.stopPropagation();
      const id = btn.getAttribute('data-cli-slot');
      const slot = appState.slots.find((s) => s.slot_id === id);
      if (slot) openSlotCliMenu(slot, btn);
    });
  });

  container.querySelectorAll('.btn-release-cli').forEach((btn) => {
    btn.addEventListener('click', () => {
      postIpc({ cmd: 'release_terminal_attach', slot_id: btn.getAttribute('data-slot') });
    });
  });

  const editor = container.querySelector('.slot-editor');
  if (!editor) return;
  const slotId = editor.getAttribute('data-editor-slot');
  const slot = appState.slots.find((s) => s.slot_id === slotId);
  if (!slot) return;

  editor.querySelector('.edit-role')?.addEventListener('change', (e) => {
    slot.role = e.target.value;
    saveRosterToBackend();
  });
  editor.querySelector('.step-dec')?.addEventListener('click', () => {
    slot.priority = Math.max(1, (slot.priority || 1) - 1);
    saveRosterToBackend();
  });
  editor.querySelector('.step-inc')?.addEventListener('click', () => {
    slot.priority = Math.min(99, (slot.priority || 1) + 1);
    saveRosterToBackend();
  });
  editor.querySelector('.edit-name')?.addEventListener('change', (e) => {
    slot.display_name = e.target.value.trim() || slot.display_name;
    saveRosterToBackend();
  });
  editor.querySelector('.edit-alias')?.addEventListener('change', (e) => {
    const clean = e.target.value.trim().replace(/^@+/, '').replace(/\s+/g, '-');
    if (clean) slot.alias = clean;
    saveRosterToBackend();
  });
  editor.querySelector('.edit-model')?.addEventListener('change', (e) => {
    slot.model_id = e.target.value;
    saveRosterToBackend();
  });
  editor.querySelector('.edit-enabled')?.addEventListener('change', (e) => {
    slot.enabled = Boolean(e.target.checked);
    saveRosterToBackend();
  });
  editor.querySelectorAll('.chip-btn').forEach((chip) => {
    chip.addEventListener('click', () => {
      appState.modelFilterFamily = chip.getAttribute('data-fam') || 'All';
      renderRoster();
    });
  });
  editor.querySelector('.edit-model-search')?.addEventListener('input', (e) => {
    appState.modelSearchText = e.target.value;
    renderRoster();
    const nextInput = document.querySelector('.slot-editor .edit-model-search');
    if (nextInput) {
      nextInput.focus();
      nextInput.setSelectionRange(nextInput.value.length, nextInput.value.length);
    }
  });
  editor.querySelector('.edit-delete')?.addEventListener('click', () => {
    if (appState.slots.length <= 1) {
      showToast('warning', '编队中至少需要保留 1 个模型槽位');
      return;
    }
    appState.slots = appState.slots.filter((s) => s.slot_id !== slotId);
    appState.editingSlotId = null;
    saveRosterToBackend();
  });
  editor.querySelector('.edit-done')?.addEventListener('click', () => {
    appState.editingSlotId = null;
    saveRosterToBackend();
  });
}

document.addEventListener('DOMContentLoaded', () => {
  document.getElementById('select-dispatch-mode')?.addEventListener('change', (e) => {
    appState.dispatch_mode = e.target.value;
    saveRosterToBackend();
  });

  document.getElementById('btn-add-slot')?.addEventListener('click', () => {
    const idx = appState.slots.length + 1;
    const defaultModel =
      appState.models.find((m) => m.is_gateway === appState.prefer_gateway)?.id ||
      appState.models[0]?.id ||
      '[Gateway] gemini-4-neon-xhigh';
    const newSlot = {
      slot_id: `coder-slot-${Date.now().toString(36)}`,
      alias: `coder-${idx}`,
      display_name: `编码程序员 #${idx}`,
      model_id: defaultModel,
      role: 'Coder',
      priority: Math.min(99, appState.slots.filter((s) => s.role === 'Coder').length + 1),
      enabled: true,
      state: 'Idle',
      conversation_id: null,
      session_reused: false,
      current_activity: null,
      consecutive_failures: 0,
    };
    appState.slots.push(newSlot);
    appState.editingSlotId = newSlot.slot_id;
    saveRosterToBackend();
  });

  document.getElementById('btn-reset-roster')?.addEventListener('click', () => {
    appState.editingSlotId = null;
    postIpc({ cmd: 'apply_default_roster' });
  });

  document.getElementById('menu-cli-live-drawer')?.addEventListener('click', () => {
    if (!cliMenuSlotId) return;
    openTerminalDrawer(cliMenuSlotId);
    closeSlotCliMenu();
  });

  document.getElementById('menu-cli-live-ext')?.addEventListener('click', () => {
    if (!cliMenuSlotId) return;
    postIpc({
      cmd: 'open_terminal',
      slot_id: cliMenuSlotId,
      attach_conversation: false,
      split_all: false,
      mount_mcp: false,
      watch_live: true,
    });
    closeSlotCliMenu();
  });

  document.getElementById('menu-cli-attach')?.addEventListener('click', () => {
    if (!cliMenuSlotId) return;
    const mountMcp = Boolean(document.getElementById('chk-mount-mcp')?.checked);
    postIpc({
      cmd: 'open_terminal',
      slot_id: cliMenuSlotId,
      attach_conversation: true,
      split_all: false,
      mount_mcp: mountMcp,
      watch_live: false,
    });
    closeSlotCliMenu();
  });

  document.getElementById('menu-cli-fresh')?.addEventListener('click', () => {
    if (!cliMenuSlotId) return;
    const mountMcp = Boolean(document.getElementById('chk-mount-mcp')?.checked);
    postIpc({
      cmd: 'open_terminal',
      slot_id: cliMenuSlotId,
      attach_conversation: false,
      split_all: false,
      mount_mcp: mountMcp,
      watch_live: false,
    });
    closeSlotCliMenu();
  });
});
