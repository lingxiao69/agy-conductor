import { appState, escapeHtml, postIpc } from './main.js';
import { formatTokenCount, formatUsd } from './billing_view.js';
import { renderCliRailAndProviderStage } from './cli_rail_view.js';
import {
  CLI_META,
  CLI_ORDER,
  RANGE_OPTIONS,
  buildChartTooltipHtml,
  buildModelTableHtml,
  buildProviderShareHtml,
  buildTrendSvgHtml,
} from './usage_charts.js';

export function normalizeAnalyticsCliId(raw) {
  if (!raw || raw === 'all') return 'all';
  if (raw === 'claude_code') return 'claude';
  return CLI_META[raw] ? raw : 'all';
}

export function requestUsageStats(range, forceRefresh = false) {
  appState.usageRange = range || appState.usageRange || 'days7';
  appState.usageLoading = true;
  if (appState.usageAnalyticsOpen) {
    renderUsageAnalyticsView();
  }
  postIpc({
    cmd: 'load_cli_usage_stats',
    range: appState.usageRange,
    force_refresh: Boolean(forceRefresh),
  });
}

export function openUsageAnalytics(filterCli = 'all') {
  appState.usageAnalyticsOpen = true;
  appState.usageFilterCli = normalizeAnalyticsCliId(filterCli);
  renderCliRailAndProviderStage();
  renderUsageAnalyticsView();
  if (!appState.usageLoading) {
    requestUsageStats(appState.usageRange || 'days7', false);
  }
}

export function closeUsageAnalytics() {
  appState.usageAnalyticsOpen = false;
  renderCliRailAndProviderStage();
}

export function handleCliUsageStatsLoaded(overview) {
  appState.usageOverview = overview || null;
  appState.usageLoading = false;
  if (overview?.range) {
    appState.usageRange = overview.range;
  }
  if (appState.usageAnalyticsOpen) {
    renderUsageAnalyticsView();
  }
}

function formatLocalYmd(date) {
  const y = date.getFullYear();
  const m = String(date.getMonth() + 1).padStart(2, '0');
  const d = String(date.getDate()).padStart(2, '0');
  return `${y}-${m}-${d}`;
}

function getFilteredSummary(overview, filterCli) {
  if (!overview) return null;
  const providers = Array.isArray(overview.providers) ? overview.providers : [];
  const models = Array.isArray(overview.models) ? overview.models : [];
  const daily = Array.isArray(overview.daily) ? overview.daily : [];

  if (filterCli === 'all') {
    return {
      cost: Number(overview.total_cost_usd) || 0,
      totalTokens: Number(overview.total_tokens) || 0,
      inputTokens: Number(overview.input_tokens) || 0,
      cacheReadTokens: Number(overview.cache_read_tokens) || 0,
      outputTokens: Number(overview.output_tokens) || 0,
      cacheHitRatio: Number(overview.cache_hit_ratio) || 0,
      sessions: Number(overview.total_sessions) || 0,
      steps: Number(overview.total_steps) || 0,
      avgDailyCost: Number(overview.avg_daily_cost_usd) || 0,
      todayCost: Number(overview.today_cost_usd) || 0,
      yesterdayCost: Number(overview.yesterday_cost_usd) || 0,
      providers,
      models,
      daily,
    };
  }

  const p = providers.find((item) => item.provider_id === filterCli) || {};
  const filteredModels = models.filter((m) => m.provider_id === filterCli);
  const now = new Date();
  const todayStr = formatLocalYmd(now);
  const yesterdayStr = formatLocalYmd(
    new Date(now.getFullYear(), now.getMonth(), now.getDate() - 1)
  );
  const todayPt = daily.find((pt) => pt.date === todayStr);
  const yesterdayPt = daily.find((pt) => pt.date === yesterdayStr);
  const activeDays = Math.max(
    1,
    daily.filter(
      (pt) =>
        (Number(pt?.by_provider_tokens?.[filterCli]) || 0) > 0 ||
        (Number(pt?.by_provider_cost?.[filterCli]) || 0) > 0
    ).length
  );
  const cost = Number(p.cost_usd) || 0;

  return {
    cost,
    totalTokens: Number(p.total_tokens) || 0,
    inputTokens: Number(p.input_tokens) || 0,
    cacheReadTokens: Number(p.cache_read_tokens) || 0,
    outputTokens: Number(p.output_tokens) || 0,
    cacheHitRatio: Number(p.cache_hit_ratio) || 0,
    sessions: Number(p.sessions) || 0,
    steps: Number(p.steps) || 0,
    avgDailyCost: cost / activeDays,
    todayCost: Number(todayPt?.by_provider_cost?.[filterCli]) || 0,
    yesterdayCost: Number(yesterdayPt?.by_provider_cost?.[filterCli]) || 0,
    providers,
    models: filteredModels,
    daily,
  };
}

export function renderUsageAnalyticsView() {
  const stage = document.getElementById('usage-analytics-stage');
  if (!stage) return;

  const overview = appState.usageOverview;
  const filterCli = appState.usageFilterCli || 'all';
  const metric = appState.usageMetric || 'cost';
  const agyRunning = ['Planning', 'Running', 'FinalGate', 'WaitingUser'].includes(appState.phase);
  const s = getFilteredSummary(overview, filterCli);

  const cliPills = [
    { id: 'all', label: '全部 CLI', color: '#2D6A5B' },
    ...CLI_ORDER.map((id) => ({ id, label: CLI_META[id].short, color: CLI_META[id].color })),
  ]
    .map((item) => {
      const active = filterCli === item.id ? 'active' : '';
      return `<button type="button" class="usage-cli-pill ${active}" data-usage-cli="${item.id}"><span class="usage-dot" style="background:${item.color}"></span>${escapeHtml(item.label)}</button>`;
    })
    .join('');

  const rangeBtns = RANGE_OPTIONS.map((r) => {
    const active = (appState.usageRange || 'days7') === r.id ? 'active' : '';
    return `<button type="button" class="seg-btn ${active}" data-usage-range="${r.id}">${r.label}</button>`;
  }).join('');

  const legendItems = (filterCli === 'all' ? CLI_ORDER : [filterCli])
    .map(
      (id) =>
        `<span class="usage-legend-item"><span class="usage-dot" style="background:${CLI_META[id].color}"></span>${escapeHtml(CLI_META[id].short)}</span>`
    )
    .join('');

  const costCompare =
    s && s.yesterdayCost > 0
      ? `今日 ${formatUsd(s.todayCost)} · 昨日 ${formatUsd(s.yesterdayCost)}`
      : s
        ? `日均 ${formatUsd(s.avgDailyCost)}`
        : '本地只读解析中...';

  const syncMeta = overview
    ? `上次同步 ${overview.synced_at} · 扫描 ${overview.scanned_files} 份日志（增量命中 ${overview.cache_hits}）`
    : '纯本地只读解析 agy / Codex / Claude Code / OpenCode 日志，零网络与代理侵入';

  stage.innerHTML = `
    <div class="usage-analytics-inner">
      <div class="usage-header-bar">
        <div class="usage-title-wrap">
          <h2>轻舟雅集 · 本地多 CLI 用量统计</h2>
          <p class="usage-subtitle">${escapeHtml(syncMeta)}</p>
        </div>
        <div class="usage-right-controls">
          ${agyRunning ? '<button type="button" class="btn-jade-soft-sm" data-usage-back-agy="1">返回运行中的 agy 协同台</button>' : '<button type="button" class="btn-secondary" data-usage-back-agy="1">返回 agy 协同台</button>'}
        </div>
      </div>

      <div class="usage-filter-bar">
        <div class="usage-cli-pills">${cliPills}</div>
        <div class="usage-right-controls">
          <div class="segmented">${rangeBtns}</div>
          <button type="button" class="btn-secondary" data-usage-sync="1" ${appState.usageLoading ? 'disabled' : ''}>
            ${appState.usageLoading ? '同步中...' : '↻ 同步本地记录'}
          </button>
        </div>
      </div>

      <div class="usage-kpi-grid">
        <div class="usage-kpi-card" style="--kpi-accent:#2D6A5B">
          <span class="usage-kpi-label">累计折算花费 (USD)</span>
          <span class="usage-kpi-val mono">${escapeHtml(formatUsd(s?.cost || 0))}</span>
          <span class="usage-kpi-sub mono">${escapeHtml(costCompare)}</span>
        </div>
        <div class="usage-kpi-card" style="--kpi-accent:#355C8A">
          <span class="usage-kpi-label">总消耗 Token</span>
          <span class="usage-kpi-val mono">${escapeHtml(formatTokenCount(s?.totalTokens || 0))}</span>
          <span class="usage-kpi-sub mono">新输入 ${escapeHtml(formatTokenCount(s?.inputTokens || 0))} · 输出 ${escapeHtml(formatTokenCount(s?.outputTokens || 0))}</span>
        </div>
        <div class="usage-kpi-card" style="--kpi-accent:#B8732A">
          <span class="usage-kpi-label">缓存读取与命中率</span>
          <span class="usage-kpi-val mono">${((s?.cacheHitRatio || 0) * 100).toFixed(1)}%</span>
          <span class="usage-kpi-sub mono">缓存读取 ${escapeHtml(formatTokenCount(s?.cacheReadTokens || 0))}</span>
        </div>
        <div class="usage-kpi-card" style="--kpi-accent:#7A4E73">
          <span class="usage-kpi-label">活跃会话 / 交互步数</span>
          <span class="usage-kpi-val mono">${s?.sessions || 0} / ${s?.steps || 0}</span>
          <span class="usage-kpi-sub">本机只读聚合 · 不接触密钥与账号表</span>
        </div>
      </div>

      <div class="usage-chart-card">
        <div class="usage-card-head">
          <span class="usage-card-title">每日消耗趋势</span>
          <div class="usage-right-controls">
            <div class="usage-legend-row">${legendItems}</div>
            <div class="segmented">
              <button type="button" class="seg-btn ${metric === 'cost' ? 'active' : ''}" data-usage-metric="cost">按花费 ($)</button>
              <button type="button" class="seg-btn ${metric === 'tokens' ? 'active' : ''}" data-usage-metric="tokens">按 Token</button>
            </div>
          </div>
        </div>
        <div class="usage-chart-wrap" id="usage-chart-wrap">
          ${buildTrendSvgHtml(s?.daily || [], filterCli, metric)}
        </div>
      </div>

      <div class="usage-detail-grid">
        <div class="usage-panel-card">
          <div class="usage-card-head"><span class="usage-card-title">各 CLI 引擎消耗占比</span></div>
          <div class="usage-panel-scroll">${buildProviderShareHtml(s?.providers || [])}</div>
        </div>
        <div class="usage-panel-card">
          <div class="usage-card-head">
            <span class="usage-card-title">模型消耗排行榜</span>
            <span class="pane-sub-count">${s?.models?.length || 0}</span>
          </div>
          <div class="usage-panel-scroll">${buildModelTableHtml(s?.models || [], filterCli === 'all')}</div>
        </div>
      </div>
    </div>
  `;
}

export function buildEmbeddedCliUsageHtml(railProviderId) {
  const pid = normalizeAnalyticsCliId(railProviderId);
  const overview = appState.usageOverview;
  const provider = overview?.providers?.find((p) => p.provider_id === pid);
  const rangeLbl = RANGE_OPTIONS.find((r) => r.id === (appState.usageRange || 'days7'))?.label || '近 7 天';

  if (!overview) {
    return `
      <div class="cli-embedded-usage">
        <div class="usage-card-head" style="margin-bottom:0">
          <span class="usage-card-title">本机历史用量统计</span>
          <button type="button" class="btn-secondary" data-sync-usage="1">立即同步记录</button>
        </div>
        <div class="empty-hint" style="padding:12px 0">正在只读聚合本地 CLI 历史日志...</div>
      </div>
    `;
  }

  const p = provider || { sessions: 0, steps: 0, total_tokens: 0, cache_hit_ratio: 0, cost_usd: 0, top_models: [] };
  return `
    <div class="cli-embedded-usage">
      <div class="usage-card-head" style="margin-bottom:0">
        <span class="usage-card-title">本机真实历史用量（${escapeHtml(rangeLbl)}）</span>
        <div class="usage-right-controls">
          <button type="button" class="btn-ghost-sm" data-sync-usage="1">↻ 刷新</button>
          <button type="button" class="btn-jade-soft-sm" data-open-analytics="${escapeHtml(pid)}">打开完整统计中心 →</button>
        </div>
      </div>
      <div class="cli-embedded-kpis">
        <div class="cli-embedded-kpi"><span class="cli-meta-label">折算花费</span><div class="cli-embedded-kpi-val mono">${escapeHtml(formatUsd(p.cost_usd))}</div></div>
        <div class="cli-embedded-kpi"><span class="cli-meta-label">总 Token</span><div class="cli-embedded-kpi-val mono">${escapeHtml(formatTokenCount(p.total_tokens))}</div></div>
        <div class="cli-embedded-kpi"><span class="cli-meta-label">缓存命中率</span><div class="cli-embedded-kpi-val mono">${((Number(p.cache_hit_ratio) || 0) * 100).toFixed(1)}%</div></div>
        <div class="cli-embedded-kpi"><span class="cli-meta-label">会话 / 步数</span><div class="cli-embedded-kpi-val mono">${p.sessions} / ${p.steps}</div></div>
      </div>
      ${buildModelTableHtml((p.top_models || []).slice(0, 6), false)}
    </div>
  `;
}

export function bindUsageAnalyticsEvents() {
  const stage = document.getElementById('usage-analytics-stage');
  if (!stage) return;

  stage.addEventListener('click', (e) => {
    const cliBtn = e.target.closest('[data-usage-cli]');
    if (cliBtn) {
      appState.usageFilterCli = cliBtn.getAttribute('data-usage-cli') || 'all';
      renderUsageAnalyticsView();
      return;
    }
    const rangeBtn = e.target.closest('[data-usage-range]');
    if (rangeBtn) {
      requestUsageStats(rangeBtn.getAttribute('data-usage-range') || 'days7', false);
      return;
    }
    const metricBtn = e.target.closest('[data-usage-metric]');
    if (metricBtn) {
      appState.usageMetric = metricBtn.getAttribute('data-usage-metric') || 'cost';
      renderUsageAnalyticsView();
      return;
    }
    if (e.target.closest('[data-usage-sync]')) {
      requestUsageStats(appState.usageRange || 'days7', true);
      return;
    }
    if (e.target.closest('[data-usage-back-agy]')) {
      appState.usageAnalyticsOpen = false;
      appState.active_cli_provider = 'agy';
      renderCliRailAndProviderStage();
      postIpc({ cmd: 'select_cli_provider', provider: 'agy' });
    }
  });

  stage.addEventListener('mousemove', (e) => {
    const hit = e.target.closest('.usage-bar-hit');
    const tip = document.getElementById('usage-chart-tooltip');
    const wrap = document.getElementById('usage-chart-wrap');
    if (!hit || !tip || !wrap) {
      tip?.classList.add('hidden');
      return;
    }
    const pt = (appState.usageOverview?.daily || [])[Number(hit.getAttribute('data-bar-idx'))];
    if (!pt) {
      tip.classList.add('hidden');
      return;
    }
    tip.innerHTML = buildChartTooltipHtml(pt, appState.usageFilterCli || 'all');
    tip.classList.remove('hidden');
    const rect = wrap.getBoundingClientRect();
    tip.style.left = `${Math.min(rect.width - 210, Math.max(8, e.clientX - rect.left + 12))}px`;
    tip.style.top = `${Math.max(4, e.clientY - rect.top - 26)}px`;
  });

  stage.addEventListener('mouseleave', () => {
    document.getElementById('usage-chart-tooltip')?.classList.add('hidden');
  });
}
