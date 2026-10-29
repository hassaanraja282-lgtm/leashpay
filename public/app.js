// LeashPay dashboard: agent chat + owner console, polling the contract through the demo server.
const $ = (s) => document.querySelector(s);
const YOCTO = 10n ** 24n;
let state = null;
let lastTopId = -1;

function near(y, dp = 3) {
  if (y === null || y === undefined) return '–';
  const v = BigInt(y);
  const w = v / YOCTO;
  const f = (v % YOCTO).toString().padStart(24, '0').slice(0, dp).replace(/0+$/, '');
  return f ? `${w}.${f}` : `${w}`;
}
const human = (t) => String(t ?? '').replace(/(\d{19,})( yoctoNEAR)?/g, (_, y) => near(y) + ' NEAR');
const esc = (s) => String(s ?? '').replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
const short = (a) => { a = String(a || ''); const i = a.indexOf('.'); return i > 0 && a.length > 26 ? a.slice(0, i) + '.…' + a.slice(a.lastIndexOf('.')) : a; };
const ago = (ns) => {
  const s = Math.max(0, (Date.now() - Number(BigInt(ns) / 1_000_000n)) / 1000);
  if (s < 60) return `${Math.floor(s)}s ago`; if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`; return `${Math.floor(s / 86400)}d ago`;
};
const labelFor = (acc) => (state?.recipients || []).find(([a]) => a === acc)?.[1];

function toast(t) { const el = $('#toast'); el.textContent = t; el.classList.add('show'); clearTimeout(el._t); el._t = setTimeout(() => el.classList.remove('show'), 2600); }

async function api(path, body) {
  const r = await fetch(path, body ? { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) } : {});
  const j = await r.json().catch(() => ({}));
  if (!r.ok) throw Object.assign(new Error(j.error || r.statusText), j);
  return j;
}

async function refresh() {
  try { state = await api('/api/state'); render(); } catch (e) { console.warn(e); }
}

function render() {
  const s = state, m = s.meta, c = s.config;
  $('#net').textContent = m.network;
  const cl = $('#contractLink'); cl.textContent = m.contract; cl.href = `${m.explorer}/address/${m.contract}`;
  $('#planner').textContent = `planner: ${m.planner}`;
  $('#agentId').textContent = c.agent;
  $('#ownerId').textContent = c.owner;

  $('#avail').textContent = near(s.balance.available, 2);
  $('#availHint').textContent = `${near(s.balance.total, 2)} NEAR total · ${near(s.balance.storage_locked, 2)} NEAR storage`;
  const cap = BigInt(s.allowance.daily_cap), rem = BigInt(s.allowance.remaining_today);
  $('#left').textContent = near(rem);
  $('#leftOf').textContent = `/ ${near(cap)} NEAR`;
  $('#leftBar').style.width = `${cap ? Number((rem * 1000n) / cap) / 10 : 0}%`;
  const reset = new Date(Number(BigInt(s.allowance.resets_at) / 1_000_000n));
  $('#resets').textContent = `resets ${reset.toUTCString().slice(17, 22)} UTC`;

  const st = $('#status'), card = $('#statusCard'), kill = $('#kill');
  st.textContent = c.paused ? 'Paused' : 'Active';
  st.className = 'big status ' + (c.paused ? 'paused' : 'active');
  card.classList.toggle('paused', c.paused);
  kill.textContent = c.paused ? 'Resume agent' : 'Pause agent';
  kill.className = 'btn ' + (c.paused ? 'ok' : 'danger');
  kill.disabled = !m.capabilities.owner;

  $('#pTx').textContent = `${near(c.policy.per_tx_cap)} NEAR`;
  $('#pDay').textContent = `${near(c.policy.daily_cap)} NEAR`;
  $('#pTh').textContent = c.policy.approval_threshold ? `${near(c.policy.approval_threshold)} NEAR` : 'never';
  $('#editPolicy').classList.toggle('hidden', !m.capabilities.owner);

  $('#allow').innerHTML = s.recipients.map(([a, l]) => `<li><b>${esc(l)}</b><a class="mono" href="${m.explorer}/address/${a}" target="_blank" rel="noopener">${esc(short(a))}</a></li>`).join('') || '<li>None yet</li>';

  $('#pendCount').textContent = s.pending.length;
  $('#pending').innerHTML = s.pending.length ? s.pending.map((p) => `
    <li class="pend">
      <div class="row"><span class="amt">${near(p.amount)} NEAR</span><span class="sub">#${p.id} · ${ago(p.requested_at)}</span></div>
      <div class="row"><span>${esc(labelFor(p.recipient) || '')}</span><span class="mono">${esc(short(p.recipient))}</span></div>
      <div class="memo">“${esc(p.memo)}”</div>
      <div class="acts">
        <button class="btn ok sm" data-approve="${p.id}" ${m.capabilities.owner ? '' : 'disabled'}>Approve</button>
        <button class="btn danger sm" data-reject="${p.id}" ${m.capabilities.owner ? '' : 'disabled'}>Reject</button>
      </div>
    </li>`).join('') : '<li class="empty">Nothing waiting.</li>';

  const top = s.log[0]?.id ?? -1;
  $('#log').innerHTML = s.log.map((e) => {
    const det = human(e.reason || e.memo || '');
    const link = e.txHash ? `<a class="ext" href="${m.explorer}/txns/${e.txHash}" target="_blank" rel="noopener">tx ↗</a>`
      : `<a class="ext" href="${m.explorer}/blocks/${e.block_height}" target="_blank" rel="noopener">block ↗</a>`;
    return `<tr class="${lastTopId >= 0 && e.id > lastTopId ? 'flash' : ''}">
      <td class="mono">${e.id}</td><td><span class="badge ${e.kind}">${e.kind.replace('_', ' ')}</span></td>
      <td class="mono">${esc(e.recipient ? (labelFor(e.recipient) ? labelFor(e.recipient) : short(e.recipient)) : '')}</td>
      <td class="r">${e.amount ? near(e.amount) + ' NEAR' : ''}</td>
      <td class="det">${esc(det)}</td><td class="sub">${ago(e.timestamp)}</td><td>${link}</td></tr>`;
  }).join('');
  lastTopId = top;
}

// ---- agent chat
function addMsg(cls, html) {
  const d = document.createElement('div'); d.className = `msg ${cls}`; d.innerHTML = html;
  $('#chat').appendChild(d); $('#chat').scrollTop = $('#chat').scrollHeight; return d;
}

async function runTask(text) {
  if (!text.trim()) return;
  addMsg('user', esc(text));
  const pending = addMsg('agent', '<span class="typing">Planning and signing request_payment…</span>');
  $('#send').disabled = true;
  try {
    const r = await api('/api/agent/task', { text });
    if (r.plan.action !== 'pay') { pending.innerHTML = esc(r.plan.reply || 'Nothing to pay.'); return; }
    const res = r.result;
    const who = labelFor(r.plan.recipient) ? `${labelFor(r.plan.recipient)} (${short(r.plan.recipient)})` : short(r.plan.recipient);
    const words = { paid: 'PAID', queued: 'QUEUED FOR APPROVAL', blocked: 'BLOCKED' };
    const icon = { paid: '✓', queued: '◔', blocked: '✕' };
    pending.innerHTML = `Requesting <b>${esc(r.plan.amount_near)} NEAR</b> → <b>${esc(who)}</b>
      <div class="plan">${esc(r.plan.planner)} · ${esc(r.plan.reasoning || '')}</div>
      <div class="verdict ${res.status}"><span>${icon[res.status]}</span><div><b>${words[res.status]}</b> by the vault contract${res.reason ? ': ' + esc(human(res.reason)) : ''}.
      <br/><a href="${r.explorerUrl}" target="_blank" rel="noopener">view transaction ↗</a> · ${near(res.remaining_today)} NEAR left today</div></div>`;
    $('#chat').scrollTop = $('#chat').scrollHeight;
    refresh();
  } catch (e) {
    pending.innerHTML = `<div class="verdict blocked"><span>!</span><div>${esc(e.message)}</div></div>`;
  } finally { $('#send').disabled = false; }
}

$('#ask').addEventListener('submit', (ev) => { ev.preventDefault(); const t = $('#task').value; $('#task').value = ''; runTask(t); });
$('#chips').addEventListener('click', (ev) => { const b = ev.target.closest('button'); if (b) runTask(b.dataset.t); });

document.querySelectorAll('[data-attack]').forEach((b) => b.addEventListener('click', async () => {
  const kind = b.dataset.attack; b.disabled = true;
  const out = $('#attackOut');
  const row = document.createElement('div'); row.className = 'a'; row.textContent = `Trying ${b.textContent.trim()} with the agent key…`;
  out.prepend(row);
  try {
    const r = await api('/api/agent/attack', { kind });
    row.innerHTML = `${r.blocked ? '✕ Rejected by NEAR' : '⚠ Not rejected'} — <code>${esc(r.message)}</code>`;
  } catch (e) { row.innerHTML = `✕ Rejected — <code>${esc(e.message)}</code>`; }
  while (out.children.length > 3) out.lastChild.remove();
  b.disabled = false;
}));

// ---- owner actions
async function ownerAction(path, body, okMsg) {
  try { const r = await api(path, body); toast(okMsg); await refresh(); return r; } catch (e) { toast(e.message); }
}
$('#pending').addEventListener('click', async (ev) => {
  const a = ev.target.closest('[data-approve]'), r = ev.target.closest('[data-reject]');
  if (a) { a.disabled = true; a.textContent = 'Approving…'; await ownerAction('/api/owner/approve', { id: a.dataset.approve }, 'Approved and paid on-chain'); }
  if (r) { r.disabled = true; r.textContent = 'Rejecting…'; await ownerAction('/api/owner/reject', { id: r.dataset.reject }, 'Rejected on-chain'); }
});
$('#kill').addEventListener('click', async () => {
  const paused = state?.config?.paused; $('#kill').disabled = true; $('#kill').textContent = paused ? 'Resuming…' : 'Pausing…';
  await ownerAction(paused ? '/api/owner/unpause' : '/api/owner/pause', {}, paused ? 'Agent resumed' : 'Kill switch on: agent paused');
});
$('#editPolicy').addEventListener('click', () => {
  const f = $('#policyForm'); f.classList.toggle('hidden');
  const p = state.config.policy;
  f.perTx.value = near(p.per_tx_cap); f.daily.value = near(p.daily_cap); f.threshold.value = p.approval_threshold ? near(p.approval_threshold) : '';
});
$('#policyForm').addEventListener('submit', async (ev) => {
  ev.preventDefault(); const f = ev.target;
  await ownerAction('/api/owner/policy', { perTx: f.perTx.value, daily: f.daily.value, threshold: f.threshold.value || null }, 'Policy updated on-chain');
  f.classList.add('hidden');
});

refresh();
setInterval(refresh, 6000);
window.leash = { runTask, refresh };
