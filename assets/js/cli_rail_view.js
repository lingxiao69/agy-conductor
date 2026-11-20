import { appState, escapeHtml, postIpc } from './main.js';
import {
  buildEmbeddedCliUsageHtml,
  openUsageAnalytics,
  renderUsageAnalyticsView,
  requestUsageStats,
} from './usage_analytics_view.js';

const DEFAULT_PROVIDERS = [
  {
    id: 'agy',
    display_name: 'Antigravity (agy)',
    short_label: 'agy',
    command_name: 'agy',
    installed: true,
    executable_path: null,
    version_hint: null,
    support_tier: 'active_orchestrator',
    tagline: '主力全自动多角色编队引擎 · 规划/编码/审查/实时流与官方单价计费已就绪',
  },
  {
    id: 'codex',
    display_name: 'Codex CLI',
    short_label: 'Codex',
    command_name: 'codex',
    installed: false,
    executable_path: null,
    version_hint: null,
    support_tier: 'planned',
    tagline: 'OpenAI 终端编码引擎 · 支持本机探测与独立终端拉起，多角色编排适配器预留中',
  },
  {
    id: 'claude_code',
    display_name: 'Claude Code',
    short_label: 'Claude',
    command_name: 'claude',
    installed: false,
    executable_path: null,
    version_hint: null,
    support_tier: 'planned',
    tagline: 'Anthropic Claude 终端引擎 · 支持本机探测与独立终端拉起，多角色编排适配器预留中',
  },
  {
    id: 'opencode',
    display_name: 'OpenCode',
    short_label: 'OpenCode',
    command_name: 'opencode',
    installed: false,
    executable_path: null,
    version_hint: null,
    support_tier: 'planned',
    tagline: '开源多模型终端编码助手 · 支持本机探测与独立终端拉起，多角色编排适配器预留中',
  },
];

const INSTALL_HINTS = {
  codex: 'npm install -g @openai/codex',
  claude_code: 'npm install -g @anthropic-ai/claude-code',
  opencode: 'npm install -g opencode-ai',
};

function providerSvg(id) {
  switch (id) {
    case 'agy':
      return `<svg class="cli-rail-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <path d="M12 3.2L4.5 18.8l7.5-3.4 7.5 3.4L12 3.2z" />
        <path d="M12 3.2v12.2" />
      </svg>`;
    case 'codex':
      return `<svg class="cli-rail-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <polygon points="12 2.8 20 7.4 20 16.6 12 21.2 4 16.6 4 7.4 12 2.8" />
        <path d="M9 10l2.2 2L9 14" />
        <path d="M13 14h2.5" />
      </svg>`;
    case 'claude_code':
      return `<svg class="cli-rail-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" aria-hidden="true">
        <path d="M12 3.5v17" />
        <path d="M3.5 12h17" />
        <path d="M6 6l12 12" />
        <path d="M18 6L6 18" />
        <circle cx="12" cy="12" r="2.2" fill="currentColor" stroke="none" />
      </svg>`;
    case 'opencode':
    default:
      return `<svg class="cli-rail-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <rect x="3.5" y="4.5" width="17" height="15" rx="3.2" />
        <path d="M9.5 9.2L6.8 12l2.7 2.8" />
        <path d="M14.5 9.2l2.7 2.8-2.7 2.8" />
        <path d="M12.8 8.8l-1.6 6.4" />
      </svg>`;
  }
}

function getProviders() {
  return Array.isArray(appState.cli_providers) && appState.cli_providers.length > 0
    ? appState.cli_providers
    : DEFAULT_PROVIDERS;
}

function isAgyRunning() {
  return ['Planning', 'Running', 'FinalGate', 'WaitingUser'].includes(appState.phase);
}

export function renderCliRailAndProviderStage() {
  const rail = document.getElementById('cli-nav-rail');
  const grid = document.getElementById('workspace-grid');
  const stage = document.getElementById('cli-provider-stage');
  const analyticsStage = document.getElementById('usage-analytics-stage');
  if (!rail || !grid || !stage) return;

  const providers = getProviders();
  const activeId = appState.active_cli_provider || 'agy';
  const analyticsOpen = Boolean(appState.usageAnalyticsOpen);
  const agyRunning = isAgyRunning();

  const itemsHtml = providers
    .map((p) => {
      const activeCls = !analyticsOpen && p.id === activeId ? 'active' : '';
      const dotCls =
        p.id === 'agy' && agyRunning
          ? 'running'
          : p.installed
            ? 'installed'
            : '';
      const stateHint = p.installed
        ? `已检测到本机 CLI${p.version_hint ? ` (${p.version_hint})` : ''}`
        : '本机暂未检测到';
      return `
        <button
          type="button"
          class="cli-rail-btn ${activeCls}"
          data-cli-provider="${escapeHtml(p.id)}"
          title="${escapeHtml(`${p.display_name} · ${stateHint}`)}"
          aria-pressed="${!analyticsOpen && p.id === activeId ? 'true' : 'false'}"
        >
          <span class="cli-rail-dot ${dotCls}"></span>
          ${providerSvg(p.id)}
          <span class="cli-rail-label">${escapeHtml(p.short_label)}</span>
        </button>
      `;
    })
    .join('');

  rail.innerHTML = `
    <div class="cli-rail-list">${itemsHtml}</div>
    <div class="cli-rail-spacer"></div>
    <button
      type="button"
      class="cli-rail-btn ${analyticsOpen ? 'active' : ''}"
      id="btn-open-usage-analytics"
      title="查看本机 agy / Codex / Claude Code / OpenCode 用量统计中心"
      aria-pressed="${analyticsOpen ? 'true' : 'false'}"
    >
      <svg class="cli-rail-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <path d="M4 19.5h16" />
        <rect x="6" y="12" width="3" height="5.5" rx="0.8" />
        <rect x="10.8" y="8.5" width="3" height="9" rx="0.8" />
        <rect x="15.6" y="5" width="3" height="12.5" rx="0.8" />
      </svg>
      <span class="cli-rail-label">统计</span>
    </button>
    <button
      type="button"
      class="cli-rail-refresh"
      id="btn-refresh-cli-rail"
      title="重新探测本机 agy / Codex / Claude Code / OpenCode 安装状态"
    >
      <svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <path d="M13.3 8a5.3 5.3 0 1 1-1.4-3.6" />
        <path d="M13.3 2.8v2.6h-2.6" />
      </svg>
    </button>
  `;

  grid.classList.toggle('analytics-mode', analyticsOpen);
  analyticsStage?.classList.toggle('hidden', !analyticsOpen);

  if (analyticsOpen) {
    grid.classList.remove('provider-preview-mode');
    stage.classList.add('hidden');
    renderUsageAnalyticsView();
    return;
  }

  const isPreview = activeId !== 'agy';
  grid.classList.toggle('provider-preview-mode', isPreview);
  stage.classList.toggle('hidden', !isPreview);

  if (!isPreview) {
    stage.innerHTML = '';
    return;
  }

  const current = providers.find((p) => p.id === activeId) || providers[0];
  const installCmd = INSTALL_HINTS[current.id] || `请确认 ${current.command_name} 已在 PATH 中`;
  const runningNotice = agyRunning
    ? `<div class="cli-running-notice">
        <span>Antigravity (agy) 多角色研发编排仍在后台稳定运行，当前切换不会中断任何子任务。</span>
        <button type="button" class="btn-jade-soft-sm" data-switch-provider="agy">返回 agy 协同台</button>
      </div>`
    : '';

  stage.innerHTML = `
    <div class="cli-provider-card">
      ${runningNotice}
      <div class="cli-provider-hero">
        <div class="cli-provider-identity">
          <div class="cli-provider-emblem">${providerSvg(current.id)}</div>
          <div>
            <h2 class="cli-provider-title">${escapeHtml(current.display_name)}</h2>
            <p class="cli-provider-tagline">${escapeHtml(current.tagline)}</p>
          </div>
        </div>
        <span class="cli-status-pill ${current.installed ? 'installed' : 'missing'}">
          ${current.installed ? '本机已安装' : '未检测到命令'}
        </span>
      </div>

      <div class="cli-meta-grid">
        <div class="cli-meta-box">
          <span class="cli-meta-label">命令行入口</span>
          <span class="cli-meta-val mono">${escapeHtml(current.command_name)}</span>
        </div>
        <div class="cli-meta-box">
          <span class="cli-meta-label">探测版本</span>
          <span class="cli-meta-val mono">${escapeHtml(current.version_hint || (current.installed ? '已安装（无版本回显）' : '尚未安装'))}</span>
        </div>
        <div class="cli-meta-box">
          <span class="cli-meta-label">可执行文件路径</span>
          <span class="cli-meta-val mono">${escapeHtml(current.executable_path || '未在 PATH 或常用目录找到')}</span>
        </div>
        <div class="cli-meta-box">
          <span class="cli-meta-label">当前目标项目目录</span>
          <span class="cli-meta-val mono">${escapeHtml(appState.workspace || '尚未选择（默认当前目录）')}</span>
        </div>
      </div>

      <div class="cli-provider-actions">
        <button
          type="button"
          class="btn-primary"
          data-launch-provider="${escapeHtml(current.id)}"
          ${current.installed ? '' : 'disabled'}
        >
          在当前项目目录拉起 ${escapeHtml(current.short_label)} 独立终端
        </button>
        <button type="button" class="btn-secondary" data-refresh-providers="1">重新探测本机环境</button>
        <button type="button" class="btn-secondary" data-switch-provider="agy">返回 agy 多模型协同台</button>
      </div>

      <div class="cli-roadmap-note">
        ${
          current.installed
            ? `已就绪：可随时点击上方按钮在 Windows Terminal / PowerShell 中免配置拉起 <strong>${escapeHtml(current.display_name)}</strong> 交互终端。轻舟多模型自动编排（规划-编码-审查-计费）协议适配器已为该引擎预留统一接口。`
            : `尚未在本机检测到 <span class="mono">${escapeHtml(current.command_name)}</span>。如需使用，请先在终端安装（示例：<span class="mono">${escapeHtml(installCmd)}</span>），完成后点击「重新探测本机环境」即可直接一键拉起。`
        }
      </div>
    </div>
    ${buildEmbeddedCliUsageHtml(current.id)}
  `;
}

export function bindCliRailEvents() {
  document.getElementById('cli-nav-rail')?.addEventListener('click', (e) => {
    if (e.target.closest('#btn-open-usage-analytics')) {
      openUsageAnalytics('all');
      return;
    }
    const providerBtn = e.target.closest('[data-cli-provider]');
    if (providerBtn) {
      const provider = providerBtn.getAttribute('data-cli-provider');
      if (provider && (appState.usageAnalyticsOpen || provider !== appState.active_cli_provider)) {
        appState.usageAnalyticsOpen = false;
        appState.active_cli_provider = provider;
        renderCliRailAndProviderStage();
        postIpc({ cmd: 'select_cli_provider', provider });
      }
      return;
    }
    if (e.target.closest('#btn-refresh-cli-rail')) {
      postIpc({ cmd: 'refresh_cli_providers' });
    }
  });

  document.getElementById('cli-provider-stage')?.addEventListener('click', (e) => {
    const launchBtn = e.target.closest('[data-launch-provider]');
    if (launchBtn) {
      const provider = launchBtn.getAttribute('data-launch-provider');
      if (provider) postIpc({ cmd: 'launch_provider_external_cli', provider });
      return;
    }
    const switchBtn = e.target.closest('[data-switch-provider]');
    if (switchBtn) {
      const provider = switchBtn.getAttribute('data-switch-provider');
      if (provider) {
        appState.usageAnalyticsOpen = false;
        appState.active_cli_provider = provider;
        renderCliRailAndProviderStage();
        postIpc({ cmd: 'select_cli_provider', provider });
      }
      return;
    }
    const openAnalyticsBtn = e.target.closest('[data-open-analytics]');
    if (openAnalyticsBtn) {
      openUsageAnalytics(openAnalyticsBtn.getAttribute('data-open-analytics') || 'all');
      return;
    }
    if (e.target.closest('[data-sync-usage]')) {
      requestUsageStats(appState.usageRange || 'days7', true);
      return;
    }
    if (e.target.closest('[data-refresh-providers]')) {
      postIpc({ cmd: 'refresh_cli_providers' });
    }
  });
}
