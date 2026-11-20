import { appState, escapeHtml } from './main.js';

export function formatTokenCount(raw) {
  const n = Math.max(0, Number(raw) || 0);
  if (n === 0) return '0 tok';
  if (n < 1000) return `${Math.round(n)} tok`;
  if (n < 1_000_000) return `${(n / 1000).toFixed(1)}k tok`;
  return `${(n / 1_000_000).toFixed(2)}M tok`;
}

export function formatShortContext(raw) {
  const n = Math.max(0, Number(raw) || 0);
  if (n === 0) return '0';
  if (n < 1000) return String(Math.round(n));
  if (n < 1_000_000) return `${Math.round(n / 1000)}k`;
  return `${(n / 1_000_000).toFixed(2)}M`;
}

export function formatUsd(raw) {
  const v = Math.max(0, Number(raw) || 0);
  if (v === 0) return '$0.0000';
  if (v < 1) return `$${v.toFixed(4)}`;
  if (v < 100) return `$${v.toFixed(3)}`;
  return `$${v.toFixed(2)}`;
}

function getSlotUsage(slot) {
  const u = slot?.usage || {};
  return {
    input_tokens: Number(u.input_tokens) || 0,
    cache_read_tokens: Number(u.cache_read_tokens) || 0,
    output_tokens: Number(u.output_tokens) || 0,
    thinking_tokens: Number(u.thinking_tokens) || 0,
    total_tokens: Number(u.total_tokens) || 0,
    peak_context_tokens: Number(u.peak_context_tokens) || 0,
    context_limit_tokens: Math.max(1, Number(u.context_limit_tokens) || 200_000),
    cost_usd: Number(u.cost_usd) || 0,
    rate_label: String(u.rate_label || '官方标准价'),
    rate_detail: String(u.rate_detail || ''),
  };
}

function buildBillingTooltip(slot, u) {
  const lines = [
    `官方定价基准：${u.rate_detail || u.rate_label}`,
    `累计消耗：${u.total_tokens.toLocaleString()} tokens（折合 ${formatUsd(u.cost_usd)}）`,
    `输入：${u.input_tokens.toLocaleString()} · 缓存命中：${u.cache_read_tokens.toLocaleString()}`,
    `输出：${u.output_tokens.toLocaleString()} · 思考：${u.thinking_tokens.toLocaleString()}`,
    `单步峰值上下文：${formatShortContext(u.peak_context_tokens)}`,
  ];
  if (slot?.state === 'Working') {
    lines.push('当前轮次实时增量追踪中（轮次结束后按官方阶梯价权威结算）');
  }
  return lines.join('\n');
}

function renderTokenInnerHtml(u, working) {
  const pulse = working ? '<span class="slot-billing-pulse" aria-hidden="true"></span>' : '';
  return `${pulse}<span>已用 ${escapeHtml(formatTokenCount(u.total_tokens))}</span>`;
}

export function buildSlotBillingHtml(slot) {
  const u = getSlotUsage(slot);
  const working = slot?.state === 'Working';
  const tooltip = buildBillingTooltip(slot, u);
  const costText = formatUsd(u.cost_usd);

  return `
    <div class="slot-billing-meter" data-billing-slot="${escapeHtml(slot.slot_id)}" title="${escapeHtml(tooltip)}">
      <div class="slot-billing-nums mono">
        <span class="slot-token-text">${renderTokenInnerHtml(u, working)}</span>
        <span class="slot-cost-badge">${escapeHtml(costText)}</span>
      </div>
    </div>
  `;
}

/**
 * 局部就地刷新单个槽位卡片的 Token 数与美元金额，
 * 避免轮次中增量推送时重建整个编队列表 DOM 而打断用户内联编辑。
 */
export function updateSlotBillingDom(slot) {
  if (!slot?.slot_id) return;
  const meter = document.querySelector(
    `.slot-billing-meter[data-billing-slot="${CSS.escape(slot.slot_id)}"]`
  );
  if (!meter) return;

  const u = getSlotUsage(slot);
  const working = slot?.state === 'Working';
  meter.title = buildBillingTooltip(slot, u);

  const tokEl = meter.querySelector('.slot-token-text');
  if (tokEl) {
    tokEl.innerHTML = renderTokenInnerHtml(u, working);
  }
  const costEl = meter.querySelector('.slot-cost-badge');
  if (costEl) {
    costEl.textContent = formatUsd(u.cost_usd);
  }
}

export function renderTeamBillingFooter() {
  const slots = Array.isArray(appState.slots) ? appState.slots : [];
  let totalTokens = 0;
  let totalCost = 0;
  for (const s of slots) {
    totalTokens += Number(s?.usage?.total_tokens) || 0;
    totalCost += Number(s?.usage?.cost_usd) || 0;
  }

  const footer = document.getElementById('roster-billing-footer');
  if (footer) {
    footer.innerHTML = `
      <span class="roster-billing-label">本轮团队累计</span>
      <span class="roster-billing-val mono">
        <span>${escapeHtml(formatTokenCount(totalTokens))}</span>
        <span class="roster-billing-usd">${escapeHtml(formatUsd(totalCost))}</span>
      </span>
    `;
    footer.title = `全部槽位累计消耗 ${totalTokens.toLocaleString()} tokens，按各模型官方标准价折算约 ${formatUsd(totalCost)}（不含折扣）`;
  }

  const topPill = document.getElementById('topbar-cost-pill');
  if (topPill) {
    const todayCost = Number(appState.usageOverview?.today_cost_usd) || 0;
    if (totalTokens > 0) {
      topPill.classList.remove('hidden');
      topPill.textContent = `${formatTokenCount(totalTokens)} · ${formatUsd(totalCost)}`;
      topPill.title = '本轮团队累计消耗（点击打开本地多 CLI 用量统计中心）';
    } else if (todayCost > 0) {
      topPill.classList.remove('hidden');
      topPill.textContent = `今日 ${formatUsd(todayCost)}`;
      topPill.title = '本机四大 CLI 今日累计折算花费（点击打开用量统计中心）';
    } else if (appState.usageOverview) {
      topPill.classList.remove('hidden');
      topPill.textContent = '用量统计';
      topPill.title = '点击打开本地多 CLI 用量统计中心';
    } else {
      topPill.classList.add('hidden');
      topPill.textContent = '';
    }
  }
}
