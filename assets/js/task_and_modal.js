import { appState, escapeHtml, postIpc } from './main.js';
import { openTerminalDrawer } from './terminal_drawer.js';

export function formatAgyVersionTag(raw, withPrefix = true) {
  const v = String(raw || '').trim();
  if (!v) return withPrefix ? 'agy 检测中' : '检测中...';
  const normalized = /^\d/.test(v) ? `v${v}` : v;
  return withPrefix ? `agy ${normalized}` : normalized;
}

const PHASE_LABELS = {
  Idle: ['待命', 'idle'],
  Planning: ['总架构师规划中', 'working'],
  Running: ['编码与审查中', 'working'],
  FinalGate: ['全局终验中', 'working'],
  Paused: ['已暂停', 'warn'],
  WaitingUser: ['等待人工拍板', 'warn'],
  Completed: ['全部验收通过', 'ok'],
  Stopped: ['已停止', 'idle'],
  Failed: ['运行异常', 'error'],
};

export function renderTopbar() {
  const wsEl = document.getElementById('topbar-workspace-path');
  const wsInput = document.getElementById('input-workspace');
  if (wsEl) {
    wsEl.textContent = appState.workspace || '尚未选择项目目录';
    wsEl.title = appState.workspace || '点击选择本地项目目录';
  }
  if (wsInput && document.activeElement !== wsInput && appState.workspace) {
    wsInput.value = appState.workspace;
  }

  const verEl = document.getElementById('topbar-agy-version');
  if (verEl) {
    verEl.textContent = formatAgyVersionTag(appState.agy_version, true);
  }

  const [baseText, dotClass] = PHASE_LABELS[appState.phase] || ['待命', 'idle'];
  const total = appState.tasks.length;
  const approved = appState.tasks.filter((t) => t.status === 'Approved').length;
  const phaseText =
    total > 0 && (appState.phase === 'Running' || appState.phase === 'Completed')
      ? `${baseText} (${approved}/${total} 已验收)`
      : baseText;

  const dotEl = document.getElementById('topbar-phase-dot');
  const textEl = document.getElementById('topbar-phase-text');
  if (dotEl) dotEl.className = `status-dot ${dotClass}`;
  if (textEl) textEl.textContent = phaseText;

  const modeSelect = document.getElementById('select-dispatch-mode');
  if (modeSelect && modeSelect.value !== appState.dispatch_mode) {
    modeSelect.value = appState.dispatch_mode;
  }
}

export function syncPermissionCards() {
  const checked =
    document.querySelector('input[name="permission_mode"]:checked')?.value || 'AcceptEdits';
  appState.permission_mode = checked;
  document.getElementById('perm-card-accept')?.classList.toggle('selected', checked === 'AcceptEdits');
  document
    .getElementById('perm-card-skip')
    ?.classList.toggle('selected', checked === 'DangerouslySkipPermissions');
  document
    .getElementById('danger-consent-row')
    ?.classList.toggle('hidden', checked !== 'DangerouslySkipPermissions');
}

export function syncPermissionRadiosFromState() {
  const radio = document.querySelector(
    `input[name="permission_mode"][value="${appState.permission_mode}"]`
  );
  if (radio) radio.checked = true;
  const sb = document.getElementById('chk-sandbox');
  if (sb) sb.checked = Boolean(appState.enable_sandbox);
  const mr = document.getElementById('input-max-rework');
  if (mr) mr.value = String(appState.max_rework_rounds || 8);
  syncPermissionCards();
}

const TASK_STATUS_META = {
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

export function renderTasksAndControls() {
  const list = document.getElementById('task-list');
  const ratio = document.getElementById('task-progress-ratio');
  const controls = document.getElementById('run-controls');
  const pauseBtn = document.getElementById('btn-pause-resume');

  const total = appState.tasks.length;
  const approved = appState.tasks.filter((t) => t.status === 'Approved').length;
  const tasksBox = document.getElementById('pane-right-tasks');
  if (tasksBox) tasksBox.classList.toggle('is-empty', total === 0);
  if (ratio) ratio.textContent = `${approved}/${total}`;

  if (list) {
    if (total === 0) {
      list.innerHTML =
        '<div class="empty-hint">启动长期目标后，总架构师拆解的子任务与审查返工进度将在此清晰呈现。</div>';
    } else {
      list.innerHTML = appState.tasks
        .map((t) => {
          const [icon, statusText] = TASK_STATUS_META[t.status] || ['○', t.status];
          const coder = t.assigned_coder_label ? `${t.assigned_coder_label} · ` : '';
          const rework = t.rework_count > 0 ? ` · 第 ${t.rework_count} 轮返工` : '';
          const arb =
            t.arch_arbitration_count > 0 ? ` · 架构裁决 ${t.arch_arbitration_count} 次` : '';
          const deps =
            t.status === 'Pending' && Array.isArray(t.depends_on) && t.depends_on.length > 0
              ? ` · 等待 ${t.depends_on.join(', ')}`
              : '';
          const paths =
            Array.isArray(t.owned_paths) && t.owned_paths.length > 0
              ? t.owned_paths.join(', ')
              : '全仓';

          return `
            <div class="task-card">
              <div class="task-title-row">
                <span class="task-icon ${escapeHtml(t.status)}">${icon}</span>
                <span>${escapeHtml(t.task_id)} ${escapeHtml(t.title)}</span>
              </div>
              <div class="task-meta">${escapeHtml(coder + statusText + rework + arb + deps)}</div>
              <div class="task-paths" title="${escapeHtml(paths)}">${escapeHtml(paths)}</div>
            </div>
          `;
        })
        .join('');
    }
  }

  const active = ['Planning', 'Running', 'FinalGate', 'Paused', 'WaitingUser'].includes(
    appState.phase
  );
  if (controls) controls.classList.toggle('hidden', !active);
  if (pauseBtn) {
    const paused = appState.phase === 'Paused' || appState.phase === 'WaitingUser';
    pauseBtn.textContent = paused ? '恢复协同' : '暂停协同';
  }
}

export function renderMaintenanceInfo() {
  const grid = document.getElementById('maintenance-info-grid');
  if (grid) {
    const gwCount = appState.models.filter((m) => m.is_gateway).length;
    const offCount = appState.models.length - gwCount;
    const doctorNotes =
      appState.doctor && Array.isArray(appState.doctor.notes) && appState.doctor.notes.length > 0
        ? appState.doctor.notes.join('；')
        : '运行正常';

    grid.innerHTML = `
      <div class="info-cell">
        <div class="info-cell-label">本机 agy 版本</div>
        <div class="info-cell-val mono">${escapeHtml(formatAgyVersionTag(appState.agy_version, false))}</div>
      </div>
      <div class="info-cell">
        <div class="info-cell-label">启动入口形态</div>
        <div class="info-cell-val">${escapeHtml(appState.runtime_kind)}</div>
      </div>
      <div class="info-cell">
        <div class="info-cell-label">可用模型总数</div>
        <div class="info-cell-val">${appState.models.length} 个（Gateway ${gwCount} / 官方 ${offCount}）</div>
      </div>
      <div class="info-cell">
        <div class="info-cell-label">本机终端宿主</div>
        <div class="info-cell-val">${escapeHtml(appState.terminal_host)}</div>
      </div>
      <div class="info-cell" style="grid-column: span 2">
        <div class="info-cell-label">入口路径与诊断</div>
        <div class="info-cell-val mono" style="font-weight:400;font-size:11.5px">${escapeHtml(appState.runtime_path || '-')}</div>
        <div style="font-size:11.5px;color:var(--text-secondary);margin-top:4px">${escapeHtml(doctorNotes)}</div>
      </div>
    `;
  }

  document.querySelectorAll('#gateway-pref-toggle .seg-btn').forEach((btn) => {
    const want = btn.getAttribute('data-gw') === 'true';
    btn.classList.toggle('active', want === Boolean(appState.prefer_gateway));
  });

  const cliHostDesc = document.getElementById('cli-host-desc');
  if (cliHostDesc) {
    cliHostDesc.textContent = appState.supports_split_pane
      ? `已识别 ${appState.terminal_host}，支持一键多窗格分屏`
      : `已识别 ${appState.terminal_host}，将以独立终端窗口打开`;
  }
}

export function appendUpdateLog(line, finished, newVersion) {
  const box = document.getElementById('update-log-box');
  if (box) {
    if (!appState.updateRunning) {
      box.textContent = '';
      appState.updateRunning = true;
    }
    box.textContent += `${line}\n`;
    box.scrollTop = box.scrollHeight;
  }
  if (finished) {
    appState.updateRunning = false;
    const btn = document.getElementById('btn-run-agy-update');
    if (btn) btn.disabled = false;
    if (newVersion) {
      appState.agy_version = newVersion;
      renderMaintenanceInfo();
    }
  }
}

export function bindModalsAndControls() {
  const maintDrawer = document.getElementById('maintenance-drawer');
  const cliModal = document.getElementById('cli-modal');

  document.getElementById('btn-open-maintenance')?.addEventListener('click', () => {
    maintDrawer?.classList.remove('hidden');
    postIpc({ cmd: 'run_doctor' });
  });
  document.getElementById('btn-close-maintenance')?.addEventListener('click', () => {
    maintDrawer?.classList.add('hidden');
  });
  maintDrawer?.addEventListener('click', (e) => {
    if (e.target === maintDrawer) maintDrawer.classList.add('hidden');
  });

  document.getElementById('btn-run-agy-update')?.addEventListener('click', (e) => {
    e.currentTarget.disabled = true;
    const box = document.getElementById('update-log-box');
    if (box) box.textContent = '正在执行前置安全检查...\n';
    appState.updateRunning = true;
    postIpc({ cmd: 'run_agy_update' });
  });

  document.getElementById('btn-rescan-models')?.addEventListener('click', () => {
    postIpc({ cmd: 'refresh_models' });
  });

  document.getElementById('btn-run-doctor')?.addEventListener('click', () => {
    postIpc({ cmd: 'run_doctor' });
  });

  document.querySelectorAll('#gateway-pref-toggle .seg-btn').forEach((btn) => {
    btn.addEventListener('click', () => {
      const prefer = btn.getAttribute('data-gw') === 'true';
      postIpc({ cmd: 'toggle_prefer_gateway', prefer_gateway: prefer });
    });
  });

  document.getElementById('btn-open-cli-modal')?.addEventListener('click', () => {
    openTerminalDrawer(null);
  });
  document.getElementById('btn-close-cli-modal')?.addEventListener('click', () => {
    cliModal?.classList.add('hidden');
  });
  cliModal?.addEventListener('click', (e) => {
    if (e.target === cliModal) cliModal.classList.add('hidden');
  });

  document.getElementById('btn-launch-live-split')?.addEventListener('click', () => {
    postIpc({
      cmd: 'open_terminal',
      slot_id: null,
      attach_conversation: false,
      split_all: true,
      mount_mcp: false,
      watch_live: true,
    });
    cliModal?.classList.add('hidden');
  });

  document.getElementById('btn-launch-split-cli')?.addEventListener('click', () => {
    const mountMcp = Boolean(document.getElementById('chk-mount-mcp')?.checked);
    postIpc({
      cmd: 'open_terminal',
      slot_id: null,
      attach_conversation: false,
      split_all: true,
      mount_mcp: mountMcp,
      watch_live: false,
    });
    cliModal?.classList.add('hidden');
  });

  document.getElementById('btn-launch-blank-cli')?.addEventListener('click', () => {
    const mountMcp = Boolean(document.getElementById('chk-mount-mcp')?.checked);
    postIpc({
      cmd: 'open_terminal',
      slot_id: null,
      attach_conversation: false,
      split_all: false,
      mount_mcp: mountMcp,
      watch_live: false,
    });
    cliModal?.classList.add('hidden');
  });

  document.getElementById('btn-pause-resume')?.addEventListener('click', () => {
    const paused = appState.phase === 'Paused' || appState.phase === 'WaitingUser';
    postIpc({ cmd: paused ? 'resume_run' : 'pause_run' });
  });

  document.getElementById('btn-stop-run')?.addEventListener('click', () => {
    postIpc({ cmd: 'stop_run' });
  });
}
