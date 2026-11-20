import { appState, escapeHtml, postIpc, showToast } from './main.js';

let pendingDeletePresetId = null;

function ensurePresetsShell() {
  const host = document.getElementById('roster-presets-bar');
  if (!host) return null;
  if (!host.querySelector('#presets-chip-list')) {
    host.innerHTML = `
      <div class="presets-header">
        <span class="presets-title">编队预设</span>
        <span class="presets-active-tag" id="presets-active-tag">0 套</span>
      </div>
      <div class="presets-chip-list" id="presets-chip-list"></div>
      <div class="presets-save-row">
        <input
          id="input-preset-name"
          class="preset-name-input"
          type="text"
          maxlength="64"
          placeholder="保存为预设，如：日常极速组 / 架构精审组"
        />
        <button class="btn-jade-soft-sm" id="btn-save-preset" type="button">保存</button>
      </div>
    `;
  }
  return host;
}

function submitSavePreset() {
  const input = document.getElementById('input-preset-name');
  if (!input) return;
  const name = input.value.trim();
  if (!name) {
    showToast('warning', '请输入预设名称，例如“日常极速组”或“架构精审组”');
    input.focus();
    return;
  }
  if (!Array.isArray(appState.slots) || appState.slots.length === 0) {
    showToast('warning', '当前编队至少需要 1 个槽位才可保存预设');
    return;
  }

  pendingDeletePresetId = null;
  postIpc({
    cmd: 'save_roster_preset',
    name,
    slots: appState.slots,
    dispatch_mode: appState.dispatch_mode,
  });
  input.value = '';
}

function buildPresetChipHtml(preset) {
  const isActive = preset.preset_id === appState.active_preset_id;
  const isConfirming = preset.preset_id === pendingDeletePresetId;
  const slotCount = Array.isArray(preset.slots) ? preset.slots.length : 0;
  const modeText =
    preset.dispatch_mode === 'StrictPrimaryFailover'
      ? '严格主备容灾'
      : '优先级优先 + 并行溢出';
  const tip = `${preset.name}（${slotCount} 槽 · ${modeText}）`;

  if (isConfirming) {
    return `
      <div class="preset-chip confirming" data-preset-id="${escapeHtml(preset.preset_id)}">
        <span class="preset-confirm-text" title="${escapeHtml(preset.name)}">删除「${escapeHtml(preset.name)}」？</span>
        <div class="preset-confirm-actions">
          <button type="button" class="btn-danger-xs preset-confirm-yes" data-preset-id="${escapeHtml(preset.preset_id)}">确认删除</button>
          <button type="button" class="btn-ghost-xs preset-confirm-no">取消</button>
        </div>
      </div>
    `;
  }

  return `
    <div class="preset-chip ${isActive ? 'active' : ''}" data-preset-id="${escapeHtml(preset.preset_id)}" title="${escapeHtml(tip)}">
      <button type="button" class="preset-chip-btn" data-apply-preset="${escapeHtml(preset.preset_id)}">
        <span class="preset-chip-name">${escapeHtml(preset.name)}</span>
        <span class="preset-chip-count">${slotCount}</span>
      </button>
      <button type="button" class="preset-chip-del" data-del-preset="${escapeHtml(preset.preset_id)}" title="删除预设「${escapeHtml(preset.name)}」" aria-label="删除预设">✕</button>
    </div>
  `;
}

export function renderPresetsBar() {
  const host = ensurePresetsShell();
  if (!host) return;

  const presets = Array.isArray(appState.presets) ? appState.presets : [];
  if (
    pendingDeletePresetId &&
    !presets.some((p) => p.preset_id === pendingDeletePresetId)
  ) {
    pendingDeletePresetId = null;
  }

  const tagEl = document.getElementById('presets-active-tag');
  if (tagEl) {
    const active = presets.find((p) => p.preset_id === appState.active_preset_id);
    if (active) {
      tagEl.textContent = `当前：${active.name}`;
      tagEl.classList.add('is-active');
      tagEl.title = `当前已应用预设：${active.name}`;
    } else if (presets.length > 0) {
      tagEl.textContent = `自定义 · ${presets.length} 套`;
      tagEl.classList.remove('is-active');
      tagEl.title = '当前为未绑定预设的自定义编队';
    } else {
      tagEl.textContent = '0 套';
      tagEl.classList.remove('is-active');
      tagEl.title = '';
    }
  }

  const listEl = document.getElementById('presets-chip-list');
  if (!listEl) return;

  if (presets.length === 0) {
    listEl.innerHTML =
      '<div class="presets-empty-hint">暂无已保存预设，可在下方命名保存当前角色与模型组合。</div>';
    return;
  }

  listEl.innerHTML = presets.map(buildPresetChipHtml).join('');

  listEl.querySelectorAll('[data-apply-preset]').forEach((btn) => {
    btn.addEventListener('click', () => {
      const presetId = btn.getAttribute('data-apply-preset');
      if (!presetId) return;
      pendingDeletePresetId = null;
      appState.editingSlotId = null;
      postIpc({ cmd: 'apply_roster_preset', preset_id: presetId });
    });
  });

  listEl.querySelectorAll('[data-del-preset]').forEach((btn) => {
    btn.addEventListener('click', (e) => {
      e.stopPropagation();
      pendingDeletePresetId = btn.getAttribute('data-del-preset');
      renderPresetsBar();
    });
  });

  listEl.querySelectorAll('.preset-confirm-yes').forEach((btn) => {
    btn.addEventListener('click', () => {
      const presetId = btn.getAttribute('data-preset-id');
      pendingDeletePresetId = null;
      renderPresetsBar();
      if (presetId) {
        postIpc({ cmd: 'delete_roster_preset', preset_id: presetId });
      }
    });
  });

  listEl.querySelectorAll('.preset-confirm-no').forEach((btn) => {
    btn.addEventListener('click', () => {
      pendingDeletePresetId = null;
      renderPresetsBar();
    });
  });
}

export function bindPresetsEvents() {
  const host = ensurePresetsShell();
  if (!host) return;

  document.getElementById('btn-save-preset')?.addEventListener('click', submitSavePreset);
  document.getElementById('input-preset-name')?.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') {
      e.preventDefault();
      submitSavePreset();
    }
  });
}
