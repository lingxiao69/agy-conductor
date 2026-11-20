import { escapeHtml } from './main.js';
import { formatTokenCount, formatUsd } from './billing_view.js';

export const CLI_META = {
  agy: { label: 'Antigravity (agy)', short: 'agy', color: '#2D6A5B', bg: '#EAF3F0' },
  codex: { label: 'Codex CLI', short: 'Codex', color: '#355C8A', bg: '#ECF2F8' },
  claude: { label: 'Claude Code', short: 'Claude', color: '#B8732A', bg: '#FBF3E8' },
  opencode: { label: 'OpenCode', short: 'OpenCode', color: '#7A4E73', bg: '#F5EEF4' },
};

export const CLI_ORDER = ['agy', 'codex', 'claude', 'opencode'];

export const RANGE_OPTIONS = [
  { id: 'today', label: '今日' },
  { id: 'days7', label: '近 7 天' },
  { id: 'days30', label: '近 30 天' },
  { id: 'all_time', label: '全部累计' },
];

export function buildTrendSvgHtml(daily, filterCli, metric) {
  if (!Array.isArray(daily) || daily.length === 0) {
    return '<div class="empty-hint" style="padding:56px 0">当前时间范围内暂无本地消耗记录</div>';
  }
  const activeProviders = filterCli === 'all' ? CLI_ORDER : [filterCli];
  const values = daily.map((pt) => {
    const map = metric === 'tokens' ? pt.by_provider_tokens || {} : pt.by_provider_cost || {};
    let total = 0;
    const segs = [];
    for (const pid of activeProviders) {
      const v = Math.max(0, Number(map[pid]) || 0);
      if (v > 0) {
        segs.push({ pid, v });
        total += v;
      }
    }
    return { pt, segs, total };
  });

  const maxVal = Math.max(...values.map((v) => v.total), 0);
  if (maxVal <= 0) {
    return '<div class="empty-hint" style="padding:56px 0">所选范围与 CLI 引擎暂无用量数据</div>';
  }

  const width = 880;
  const height = 205;
  const padL = 58;
  const padR = 16;
  const padT = 14;
  const padB = 30;
  const plotW = width - padL - padR;
  const plotH = height - padT - padB;
  const n = values.length;
  const slotW = plotW / Math.max(1, n);
  const barW = Math.max(6, Math.min(34, slotW * 0.56));
  const labelStride = n <= 10 ? 1 : n <= 20 ? 2 : Math.ceil(n / 10);

  const gridLines = [0, 0.33, 0.66, 1]
    .map((ratio) => {
      const y = (padT + plotH * (1 - ratio)).toFixed(1);
      const tickVal = maxVal * ratio;
      const tickLabel =
        metric === 'tokens' ? formatTokenCount(tickVal).replace(' tok', '') : formatUsd(tickVal);
      return `
        <line x1="${padL}" y1="${y}" x2="${width - padR}" y2="${y}" stroke="#E7E4DC" stroke-dasharray="${ratio === 0 ? '' : '3 3'}" />
        <text x="${padL - 8}" y="${Number(y) + 3.5}" text-anchor="end" font-size="10.5" fill="#8C939C" class="mono">${escapeHtml(tickLabel)}</text>
      `;
    })
    .join('');

  const bars = values
    .map((item, idx) => {
      const cx = padL + slotW * idx + slotW / 2;
      const bx = cx - barW / 2;
      let accH = 0;
      const rects = item.segs
        .map((seg, sIdx) => {
          const h = (seg.v / maxVal) * plotH;
          const y = padT + plotH - accH - h;
          accH += h;
          const color = CLI_META[seg.pid]?.color || '#2D6A5B';
          const rx = sIdx === item.segs.length - 1 ? 3 : 0;
          return `<rect x="${bx.toFixed(1)}" y="${y.toFixed(1)}" width="${barW.toFixed(1)}" height="${Math.max(1.5, h).toFixed(1)}" rx="${rx}" fill="${color}" />`;
        })
        .join('');
      const showLbl = idx % labelStride === 0 || idx === n - 1;
      const xLabel = showLbl
        ? `<text x="${cx.toFixed(1)}" y="${height - 10}" text-anchor="middle" font-size="10.5" fill="#8C939C" class="mono">${escapeHtml(item.pt.label)}</text>`
        : '';
      const hitRect = `<rect class="usage-bar-hit" data-bar-idx="${idx}" x="${(padL + slotW * idx).toFixed(1)}" y="${padT}" width="${slotW.toFixed(1)}" height="${plotH}" fill="transparent" style="cursor:pointer" />`;
      return `${rects}${xLabel}${hitRect}`;
    })
    .join('');

  return `
    <svg class="usage-chart-svg" viewBox="0 0 ${width} ${height}" preserveAspectRatio="none" role="img" aria-label="每日用量趋势图">
      ${gridLines}
      ${bars}
    </svg>
    <div class="usage-chart-tooltip hidden" id="usage-chart-tooltip"></div>
  `;
}

export function buildProviderShareHtml(providers) {
  if (!Array.isArray(providers) || providers.length === 0) {
    return '<div class="empty-hint">暂无本地 CLI 数据</div>';
  }
  return providers
    .map((p) => {
      const meta = CLI_META[p.provider_id] || { label: p.display_name, color: '#2D6A5B' };
      const sharePct = Math.round((Number(p.cost_share) || 0) * 1000) / 10;
      const hitPct = Math.round((Number(p.cache_hit_ratio) || 0) * 100);
      return `
        <div class="usage-provider-row">
          <div class="usage-provider-top">
            <span class="usage-provider-name">
              <span class="usage-dot" style="background:${meta.color}"></span>
              <span>${escapeHtml(meta.label)}</span>
            </span>
            <span class="mono" style="font-weight:600;color:${meta.color}">${escapeHtml(formatUsd(p.cost_usd))}</span>
          </div>
          <div class="usage-bar-track">
            <div class="usage-bar-fill" style="width:${Math.min(100, sharePct)}%;background:${meta.color}"></div>
          </div>
          <div class="usage-provider-nums mono">
            <span>${p.sessions} 会话 · ${p.steps} 步 · 缓存 ${hitPct}%</span>
            <span>${escapeHtml(formatTokenCount(p.total_tokens))} (${sharePct.toFixed(1)}%)</span>
          </div>
        </div>
      `;
    })
    .join('');
}

export function buildModelTableHtml(models, showCliBadge) {
  if (!Array.isArray(models) || models.length === 0) {
    return '<div class="empty-hint" style="padding:28px 0">暂无模型调用消耗记录</div>';
  }
  const rows = models
    .slice(0, 40)
    .map((m) => {
      const meta = CLI_META[m.provider_id] || {
        short: m.provider_id,
        color: '#2D6A5B',
        bg: '#EAF3F0',
      };
      const chip = showCliBadge
        ? `<span class="usage-cli-chip" style="background:${meta.bg};color:${meta.color}">${escapeHtml(meta.short)}</span>`
        : '';
      return `
        <tr>
          <td class="mono" title="${escapeHtml(m.model_id)}">${escapeHtml(m.model_id)}${chip}</td>
          <td class="num mono">${m.steps}</td>
          <td class="num mono">${escapeHtml(formatTokenCount(m.input_tokens))}</td>
          <td class="num mono">${escapeHtml(formatTokenCount(m.cache_read_tokens))}</td>
          <td class="num mono">${escapeHtml(formatTokenCount(m.output_tokens))}</td>
          <td class="num mono" style="font-weight:600;color:var(--accent-jade)">${escapeHtml(formatUsd(m.cost_usd))}</td>
        </tr>
      `;
    })
    .join('');

  return `
    <table class="usage-model-table">
      <thead>
        <tr>
          <th>模型型号</th>
          <th class="num">步数</th>
          <th class="num">新输入</th>
          <th class="num">缓存读</th>
          <th class="num">输出</th>
          <th class="num">花费 ($)</th>
        </tr>
      </thead>
      <tbody>${rows}</tbody>
    </table>
  `;
}

export function buildChartTooltipHtml(pt, filterCli) {
  const providers = filterCli === 'all' ? CLI_ORDER : [filterCli];
  const rows = providers
    .map((pid) => {
      const c = Number(pt.by_provider_cost?.[pid]) || 0;
      const t = Number(pt.by_provider_tokens?.[pid]) || 0;
      if (c <= 0 && t <= 0) return '';
      const meta = CLI_META[pid];
      return `<div class="usage-tooltip-row"><span><span class="usage-dot" style="display:inline-block;margin-right:5px;background:${meta.color}"></span>${escapeHtml(meta.short)}</span><span>${escapeHtml(formatUsd(c))} · ${escapeHtml(formatTokenCount(t))}</span></div>`;
    })
    .filter(Boolean)
    .join('');
  return `<div class="usage-tooltip-date mono">${escapeHtml(pt.date)}</div>${rows || '<div style="color:var(--text-muted)">当日无消耗</div>'}`;
}
