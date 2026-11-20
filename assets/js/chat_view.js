import { appState, escapeHtml, postIpc } from './main.js';
import { openTerminalDrawer } from './terminal_drawer.js';
import { formatTokenCount, formatUsd } from './billing_view.js';
import { openBlockerReportModal, openCompletionReportModal } from './report_modal.js';

const BADGE_LABELS = {
  Plan: '规划派单',
  Submit: '提请审查',
  Approved: '验收通过',
  Rework: '打回修改',
  Blocked: '架构阻塞',
  FinalAccept: '全局终验通过',
  User: '人工指导',
  Info: '系统提示',
  Warning: '告警',
};

const ROLE_LABELS = {
  Architect: '总架构师',
  Coder: '编码程序员',
  Reviewer: '代码审查员',
};

function renderRichMentions(rawText) {
  const escaped = escapeHtml(rawText || '');
  return escaped.replace(/(^|\s)(@[a-zA-Z0-9_-]+)/g, '$1<span class="mention-tag">$2</span>');
}

export function renderMessageCard(msg) {
  const cardCls = msg.is_system ? 'msg-card system' : msg.is_user ? 'msg-card user' : 'msg-card';
  const roleTag = msg.sender_role
    ? `<span class="role-pill ${escapeHtml(msg.sender_role)}">${ROLE_LABELS[msg.sender_role] || escapeHtml(msg.sender_role)}</span>`
    : '';
  const prioTag = msg.priority ? `<span class="priority-badge">P${msg.priority}</span>` : '';
  const badgeText = BADGE_LABELS[msg.badge];
  const badgeHtml = badgeText
    ? `<span class="msg-badge ${escapeHtml(msg.badge)}">${badgeText}</span>`
    : '';
  const taskTag = msg.task_id
    ? `<span class="btn-chip-tag mono">${escapeHtml(msg.task_id)}</span>`
    : '';

  const issuesHtml =
    Array.isArray(msg.issues) && msg.issues.length > 0
      ? `<div class="msg-issues">
          ${msg.issues
            .map((iss) => {
              const loc = iss.line_hint
                ? `${iss.file_path}:${iss.line_hint}`
                : iss.file_path || '';
              return `<div class="issue-item">
                <span class="issue-sev">[${escapeHtml(iss.severity)}]</span>
                <span class="mono">${escapeHtml(loc)}</span>
                <span>${escapeHtml(iss.detail)}</span>
              </div>`;
            })
            .join('')}
        </div>`
      : '';

  const metaParts = [];
  if (Array.isArray(msg.modified_files) && msg.modified_files.length > 0) {
    metaParts.push(`变更 ${msg.modified_files.length} 个文件`);
  }
  if (typeof msg.duration_seconds === 'number' && msg.duration_seconds > 0) {
    metaParts.push(`耗时 ${Math.round(msg.duration_seconds)}s`);
  }
  if (typeof msg.tool_steps === 'number' && msg.tool_steps > 0) {
    metaParts.push(`${msg.tool_steps} 步工具操作`);
  }

  const detailsHtml =
    metaParts.length > 0
      ? `<details class="msg-details">
          <summary>${escapeHtml(metaParts.join(' · '))}</summary>
          ${
            msg.modified_files && msg.modified_files.length > 0
              ? `<ul class="msg-file-list">${msg.modified_files
                  .map((f) => `<li>${escapeHtml(f)}</li>`)
                  .join('')}</ul>`
              : ''
          }
        </details>`
      : '';

  return `
    <article class="${cardCls}">
      <div class="msg-header">
        <div class="msg-sender-group">
          ${roleTag}
          <span class="msg-sender-name">${escapeHtml(msg.sender_display)}</span>
          ${prioTag}
          ${taskTag}
          ${badgeHtml}
        </div>
        <span class="msg-time">${escapeHtml(msg.timestamp)}</span>
      </div>
      <div class="msg-body">${renderRichMentions(msg.content)}</div>
      ${issuesHtml}
      ${detailsHtml}
    </article>
  `;
}

function renderCompletionBanner() {
  const banner = document.getElementById('completion-banner');
  if (!banner) return;
  const c = appState.completion;
  if (!c) {
    banner.classList.add('hidden');
    return;
  }

  const mins = Math.floor((c.total_duration_seconds || 0) / 60);
  const secs = (c.total_duration_seconds || 0) % 60;
  const dur = mins > 0 ? `${mins} 分 ${secs} 秒` : `${secs} 秒`;
  const contrib = (c.contributions || [])
    .map((item) => {
      const costPart =
        item.usage && item.usage.total_tokens > 0
          ? ` · ${formatTokenCount(item.usage.total_tokens)} / ${formatUsd(item.usage.cost_usd)}`
          : '';
      return `${escapeHtml(item.display_name)} (${item.turns} 轮${escapeHtml(costPart)})`;
    })
    .join(' · ');
  const billingPart =
    c.total_tokens > 0
      ? `<span>累计消耗：<strong class="mono">${escapeHtml(formatTokenCount(c.total_tokens))} · ${escapeHtml(formatUsd(c.total_cost_usd))}</strong></span>`
      : '';
  const cmds = (c.verified_commands || [])
    .map((cmd) => `<span class="btn-chip-tag mono">${escapeHtml(cmd)}</span>`)
    .join(' ');

  banner.classList.remove('hidden');
  banner.innerHTML = `
    <div class="completion-title">项目开发与审查已全部验收通过</div>
    <div class="completion-meta">
      <span>总耗时：${dur}</span>
      <span>完成子任务：${c.total_tasks} 项</span>
      <span>累计打回返工：${c.total_rework_rounds} 轮</span>
      ${billingPart}
      ${contrib ? `<span>贡献：${contrib}</span>` : ''}
    </div>
    ${cmds ? `<div style="display:flex;flex-wrap:wrap;gap:6px">${cmds}</div>` : ''}
    <div class="completion-actions">
      <button type="button" class="btn-primary" id="btn-complete-open-report">查看完整报告</button>
      <button type="button" class="btn-secondary" id="btn-complete-open-dir">打开项目目录</button>
      <button type="button" class="btn-secondary" id="btn-complete-open-cli">拉起终端验收</button>
    </div>
  `;

  document.getElementById('btn-complete-open-report')?.addEventListener('click', () => {
    openCompletionReportModal(appState.completion);
  });
  document.getElementById('btn-complete-open-dir')?.addEventListener('click', () => {
    postIpc({ cmd: 'open_workspace_explorer' });
  });
  document.getElementById('btn-complete-open-cli')?.addEventListener('click', () => {
    postIpc({
      cmd: 'open_terminal',
      slot_id: null,
      attach_conversation: false,
      split_all: false,
      mount_mcp: false,
      watch_live: false,
    });
  });
}

function renderBlockerBanner() {
  const banner = document.getElementById('blocker-banner');
  if (!banner) return;
  const alert = appState.active_blocker;
  if (!alert || appState.phase !== 'WaitingUser') {
    banner.classList.add('hidden');
    banner.innerHTML = '';
    return;
  }

  const taskTag = alert.task_id
    ? `<span class="btn-chip-tag mono">${escapeHtml(alert.task_id)}</span>`
    : '';
  banner.classList.remove('hidden');
  banner.innerHTML = `
    <div class="blocker-banner-top">
      <div class="blocker-banner-title">
        <span>⚠ ${escapeHtml(alert.title || '需要您拍板确认')}</span>
        ${taskTag}
      </div>
    </div>
    <div class="blocker-banner-body">${escapeHtml(alert.plain_explanation || '')}</div>
    <div class="blocker-banner-actions">
      <button type="button" class="btn-primary" id="btn-blocker-focus-reply">去底部输入框回复拍板</button>
      <button type="button" class="btn-secondary" id="btn-blocker-open-modal">展开详情弹窗</button>
      <button type="button" class="btn-secondary" id="btn-blocker-open-live">查看实时进程终端</button>
    </div>
  `;

  document.getElementById('btn-blocker-focus-reply')?.addEventListener('click', () => {
    const input = document.getElementById('input-user-message');
    if (input) {
      input.focus();
      input.scrollIntoView({ behavior: 'smooth', block: 'nearest' });
    }
  });
  document.getElementById('btn-blocker-open-modal')?.addEventListener('click', () => {
    openBlockerReportModal(appState.active_blocker);
  });
  document.getElementById('btn-blocker-open-live')?.addEventListener('click', () => {
    openTerminalDrawer(null);
  });
}

function renderLiveActivityCapsules() {
  const bar = document.getElementById('live-activity-bar');
  if (!bar) return;
  const working = appState.slots.filter((s) => s.state === 'Working');
  if (working.length === 0) {
    bar.classList.add('hidden');
    bar.innerHTML = '';
    return;
  }

  bar.classList.remove('hidden');
  const capsules = working
    .map((s) => {
      const act = s.current_activity || '正在分析与执行...';
      return `
        <div class="activity-capsule">
          <span class="status-dot working"></span>
          <span class="activity-capsule-text">
            <strong>${escapeHtml(s.alias)} (P${s.priority})</strong> ${escapeHtml(act)}
          </span>
        </div>
      `;
    })
    .join('');

  bar.innerHTML = `
    ${capsules}
    <button type="button" class="btn-live-proc" id="btn-live-open-terminal">
      <span class="mono">&gt;_</span> 查看实时进程
    </button>
  `;

  document.getElementById('btn-live-open-terminal')?.addEventListener('click', () => {
    openTerminalDrawer(working.length === 1 ? working[0].slot_id : null);
  });
}

export function renderChatAndActivity(scrollToBottom = false) {
  renderCompletionBanner();
  renderBlockerBanner();
  renderLiveActivityCapsules();

  const stream = document.getElementById('chat-stream');
  if (!stream) return;
  stream.innerHTML = appState.messages.map(renderMessageCard).join('');
  if (scrollToBottom) {
    stream.scrollTop = stream.scrollHeight;
  }
}
