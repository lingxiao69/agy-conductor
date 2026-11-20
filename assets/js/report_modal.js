import { escapeHtml, postIpc, showToast } from './main.js';
import { formatTokenCount, formatUsd } from './billing_view.js';

let activeReportTitle = '';
let activeReportMarkdown = '';

function formatInlineMarkdown(escapedLine) {
  const codeSlots = [];
  let text = escapedLine.replace(/`([^`]+)`/g, (_m, code) => {
    const idx = codeSlots.length;
    codeSlots.push(`<code>${code}</code>`);
    return `\u0000CODE${idx}\u0000`;
  });
  text = text.replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>');
  text = text.replace(/(^|\s)(@[a-zA-Z0-9_-]+)/g, '$1<span class="mention-tag">$2</span>');
  return text.replace(/\u0000CODE(\d+)\u0000/g, (_m, i) => codeSlots[Number(i)] || '');
}

/**
 * 安全的轻量 Markdown 渲染器：先全局 escapeHtml 杜绝 XSS，再排版标题、代码块、列表与加粗。
 */
export function renderSafeMarkdown(rawText) {
  const normalized = String(rawText ?? '').replace(/\r\n/g, '\n').trim();
  if (!normalized) {
    return '<p style="color:var(--text-muted)">暂无详细报告正文。</p>';
  }

  const lines = normalized.split('\n');
  const out = [];
  let inCode = false;
  let codeLines = [];
  let listMode = null;

  const closeList = () => {
    if (listMode) {
      out.push(`</${listMode}>`);
      listMode = null;
    }
  };

  for (const rawLine of lines) {
    const trimmed = rawLine.trim();
    if (trimmed.startsWith('```')) {
      closeList();
      if (!inCode) {
        inCode = true;
        codeLines = [];
      } else {
        out.push(`<pre><code>${escapeHtml(codeLines.join('\n'))}</code></pre>`);
        inCode = false;
        codeLines = [];
      }
      continue;
    }
    if (inCode) {
      codeLines.push(rawLine);
      continue;
    }

    if (!trimmed) {
      closeList();
      continue;
    }

    if (/^(-{3,}|\*{3,})$/.test(trimmed)) {
      closeList();
      out.push('<hr />');
      continue;
    }

    const heading = trimmed.match(/^(#{1,4})\s+(.+)$/);
    if (heading) {
      closeList();
      const lvl = Math.min(4, Math.max(2, heading[1].length));
      out.push(`<h${lvl}>${formatInlineMarkdown(escapeHtml(heading[2]))}</h${lvl}>`);
      continue;
    }

    const quote = trimmed.match(/^>\s?(.*)$/);
    if (quote) {
      closeList();
      out.push(`<blockquote>${formatInlineMarkdown(escapeHtml(quote[1]))}</blockquote>`);
      continue;
    }

    const ul = trimmed.match(/^[-*•]\s+(.+)$/);
    if (ul) {
      if (listMode !== 'ul') {
        closeList();
        listMode = 'ul';
        out.push('<ul>');
      }
      out.push(`<li>${formatInlineMarkdown(escapeHtml(ul[1]))}</li>`);
      continue;
    }

    const ol = trimmed.match(/^\d+[.)]\s+(.+)$/);
    if (ol) {
      if (listMode !== 'ol') {
        closeList();
        listMode = 'ol';
        out.push('<ol>');
      }
      out.push(`<li>${formatInlineMarkdown(escapeHtml(ol[1]))}</li>`);
      continue;
    }

    closeList();
    out.push(`<p>${formatInlineMarkdown(escapeHtml(rawLine))}</p>`);
  }

  if (inCode && codeLines.length > 0) {
    out.push(`<pre><code>${escapeHtml(codeLines.join('\n'))}</code></pre>`);
  }
  closeList();
  return out.join('\n');
}

function formatDuration(secRaw) {
  const sec = Math.max(0, Number(secRaw) || 0);
  const mins = Math.floor(sec / 60);
  const rem = sec % 60;
  return mins > 0 ? `${mins} 分 ${rem} 秒` : `${rem} 秒`;
}

function buildCompletionMarkdown(summary) {
  const parts = [
    `# 项目验收报告`,
    '',
    `- **项目目录**：\`${summary.workspace || '-'}\``,
    `- **长期目标**：${summary.goal || '-'}`,
    `- **总耗时**：${formatDuration(summary.total_duration_seconds)}`,
    `- **完成子任务**：${summary.total_tasks || 0} 项 · 返工 ${summary.total_rework_rounds || 0} 轮`,
  ];
  if ((summary.total_tokens || 0) > 0) {
    parts.push(
      `- **累计消耗**：${formatTokenCount(summary.total_tokens)} · ${formatUsd(summary.total_cost_usd)}`
    );
  }
  if (Array.isArray(summary.verified_commands) && summary.verified_commands.length > 0) {
    parts.push(`- **已验证命令**：${summary.verified_commands.map((c) => `\`${c}\``).join('、')}`);
  }
  parts.push('', '---', '', summary.architect_note || '全部验收通过。');

  if (Array.isArray(summary.contributions) && summary.contributions.length > 0) {
    parts.push('', '### 编队成员贡献');
    for (const item of summary.contributions) {
      const tokPart =
        item.usage && item.usage.total_tokens > 0
          ? `（${formatTokenCount(item.usage.total_tokens)} · ${formatUsd(item.usage.cost_usd)}）`
          : '';
      parts.push(`- **${item.display_name}**：${item.turns} 轮${tokPart}`);
    }
  }
  return parts.join('\n');
}

function showReportModal({ isBlocker, badge, title, pills, markdownBody, rawExportMarkdown }) {
  const overlay = document.getElementById('report-modal-overlay');
  const card = document.getElementById('report-modal-card');
  const badgeEl = document.getElementById('report-modal-badge');
  const titleEl = document.getElementById('report-modal-title');
  const pillsEl = document.getElementById('report-modal-pills');
  const bodyEl = document.getElementById('report-modal-body');
  const replyBtn = document.getElementById('btn-report-reply-blocker');
  const exportBtn = document.getElementById('btn-report-export-md');
  if (!overlay || !card || !bodyEl) return;

  activeReportTitle = title || '轻舟报告';
  activeReportMarkdown = rawExportMarkdown || markdownBody || '';

  card.classList.toggle('is-blocker', Boolean(isBlocker));
  if (badgeEl) badgeEl.textContent = badge || '报告详情';
  if (titleEl) titleEl.textContent = activeReportTitle;
  if (pillsEl) {
    pillsEl.innerHTML = (pills || [])
      .map(
        (p) =>
          `<span class="report-metric-pill"><span>${escapeHtml(p.label)}</span><strong class="mono">${escapeHtml(p.value)}</strong></span>`
      )
      .join('');
    pillsEl.classList.toggle('hidden', !pills || pills.length === 0);
  }

  bodyEl.innerHTML = `<div class="md-content">${renderSafeMarkdown(markdownBody)}</div>`;
  bodyEl.scrollTop = 0;

  replyBtn?.classList.toggle('hidden', !isBlocker);
  exportBtn?.classList.toggle('hidden', Boolean(isBlocker));

  overlay.classList.remove('hidden');
  overlay.setAttribute('aria-hidden', 'false');
}

export function openCompletionReportModal(summary) {
  if (!summary) return;
  const pills = [
    { label: '耗时', value: formatDuration(summary.total_duration_seconds) },
    { label: '任务', value: `${summary.total_tasks || 0} 项` },
    { label: '返工', value: `${summary.total_rework_rounds || 0} 轮` },
  ];
  if ((summary.total_tokens || 0) > 0) {
    pills.push({ label: 'Token', value: formatTokenCount(summary.total_tokens) });
    pills.push({ label: '折算', value: formatUsd(summary.total_cost_usd) });
  }
  const fullMd = buildCompletionMarkdown(summary);
  const bodyMd = summary.architect_note?.trim() ? summary.architect_note : fullMd;
  showReportModal({
    isBlocker: false,
    badge: '项目验收通过',
    title: '项目验收报告',
    pills,
    markdownBody: bodyMd,
    rawExportMarkdown: fullMd,
  });
}

export function openBlockerReportModal(alert) {
  if (!alert) return;
  const pills = [];
  if (alert.task_id) pills.push({ label: '关联任务', value: alert.task_id });
  if (alert.reason_kind) pills.push({ label: '阻塞类型', value: alert.reason_kind });
  showReportModal({
    isBlocker: true,
    badge: '需要您拍板确认',
    title: alert.title || '需要人工决策',
    pills,
    markdownBody: alert.plain_explanation || '请查看群聊记录并在底部输入框直接回复您的决定。',
    rawExportMarkdown: `# ${alert.title || '阻塞说明'}\n\n${alert.plain_explanation || ''}`,
  });
}

export function closeReportModal() {
  const overlay = document.getElementById('report-modal-overlay');
  if (!overlay) return;
  overlay.classList.add('hidden');
  overlay.setAttribute('aria-hidden', 'true');
}

async function copyTextSafe(text) {
  if (navigator.clipboard && typeof navigator.clipboard.writeText === 'function') {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch (_err) {
      // Fallback below
    }
  }
  const ta = document.createElement('textarea');
  ta.value = text;
  ta.style.position = 'fixed';
  ta.style.opacity = '0';
  document.body.appendChild(ta);
  ta.select();
  let ok = false;
  try {
    ok = document.execCommand('copy');
  } catch (_err) {
    ok = false;
  }
  ta.remove();
  return ok;
}

export function bindReportModalEvents() {
  const overlay = document.getElementById('report-modal-overlay');
  document.getElementById('btn-close-report-modal')?.addEventListener('click', closeReportModal);
  document.getElementById('btn-report-dismiss')?.addEventListener('click', closeReportModal);
  overlay?.addEventListener('click', (e) => {
    if (e.target === overlay) closeReportModal();
  });

  document.getElementById('btn-report-copy')?.addEventListener('click', async () => {
    const ok = await copyTextSafe(activeReportMarkdown);
    showToast(ok ? 'success' : 'warning', ok ? '已复制完整报告全文到剪贴板' : '复制失败，请手动选中复制');
  });

  document.getElementById('btn-report-export-md')?.addEventListener('click', () => {
    postIpc({
      cmd: 'export_report_markdown',
      title: activeReportTitle || '项目验收报告',
      markdown: activeReportMarkdown,
    });
  });

  document.getElementById('btn-report-reply-blocker')?.addEventListener('click', () => {
    closeReportModal();
    const input = document.getElementById('input-user-message');
    if (input) {
      input.focus();
      input.scrollIntoView({ behavior: 'smooth', block: 'nearest' });
    }
  });

  document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape' && overlay && !overlay.classList.contains('hidden')) {
      closeReportModal();
    }
  });
}
