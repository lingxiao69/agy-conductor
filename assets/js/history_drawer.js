import { appState, escapeHtml, postIpc } from './main.js';
import { renderMessageCard } from './chat_view.js';

const HISTORY_PHASE_META = {
  Idle: ['待命', 'idle'],
  Planning: ['架构规划中', 'working'],
  Running: ['编码与审查中', 'working'],
  FinalGate: ['全局终验中', 'working'],
  Paused: ['已暂停', 'warn'],
  WaitingUser: ['等待人工拍板', 'warn'],
  Completed: ['验收已完成', 'ok'],
  Stopped: ['已停止', 'idle'],
  Failed: ['运行异常', 'error'],
  '待命': ['待命', 'idle'],
  '架构规划中': ['架构规划中', 'working'],
  '编码与审查中': ['编码与审查中', 'working'],
  '全局终验中': ['全局终验中', 'working'],
  '已暂停': ['已暂停', 'warn'],
  '等待人工拍板': ['等待人工拍板', 'warn'],
  '验收已完成': ['验收已完成', 'ok'],
  '全部验收通过': ['全部验收通过', 'ok'],
  '已停止': ['已停止', 'idle'],
  '运行异常': ['运行异常', 'error'],
  '仅对话记录': ['仅对话记录', 'idle'],
};

const HISTORY_TASK_STATUS_META = {
  Pending: ['○', '待派单'],
  WaitingPathLock: ['⏳', '等待路径锁释放'],
  Coding: ['●', '编码中'],
  PendingReview: ['⏳', '等待代码审查'],
  InReview: ['●', '独立审查中'],
  ReworkNeeded: ['↻', '打回修改中'],
  Approved: ['✓', '审查通过'],
  BlockedByArch: ['⚠', '架构阻塞'],
  NeedsUserDecision: ['⚠', '需人工拍板'],
};

let pendingDeleteRoomId = null;
let shouldRefreshActiveOnList = false;

function phaseBadgeInfo(phase) {
  return HISTORY_PHASE_META[phase] || [phase || '未知状态', 'idle'];
}

function hasSummaryChanged(summary, detail) {
  if (!summary || !detail || summary.room_id !== detail.room_id) return true;
  const tasks = Array.isArray(detail.tasks) ? detail.tasks : [];
  const messages = Array.isArray(detail.messages) ? detail.messages : [];
  const approvedCount = tasks.filter((t) => t.status === 'Approved').length;
  return (
    summary.updated_at !== detail.updated_at ||
    summary.phase !== detail.phase ||
    (summary.message_count ?? 0) !== messages.length ||
    (summary.task_count ?? 0) !== tasks.length ||
    (summary.approved_task_count ?? 0) !== approvedCount
  );
}

function formatUpdatedAt(raw) {
  if (!raw) return '-';
  const parsed = new Date(raw);
  if (Number.isNaN(parsed.getTime())) return String(raw);
  const pad = (n) => String(n).padStart(2, '0');
  return `${parsed.getFullYear()}-${pad(parsed.getMonth() + 1)}-${pad(parsed.getDate())} ${pad(parsed.getHours())}:${pad(parsed.getMinutes())}`;
}

function getFilteredRooms() {
  const rooms = Array.isArray(appState.historyRooms) ? appState.historyRooms : [];
  const q = (appState.historySearchText || '').trim().toLowerCase();
  if (!q) return rooms;
  return rooms.filter((r) => {
    const [phaseZh] = phaseBadgeInfo(r.phase);
    return [r.goal, r.workspace, r.phase, phaseZh, r.room_id]
      .some((v) => String(v || '').toLowerCase().includes(q));
  });
}

function selectHistoryRoom(roomId, { silentRefresh = false } = {}) {
  if (!roomId) return;
  pendingDeleteRoomId = null;
  const isSwitchingRoom = appState.activeHistoryRoomId !== roomId;
  if (isSwitchingRoom && appState.activeHistoryDetail?.room_id !== roomId) {
    appState.activeHistoryDetail = null;
  }
  appState.activeHistoryRoomId = roomId;
  if (!silentRefresh || !appState.activeHistoryDetail || appState.activeHistoryDetail.room_id !== roomId) {
    appState.historyLoadingDetail = true;
  }
  renderHistoryDrawer();
  postIpc({ cmd: 'load_history_room', room_id: roomId });
}

function renderHistoryList() {
  const listEl = document.getElementById('history-room-list');
  const badgeEl = document.getElementById('history-count-badge');
  if (!listEl) return;

  const allRooms = Array.isArray(appState.historyRooms) ? appState.historyRooms : [];
  const filtered = getFilteredRooms();
  if (badgeEl) {
    badgeEl.textContent = appState.historySearchText.trim()
      ? `${filtered.length}/${allRooms.length}`
      : `${allRooms.length}`;
  }

  if (appState.historyLoadingList && allRooms.length === 0) {
    listEl.innerHTML = '<div class="empty-hint">正在读取本地历史运行记录...</div>';
    return;
  }
  if (allRooms.length === 0) {
    listEl.innerHTML =
      '<div class="empty-hint">本地暂无历史项目记录。<br/>运行一次长期目标后将自动归档于此。</div>';
    return;
  }
  if (filtered.length === 0) {
    listEl.innerHTML = `
      <div class="empty-hint">
        <div>未找到匹配「${escapeHtml(appState.historySearchText.trim())}」的历史记录</div>
        <button type="button" class="btn-link" id="btn-clear-history-filter" style="margin-top:6px">清除筛选条件</button>
      </div>`;
    document.getElementById('btn-clear-history-filter')?.addEventListener('click', () => {
      appState.historySearchText = '';
      const input = document.getElementById('history-search-input');
      if (input) input.value = '';
      renderHistoryList();
    });
    return;
  }

  listEl.innerHTML = filtered
    .map((room) => {
      const [phaseLabel, phaseCls] = phaseBadgeInfo(room.phase);
      const isActive = room.room_id === appState.activeHistoryRoomId;
      const goalText = room.goal?.trim() || '（未记录研发目标）';
      const wsText = room.workspace?.trim() || '未指定目录';
      return `
        <button type="button" class="history-room-card ${isActive ? 'active' : ''}" data-room-id="${escapeHtml(room.room_id)}">
          <div class="history-room-top">
            <span class="history-phase-tag ${phaseCls}">${escapeHtml(phaseLabel)}</span>
            <span class="history-room-time">${escapeHtml(formatUpdatedAt(room.updated_at))}</span>
          </div>
          <div class="history-room-goal" title="${escapeHtml(goalText)}">${escapeHtml(goalText)}</div>
          <div class="history-room-ws mono" title="${escapeHtml(wsText)}">${escapeHtml(wsText)}</div>
          <div class="history-room-stats">
            <span>子任务 ${room.approved_task_count ?? 0}/${room.task_count ?? 0}</span>
            <span>对话 ${room.message_count ?? 0} 条</span>
          </div>
        </button>`;
    })
    .join('');

  listEl.querySelectorAll('.history-room-card').forEach((card) => {
    card.addEventListener('click', () => {
      const roomId = card.getAttribute('data-room-id');
      if (roomId) selectHistoryRoom(roomId);
    });
  });
}

function buildHistoryTaskCardHtml(t) {
  const [icon, statusText] = HISTORY_TASK_STATUS_META[t.status] || ['○', t.status || '未知'];
  const coder = t.assigned_coder_label ? `${t.assigned_coder_label} · ` : '';
  const rework = t.rework_count > 0 ? ` · 第 ${t.rework_count} 轮返工` : '';
  const paths =
    Array.isArray(t.owned_paths) && t.owned_paths.length > 0 ? t.owned_paths.join(', ') : '全仓';

  const extra = [];
  if (t.last_coder_summary) {
    extra.push(`<div><strong>编码摘要：</strong>${escapeHtml(t.last_coder_summary)}</div>`);
  }
  if (t.last_review_note) {
    extra.push(`<div><strong>审查意见：</strong>${escapeHtml(t.last_review_note)}</div>`);
  }
  if (t.blocker_reason) {
    extra.push(`<div><strong>阻塞原因：</strong>${escapeHtml(t.blocker_reason)}</div>`);
  }
  if (Array.isArray(t.last_review_issues) && t.last_review_issues.length > 0) {
    extra.push(`<div class="msg-issues">${t.last_review_issues
      .map((iss) => {
        const loc = iss.line_hint ? `${iss.file_path}:${iss.line_hint}` : iss.file_path || '';
        return `<div class="issue-item"><span class="issue-sev">[${escapeHtml(iss.severity)}]</span><span class="mono">${escapeHtml(loc)}</span><span>${escapeHtml(iss.detail)}</span></div>`;
      })
      .join('')}</div>`);
  }
  if (Array.isArray(t.last_modified_files) && t.last_modified_files.length > 0) {
    extra.push(`<div><strong>涉及文件（${t.last_modified_files.length}）：</strong><ul class="msg-file-list">${t.last_modified_files
      .map((f) => `<li>${escapeHtml(f)}</li>`)
      .join('')}</ul></div>`);
  }

  const extraHtml =
    extra.length > 0
      ? `<details class="history-task-details"><summary>展开交付与审查明细</summary><div class="history-task-extra">${extra.join('')}</div></details>`
      : '';

  return `
    <div class="task-card history-task-card">
      <div class="task-title-row">
        <span class="task-icon ${escapeHtml(t.status)}">${icon}</span>
        <span>${escapeHtml(t.task_id)} ${escapeHtml(t.title)}</span>
      </div>
      <div class="task-meta">${escapeHtml(coder + statusText + rework)}</div>
      <div class="task-paths" title="${escapeHtml(paths)}">${escapeHtml(paths)}</div>
      ${t.description ? `<div class="history-task-desc">${escapeHtml(t.description)}</div>` : ''}
      ${extraHtml}
    </div>`;
}

function renderHistoryDetailPane() {
  const pane = document.getElementById('history-detail-pane');
  if (!pane) return;

  const allRooms = Array.isArray(appState.historyRooms) ? appState.historyRooms : [];
  if (appState.historyLoadingDetail) {
    pane.innerHTML = `<div class="history-empty-state"><div class="history-empty-title">正在加载项目历史详情...</div><div>正在解析运行快照与群聊记录</div></div>`;
    return;
  }

  const detail = appState.activeHistoryDetail;
  if (!detail || !appState.activeHistoryRoomId) {
    pane.innerHTML =
      allRooms.length === 0
        ? `<div class="history-empty-state"><div class="history-empty-title">尚无历史项目记录</div><div>启动长期研发目标后，运行快照与群聊记录将自动保存到本地 rooms 目录，方便回看与导出报告。</div></div>`
        : `<div class="history-empty-state"><div class="history-empty-title">请选择一条历史项目记录</div><div>在左侧点击任意运行记录，即可回放群聊对话、检查子任务验收状态或导出 Markdown 报告。</div></div>`;
    return;
  }

  const [phaseLabel, phaseCls] = phaseBadgeInfo(detail.phase);
  const tasks = Array.isArray(detail.tasks) ? detail.tasks : [];
  const messages = Array.isArray(detail.messages) ? detail.messages : [];
  const approvedCount = tasks.filter((t) => t.status === 'Approved').length;
  const activeTab = appState.historyActiveTab === 'tasks' ? 'tasks' : 'chat';
  const deleteControlHtml =
    pendingDeleteRoomId === detail.room_id
      ? `<div class="history-delete-confirm"><span>确认删除该历史记录？</span><button type="button" class="btn-danger-xs" id="btn-history-delete-confirm">确认删除</button><button type="button" class="btn-ghost-xs" id="btn-history-delete-cancel">取消</button></div>`
      : `<button type="button" class="btn-danger-soft" id="btn-history-delete-init" style="padding:5px 11px;font-size:12px">删除记录</button>`;

  const bodyHtml =
    activeTab === 'chat'
      ? messages.length > 0
        ? messages.map(renderMessageCard).join('')
        : '<div class="empty-hint">该历史会话暂无群聊对话记录。</div>'
      : tasks.length > 0
        ? tasks.map(buildHistoryTaskCardHtml).join('')
        : '<div class="empty-hint">该历史会话尚未生成子任务拆解记录。</div>';

  pane.innerHTML = `
    <div class="history-detail-header">
      <div class="history-detail-meta-row">
        <div class="history-detail-meta-left">
          <span class="history-phase-tag ${phaseCls}">${escapeHtml(phaseLabel)}</span>
          <span class="btn-chip-tag mono">${escapeHtml(detail.room_id)}</span>
        </div>
        <span class="history-room-time">更新于 ${escapeHtml(formatUpdatedAt(detail.updated_at))}</span>
      </div>
      <h4 class="history-detail-goal">${escapeHtml(detail.goal || '（未记录长期目标）')}</h4>
      <div class="history-detail-ws">工作目录：<span class="mono">${escapeHtml(detail.workspace || '-')}</span></div>
      <div class="history-detail-toolbar">
        <div class="segmented" id="history-detail-tabs">
          <button type="button" class="seg-btn ${activeTab === 'chat' ? 'active' : ''}" data-htab="chat">群聊对话 (${messages.length})</button>
          <button type="button" class="seg-btn ${activeTab === 'tasks' ? 'active' : ''}" data-htab="tasks">子任务进度 (${approvedCount}/${tasks.length})</button>
        </div>
        <div class="history-detail-actions">
          <button type="button" class="btn-secondary" id="btn-history-export" style="padding:5px 11px;font-size:12px">导出报告</button>
          ${deleteControlHtml}
        </div>
      </div>
    </div>
    <div class="history-detail-body">${bodyHtml}</div>`;

  pane.querySelectorAll('#history-detail-tabs .seg-btn').forEach((btn) => {
    btn.addEventListener('click', () => {
      appState.historyActiveTab = btn.getAttribute('data-htab') || 'chat';
      renderHistoryDetailPane();
    });
  });
  document.getElementById('btn-history-export')?.addEventListener('click', () => {
    postIpc({ cmd: 'export_history_markdown', room_id: detail.room_id });
  });
  document.getElementById('btn-history-delete-init')?.addEventListener('click', () => {
    pendingDeleteRoomId = detail.room_id;
    renderHistoryDetailPane();
  });
  document.getElementById('btn-history-delete-cancel')?.addEventListener('click', () => {
    pendingDeleteRoomId = null;
    renderHistoryDetailPane();
  });
  document.getElementById('btn-history-delete-confirm')?.addEventListener('click', () => {
    const targetId = detail.room_id;
    pendingDeleteRoomId = null;
    renderHistoryDetailPane();
    postIpc({ cmd: 'delete_history_room', room_id: targetId });
  });
}

export function renderHistoryDrawer() {
  renderHistoryList();
  renderHistoryDetailPane();
}

export function openHistoryDrawer() {
  const drawer = document.getElementById('history-drawer');
  if (!drawer) return;
  drawer.classList.remove('hidden');
  drawer.setAttribute('aria-hidden', 'false');
  appState.historyDrawerOpen = true;
  appState.historyLoadingList = true;
  shouldRefreshActiveOnList = Boolean(appState.activeHistoryRoomId);
  pendingDeleteRoomId = null;
  renderHistoryDrawer();
  postIpc({ cmd: 'list_history_rooms' });
}

export function closeHistoryDrawer() {
  const drawer = document.getElementById('history-drawer');
  if (!drawer) return;
  drawer.classList.add('hidden');
  drawer.setAttribute('aria-hidden', 'true');
  appState.historyDrawerOpen = false;
  shouldRefreshActiveOnList = false;
  pendingDeleteRoomId = null;
}

export function handleHistoryRoomsListed(rooms) {
  appState.historyRooms = Array.isArray(rooms) ? rooms : [];
  appState.historyLoadingList = false;

  const activeSummary = appState.activeHistoryRoomId
    ? appState.historyRooms.find((r) => r.room_id === appState.activeHistoryRoomId)
    : null;

  if (appState.activeHistoryRoomId && !activeSummary) {
    appState.activeHistoryRoomId = null;
    appState.activeHistoryDetail = null;
    appState.historyLoadingDetail = false;
    shouldRefreshActiveOnList = false;
  }

  if (appState.historyDrawerOpen && appState.historyRooms.length > 0) {
    if (!appState.activeHistoryRoomId) {
      shouldRefreshActiveOnList = false;
      selectHistoryRoom(appState.historyRooms[0].room_id);
      return;
    }
    if (shouldRefreshActiveOnList || hasSummaryChanged(activeSummary, appState.activeHistoryDetail)) {
      shouldRefreshActiveOnList = false;
      selectHistoryRoom(appState.activeHistoryRoomId, { silentRefresh: true });
      return;
    }
  }
  shouldRefreshActiveOnList = false;
  renderHistoryDrawer();
}

export function handleHistoryRoomLoaded(detail) {
  appState.historyLoadingDetail = false;
  if (!detail) {
    renderHistoryDrawer();
    return;
  }
  const isSameRoomRefresh =
    Boolean(appState.activeHistoryDetail) &&
    appState.activeHistoryDetail.room_id === detail.room_id;
  appState.activeHistoryRoomId = detail.room_id;
  appState.activeHistoryDetail = detail;
  pendingDeleteRoomId = null;
  if (!isSameRoomRefresh) {
    const hasMsgs = Array.isArray(detail.messages) && detail.messages.length > 0;
    const hasTasks = Array.isArray(detail.tasks) && detail.tasks.length > 0;
    appState.historyActiveTab = !hasMsgs && hasTasks ? 'tasks' : 'chat';
  }
  renderHistoryDrawer();
}

export function handleHistoryRoomDeleted(roomId, rooms) {
  appState.historyRooms = Array.isArray(rooms) ? rooms : [];
  pendingDeleteRoomId = null;
  if (appState.activeHistoryRoomId === roomId) {
    appState.activeHistoryRoomId = null;
    appState.activeHistoryDetail = null;
    appState.historyLoadingDetail = false;
  }
  renderHistoryDrawer();
}

export function bindHistoryDrawerEvents() {
  const drawer = document.getElementById('history-drawer');
  document.getElementById('btn-open-history')?.addEventListener('click', openHistoryDrawer);
  document.getElementById('btn-close-history')?.addEventListener('click', closeHistoryDrawer);
  drawer?.addEventListener('click', (e) => {
    if (e.target === drawer) closeHistoryDrawer();
  });
  document.getElementById('history-search-input')?.addEventListener('input', (e) => {
    appState.historySearchText = e.target.value;
    renderHistoryList();
  });
  document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape' && appState.historyDrawerOpen) closeHistoryDrawer();
  });
}
