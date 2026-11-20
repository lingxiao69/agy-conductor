import { renderRoster, closeSlotCliMenu } from './roster_view.js';
import { renderChatAndActivity } from './chat_view.js';
import { renderPresetsBar, bindPresetsEvents } from './presets_view.js';
import { bindCliRailEvents, renderCliRailAndProviderStage } from './cli_rail_view.js';
import { updateSlotBillingDom, renderTeamBillingFooter } from './billing_view.js';
import {
  bindHistoryDrawerEvents,
  renderHistoryDrawer,
  handleHistoryRoomsListed,
  handleHistoryRoomLoaded,
  handleHistoryRoomDeleted,
} from './history_drawer.js';
import {
  bindTerminalDrawerEvents,
  renderTerminalDrawer,
  appendLiveTerminalLine,
  clearTerminalLinesUi,
} from './terminal_drawer.js';
import {
  bindSessionResumeEvents,
  renderSessionResumePanel,
  getSelectedSessionsForStart,
  resetSessionSelections,
} from './session_resume_view.js';
import {
  renderTasksAndControls,
  renderMaintenanceInfo,
  renderTopbar,
  appendUpdateLog,
  bindModalsAndControls,
  syncPermissionCards,
  syncPermissionRadiosFromState,
} from './task_and_modal.js';
import {
  bindReportModalEvents,
  openBlockerReportModal,
  openCompletionReportModal,
} from './report_modal.js';
import {
  bindUsageAnalyticsEvents,
  handleCliUsageStatsLoaded,
  openUsageAnalytics,
} from './usage_analytics_view.js';
import {
  bindLiveSidePaneEvents,
  renderLiveSidePane,
  appendLiveSidePaneLine,
  updateLiveSidePaneStatus,
  clearLiveSidePaneUi,
  resetLiveSidePaneFollow,
} from './live_side_pane.js';

export const appState = {
  agy_version: '检测中...',
  runtime_kind: '检测中...',
  runtime_path: '',
  active_cli_provider: 'agy',
  cli_providers: [],
  terminal_host: '自动检测',
  supports_split_pane: false,
  prefer_gateway: true,
  workspace: '',
  goal: '',
  phase: 'Idle',
  dispatch_mode: 'PriorityWithOverflow',
  permission_mode: 'AcceptEdits',
  enable_sandbox: false,
  max_rework_rounds: 8,
  reuse_role_sessions: true,
  workspace_sessions: [],
  terminal_lines: [],
  active_blocker: null,
  selectedSessions: {},
  terminalDrawerOpen: false,
  terminalFilterSlotId: 'all',
  livePanePinnedSlotId: null,
  livePaneAutoSlotId: null,
  models: [],
  slots: [],
  presets: [],
  active_preset_id: null,
  tasks: [],
  messages: [],
  completion: null,
  doctor: null,
  historyRooms: [],
  activeHistoryRoomId: null,
  activeHistoryDetail: null,
  historySearchText: '',
  historyDrawerOpen: false,
  historyLoadingList: false,
  historyLoadingDetail: false,
  historyActiveTab: 'chat',
  editingSlotId: null,
  modelFilterFamily: 'All',
  modelSearchText: '',
  goalExpanded: false,
  rightCollapsed: false,
  updateRunning: false,
  usageAnalyticsOpen: false,
  usageOverview: null,
  usageRange: 'days7',
  usageFilterCli: 'all',
  usageMetric: 'cost',
  usageLoading: false,
};

export function postIpc(payload) {
  if (window.ipc && typeof window.ipc.postMessage === 'function') {
    window.ipc.postMessage(JSON.stringify(payload));
  }
}

export function escapeHtml(raw) {
  return String(raw ?? '')
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&#39;');
}

export function showToast(level, message) {
  const stack = document.getElementById('toast-stack');
  if (!stack) return;
  const el = document.createElement('div');
  el.className = `toast ${level || 'info'}`;
  el.textContent = message;
  stack.appendChild(el);
  window.setTimeout(() => {
    el.style.opacity = '0';
    window.setTimeout(() => el.remove(), 220);
  }, 4200);
}

function renderStageVisibility() {
  const setupStage = document.getElementById('setup-stage');
  const chatStage = document.getElementById('chat-stage');
  const isIdleEmpty = appState.phase === 'Idle' && appState.messages.length === 0;
  setupStage.classList.toggle('hidden', !isIdleEmpty);
  chatStage.classList.toggle('hidden', isIdleEmpty);

  const banner = document.getElementById('goal-banner');
  const bannerText = document.getElementById('goal-banner-text');
  const btnNewGoal = document.getElementById('btn-new-goal');
  if (bannerText) bannerText.textContent = appState.goal || '';
  if (banner) banner.classList.toggle('expanded', appState.goalExpanded);
  const canStartNew = ['Completed', 'Stopped', 'Failed', 'Idle'].includes(appState.phase);
  if (btnNewGoal) btnNewGoal.classList.toggle('hidden', !canStartNew);
}

export function renderAll() {
  renderCliRailAndProviderStage();
  renderTopbar();
  renderStageVisibility();
  renderSessionResumePanel();
  renderPresetsBar();
  renderRoster();
  renderTeamBillingFooter();
  renderChatAndActivity();
  renderTasksAndControls();
  renderLiveSidePane({ forceLines: true });
  renderMaintenanceInfo();
  if (appState.historyDrawerOpen) {
    renderHistoryDrawer();
  }
  if (appState.terminalDrawerOpen) {
    renderTerminalDrawer();
  }
}

function normalizePathKey(p) {
  return String(p || '')
    .trim()
    .replaceAll('/', '\\')
    .replace(/\\+$/, '')
    .toLowerCase();
}

function bindCoreEvents() {
  document.getElementById('topbar-cost-pill')?.addEventListener('click', () => {
    openUsageAnalytics('all');
  });
  document.getElementById('btn-pick-workspace-top')?.addEventListener('click', () => {
    postIpc({ cmd: 'pick_workspace_folder' });
  });
  document.getElementById('btn-pick-workspace-form')?.addEventListener('click', () => {
    postIpc({ cmd: 'pick_workspace_folder' });
  });

  document.getElementById('input-workspace')?.addEventListener('change', (e) => {
    const val = e.target.value.trim();
    appState.workspace = val;
    resetSessionSelections();
    postIpc({ cmd: 'set_workspace_folder', workspace: val });
  });

  document.querySelectorAll('input[name="permission_mode"]').forEach((el) => {
    el.addEventListener('change', syncPermissionCards);
  });

  document.getElementById('btn-start-goal')?.addEventListener('click', () => {
    const ws = document.getElementById('input-workspace').value.trim();
    const goal = document.getElementById('input-goal').value.trim();
    const perm =
      document.querySelector('input[name="permission_mode"]:checked')?.value || 'AcceptEdits';
    const sandbox = Boolean(document.getElementById('chk-sandbox')?.checked);
    const maxRework = Math.max(
      1,
      Math.min(20, Number(document.getElementById('input-max-rework')?.value || 8))
    );

    if (!ws) {
      showToast('warning', '请先选择目标项目目录');
      return;
    }
    if (!goal) {
      showToast('warning', '请输入清晰的长期研发目标');
      return;
    }
    if (
      perm === 'DangerouslySkipPermissions' &&
      !document.getElementById('chk-danger-consent')?.checked
    ) {
      showToast('warning', '请先勾选知情确认后再启用全自动放行模式');
      return;
    }

    appState.workspace = ws;
    appState.goal = goal;
    resetLiveSidePaneFollow();
    postIpc({
      cmd: 'start_goal',
      workspace: ws,
      goal,
      permission_mode: perm,
      enable_sandbox: sandbox,
      max_rework_rounds: maxRework,
      reuse_role_sessions: appState.reuse_role_sessions !== false,
      selected_sessions: getSelectedSessionsForStart(),
    });
  });

  document.getElementById('chat-composer')?.addEventListener('submit', (e) => {
    e.preventDefault();
    const input = document.getElementById('input-user-message');
    const text = input.value.trim();
    if (!text) return;
    input.value = '';
    postIpc({ cmd: 'post_user_message', text });
  });

  document.getElementById('btn-toggle-goal-detail')?.addEventListener('click', (e) => {
    appState.goalExpanded = !appState.goalExpanded;
    e.target.textContent = appState.goalExpanded ? '收起' : '展开';
    renderStageVisibility();
  });

  document.getElementById('btn-new-goal')?.addEventListener('click', () => {
    resetLiveSidePaneFollow();
    postIpc({ cmd: 'reset_run' });
  });

  document.getElementById('btn-toggle-right-pane')?.addEventListener('click', () => {
    appState.rightCollapsed = !appState.rightCollapsed;
    document
      .getElementById('workspace-grid')
      ?.classList.toggle('right-collapsed', appState.rightCollapsed);
  });

  document.addEventListener('click', (e) => {
    if (!e.target.closest('#slot-cli-menu') && !e.target.closest('.btn-cli-mini')) {
      closeSlotCliMenu();
    }
  });
}

window.__agyDispatch = function handleBackendEvent(evt) {
  if (!evt || !evt.type) return;
  switch (evt.type) {
    case 'state_snapshot': {
      const { type: _t, ...snapshot } = evt;
      Object.assign(appState, snapshot);
      if (!Array.isArray(appState.presets)) appState.presets = [];
      if (!Array.isArray(appState.workspace_sessions)) appState.workspace_sessions = [];
      if (!Array.isArray(appState.terminal_lines)) appState.terminal_lines = [];
      if (!Array.isArray(appState.cli_providers)) appState.cli_providers = [];
      if (!appState.active_cli_provider) appState.active_cli_provider = 'agy';
      syncPermissionRadiosFromState();
      renderAll();
      break;
    }
    case 'cli_providers_updated':
      if (evt.active_provider) appState.active_cli_provider = evt.active_provider;
      if (Array.isArray(evt.providers)) appState.cli_providers = evt.providers;
      renderCliRailAndProviderStage();
      break;
    case 'workspace_selected':
      if (appState.workspace !== (evt.workspace || '')) {
        resetSessionSelections();
      }
      appState.workspace = evt.workspace || '';
      renderTopbar();
      renderCliRailAndProviderStage();
      break;
    case 'workspace_sessions_discovered': {
      const currentKey = normalizePathKey(appState.workspace);
      const evtKey = normalizePathKey(evt.workspace);
      if (currentKey && evtKey && currentKey !== evtKey) {
        break;
      }
      appState.workspace_sessions = Array.isArray(evt.sessions) ? evt.sessions : [];
      if (typeof evt.reuse_role_sessions === 'boolean') {
        appState.reuse_role_sessions = evt.reuse_role_sessions;
      }
      renderSessionResumePanel();
      renderRoster();
      break;
    }
    case 'models_updated':
      appState.agy_version = evt.version;
      appState.runtime_kind = evt.runtime_kind;
      appState.runtime_path = evt.runtime_path;
      appState.models = evt.models || [];
      appState.prefer_gateway = Boolean(evt.prefer_gateway);
      if (Array.isArray(evt.slots) && evt.slots.length > 0) {
        appState.slots = evt.slots;
      }
      renderAll();
      break;
    case 'doctor_updated':
      appState.doctor = evt;
      renderMaintenanceInfo();
      break;
    case 'phase_changed':
      appState.phase = evt.phase;
      if (evt.phase !== 'WaitingUser') {
        appState.active_blocker = null;
      }
      renderCliRailAndProviderStage();
      renderTopbar();
      renderStageVisibility();
      renderChatAndActivity();
      renderTasksAndControls();
      break;
    case 'slot_state_changed': {
      const slot = appState.slots.find((s) => s.slot_id === evt.slot_id);
      if (slot) {
        slot.state = evt.state;
        slot.current_activity = evt.activity ?? null;
        if (evt.conversation_id) slot.conversation_id = evt.conversation_id;
        if (typeof evt.session_reused === 'boolean') slot.session_reused = evt.session_reused;
      }
      renderRoster();
      renderChatAndActivity();
      renderLiveSidePane();
      if (appState.terminalDrawerOpen) renderTerminalDrawer();
      break;
    }
    case 'slot_usage_updated': {
      const slot = appState.slots.find((s) => s.slot_id === evt.slot_id);
      if (slot && evt.usage) {
        slot.usage = evt.usage;
        updateSlotBillingDom(slot);
        renderTeamBillingFooter();
        updateLiveSidePaneStatus(slot);
      }
      break;
    }
    case 'chat_message_appended':
      appState.messages.push(evt);
      renderStageVisibility();
      renderChatAndActivity(true);
      break;
    case 'tasks_updated':
      appState.tasks = evt.tasks || evt;
      if (!Array.isArray(appState.tasks)) {
        appState.tasks = [];
      }
      renderTopbar();
      renderTasksAndControls();
      break;
    case 'terminal_line_appended':
      appendLiveTerminalLine(evt);
      appendLiveSidePaneLine(evt);
      break;
    case 'terminal_log_cleared':
      clearTerminalLinesUi();
      clearLiveSidePaneUi();
      break;
    case 'blocker_needs_user':
      appState.active_blocker = evt;
      appState.phase = 'WaitingUser';
      renderTopbar();
      renderStageVisibility();
      renderChatAndActivity(true);
      renderTasksAndControls();
      openBlockerReportModal(evt);
      break;
    case 'update_log_chunk':
      appendUpdateLog(evt.line, evt.finished, evt.new_version);
      break;
    case 'project_completed':
      appState.completion = evt;
      appState.active_blocker = null;
      appState.phase = 'Completed';
      renderAll();
      openCompletionReportModal(evt);
      break;
    case 'cli_usage_stats_loaded':
      handleCliUsageStatsLoaded(evt.overview || null);
      renderTeamBillingFooter();
      renderCliRailAndProviderStage();
      break;
    case 'system_toast':
      showToast(evt.level, evt.message);
      if (
        (evt.level === 'error' || evt.level === 'warning') &&
        (appState.historyLoadingList || appState.historyLoadingDetail)
      ) {
        if (appState.historyLoadingDetail && !appState.activeHistoryDetail) {
          appState.activeHistoryRoomId = null;
        }
        appState.historyLoadingList = false;
        appState.historyLoadingDetail = false;
        if (appState.historyDrawerOpen) renderHistoryDrawer();
      }
      break;
    case 'history_rooms_listed':
      handleHistoryRoomsListed(evt.rooms || []);
      break;
    case 'history_room_loaded':
      handleHistoryRoomLoaded(evt.detail || null);
      break;
    case 'history_room_deleted':
      handleHistoryRoomDeleted(evt.room_id, evt.rooms || []);
      break;
    default:
      break;
  }
};

document.addEventListener('DOMContentLoaded', () => {
  bindCoreEvents();
  bindCliRailEvents();
  bindPresetsEvents();
  bindSessionResumeEvents();
  bindHistoryDrawerEvents();
  bindTerminalDrawerEvents();
  bindLiveSidePaneEvents();
  bindModalsAndControls();
  bindReportModalEvents();
  bindUsageAnalyticsEvents();
  syncPermissionCards();
  postIpc({ cmd: 'init_app' });
});
