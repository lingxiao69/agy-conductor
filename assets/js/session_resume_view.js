import { appState, escapeHtml, postIpc } from './main.js';
import { formatShortContext } from './billing_view.js';
import { renderRoster } from './roster_view.js';

const ROLE_LABELS = {
  Architect: '总架构师',
  Coder: '编码程序员',
  Reviewer: '代码审查员',
};

function shortConvId(cid) {
  const clean = String(cid || '').trim();
  return clean.length > 10 ? `${clean.slice(0, 8)}...` : clean;
}

function formatShortTime(ts) {
  if (!ts) return '';
  const d = new Date(ts);
  if (Number.isNaN(d.getTime())) return '';
  const mm = String(d.getMonth() + 1).padStart(2, '0');
  const dd = String(d.getDate()).padStart(2, '0');
  const hh = String(d.getHours()).padStart(2, '0');
  const mi = String(d.getMinutes()).padStart(2, '0');
  return `${mm}-${dd} ${hh}:${mi}`;
}

function truncateGoal(text, maxLen = 22) {
  const clean = String(text || '').replace(/\s+/g, ' ').trim();
  if (!clean) return '';
  return clean.length > maxLen ? `${clean.slice(0, maxLen)}…` : clean;
}

function sourceBadgeMeta(source, healthy) {
  if (healthy === false) {
    return ['unhealthy', '体积过大/异常'];
  }
  if (source === 'agy_brain') {
    return ['agy_brain', '本机 CLI 历史'];
  }
  return ['conductor', '轻舟记录'];
}

function isHealthySession(item) {
  return Boolean(item) && item.healthy_for_reuse !== false;
}

function computeAutoMatchForSlots(slots, sessions) {
  const matched = {};
  const used = new Set();
  const healthy = sessions.filter(isHealthySession);
  const enabledSlots = [...slots].sort((a, b) => {
    if (Boolean(b.enabled) !== Boolean(a.enabled)) return b.enabled ? 1 : -1;
    return (a.priority || 1) - (b.priority || 1);
  });

  // Pass 1: slot_id + role (仅自动沿用健康会话)
  for (const s of enabledSlots) {
    const found = healthy.find(
      (item) =>
        !used.has(item.conversation_id) &&
        item.role === s.role &&
        item.slot_id &&
        item.slot_id.toLowerCase() === String(s.slot_id || '').toLowerCase()
    );
    if (found) {
      used.add(found.conversation_id);
      matched[s.slot_id] = found.conversation_id;
    }
  }

  // Pass 2: alias + role
  for (const s of enabledSlots) {
    if (matched[s.slot_id]) continue;
    const alias = String(s.alias || '').replace(/^@+/, '').toLowerCase();
    const found = healthy.find(
      (item) =>
        !used.has(item.conversation_id) &&
        item.role === s.role &&
        item.alias &&
        item.alias.replace(/^@+/, '').toLowerCase() === alias
    );
    if (found) {
      used.add(found.conversation_id);
      matched[s.slot_id] = found.conversation_id;
    }
  }

  // Pass 3: same role recency fallback
  for (const s of enabledSlots) {
    if (matched[s.slot_id]) continue;
    const found = healthy.find(
      (item) => !used.has(item.conversation_id) && item.role === s.role
    );
    if (found) {
      used.add(found.conversation_id);
      matched[s.slot_id] = found.conversation_id;
    }
  }

  return matched;
}

/**
 * 计算每个槽位当前生效的历史沿用方案（供左侧角色卡片微章与启动页共用）。
 */
export function computeEffectiveSlotSessionMap() {
  const result = {};
  const sessions = Array.isArray(appState.workspace_sessions) ? appState.workspace_sessions : [];
  const reuse = appState.reuse_role_sessions !== false;
  if (!reuse || sessions.length === 0) return result;

  const enabledSlots = appState.slots.filter((s) => s.enabled);
  const autoMatches = computeAutoMatchForSlots(enabledSlots, sessions);

  for (const slot of enabledSlots) {
    const overrideVal = appState.selectedSessions?.[slot.slot_id];
    const currentVal =
      overrideVal !== undefined ? overrideVal : autoMatches[slot.slot_id] || '__new__';
    if (currentVal && currentVal !== '__new__') {
      const sess = sessions.find((item) => item.conversation_id === currentVal) || null;
      result[slot.slot_id] = { cid: currentVal, session: sess, skippedUnhealthy: null };
    } else if (overrideVal === undefined) {
      const unhealthy = sessions.find(
        (item) =>
          item.healthy_for_reuse === false &&
          (item.slot_id === slot.slot_id || item.role === slot.role)
      );
      if (unhealthy) {
        result[slot.slot_id] = { cid: null, session: null, skippedUnhealthy: unhealthy };
      }
    }
  }
  return result;
}

export function resetSessionSelections() {
  appState.selectedSessions = {};
}

export function getSelectedSessionsForStart() {
  if (!appState.reuse_role_sessions) return {};
  const out = {};
  const map = appState.selectedSessions || {};
  for (const [slotId, cid] of Object.entries(map)) {
    if (typeof cid === 'string' && cid.trim().length > 0) {
      out[slotId] = cid.trim();
    }
  }
  return out;
}

export function renderSessionResumePanel() {
  const panel = document.getElementById('session-resume-panel');
  if (!panel) return;

  const sessions = Array.isArray(appState.workspace_sessions) ? appState.workspace_sessions : [];
  if (!appState.workspace || sessions.length === 0) {
    panel.classList.add('hidden');
    panel.innerHTML = '';
    return;
  }

  const enabledSlots = appState.slots.filter((s) => s.enabled);
  const autoMatches = computeAutoMatchForSlots(enabledSlots, sessions);
  const reuse = appState.reuse_role_sessions !== false;
  const healthyCount = sessions.filter(isHealthySession).length;

  const rowsHtml = enabledSlots
    .map((slot) => {
      const roleSessions = sessions.filter((item) => item.role === slot.role);
      const allOptions = roleSessions.length > 0 ? roleSessions : sessions;
      const overrideVal = appState.selectedSessions?.[slot.slot_id];
      const currentVal =
        overrideVal !== undefined ? overrideVal : autoMatches[slot.slot_id] || '__new__';

      const chosenObj =
        currentVal !== '__new__'
          ? sessions.find((item) => item.conversation_id === currentVal)
          : null;
      const [badgeCls, badgeLabel] = chosenObj
        ? sourceBadgeMeta(chosenObj.source, chosenObj.healthy_for_reuse)
        : ['fresh', '全新会话'];

      const optionItems = [
        `<option value="__new__" ${currentVal === '__new__' ? 'selected' : ''}>新开全新会话（推荐轻量）</option>`,
        ...allOptions.map((item) => {
          const ts = formatShortTime(item.updated_at);
          const ctxTag = item.context_tokens > 0 ? `${formatShortContext(item.context_tokens)}` : '';
          const healthTag =
            item.healthy_for_reuse === false
              ? `[${item.unhealthy_reason || '上下文过载，已自动跳过'}]`
              : '';
          const goalHint = truncateGoal(item.last_goal);
          const parts = [
            `@${item.alias || item.slot_id}`,
            shortConvId(item.conversation_id),
            ctxTag,
            ts,
            healthTag || goalHint,
          ].filter(Boolean);
          const sel = currentVal === item.conversation_id ? 'selected' : '';
          return `<option value="${escapeHtml(item.conversation_id)}" ${sel}>${escapeHtml(parts.join(' · '))}</option>`;
        }),
      ].join('');

      return `
        <div class="session-slot-row">
          <div class="session-slot-identity">
            <span class="role-pill ${escapeHtml(slot.role)}">${ROLE_LABELS[slot.role] || escapeHtml(slot.role)}</span>
            <span class="session-slot-name" title="${escapeHtml(slot.display_name)}">${escapeHtml(slot.display_name)}</span>
          </div>
          <select class="session-select" data-session-slot="${escapeHtml(slot.slot_id)}" ${reuse ? '' : 'disabled'}>
            ${optionItems}
          </select>
          <span class="session-source-badge ${badgeCls}">${badgeLabel}</span>
        </div>
      `;
    })
    .join('');

  panel.classList.remove('hidden');
  panel.innerHTML = `
    <div class="session-resume-header">
      <label class="session-resume-toggle">
        <input type="checkbox" id="chk-reuse-role-sessions" ${reuse ? 'checked' : ''} />
        <span>沿用最近一次各身份角色 AI 会话执行（免重新熟悉项目）</span>
      </label>
      <span class="session-resume-summary">已识别 ${sessions.length} 条（健康可沿用 ${healthyCount} 条）</span>
    </div>
    <p class="session-resume-hint">
      系统会自动跳过上下文超过 120k 或末尾异常的历史会话以防卡死；你也可以按角色手动切换。
    </p>
    <div class="session-slot-list ${reuse ? '' : 'muted'}">
      ${rowsHtml}
    </div>
  `;

  panel.querySelector('#chk-reuse-role-sessions')?.addEventListener('change', (e) => {
    const nextReuse = Boolean(e.target.checked);
    appState.reuse_role_sessions = nextReuse;
    postIpc({ cmd: 'set_reuse_role_sessions', reuse: nextReuse });
    renderSessionResumePanel();
    renderRoster();
  });

  panel.querySelectorAll('[data-session-slot]').forEach((sel) => {
    sel.addEventListener('change', (e) => {
      const slotId = sel.getAttribute('data-session-slot');
      if (!slotId) return;
      appState.selectedSessions = {
        ...(appState.selectedSessions || {}),
        [slotId]: e.target.value,
      };
      renderSessionResumePanel();
      renderRoster();
    });
  });
}

export function bindSessionResumeEvents() {
  // Delegated listeners are attached directly in renderSessionResumePanel.
}
