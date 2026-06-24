/**
 * main.js — Bartleby frontend logic
 *
 * ── Architecture overview ────────────────────────────────────────────────────
 *
 *   JS → Rust : invoke("command_name", args)
 *   Rust → JS : listen("event-name", handler)
 *
 * ── Data flow during a copy ──────────────────────────────────────────────────
 *
 *   1. User clicks "Copy"
 *   2. launchCopy() validates inputs, registers Tauri event listeners
 *   3. invoke("start_copy", args) — Rust starts the background copy thread
 *   4. Rust emits "copy-progress" → progress bar updates
 *   5. Rust emits "copy-log" per file → log panel grows
 *   6. If a prompt is needed, Rust emits "copy-prompt" → showPrompt() modal
 *   7. User responds → invoke("prompt_reply") → Rust unblocks
 *   8. Rust emits "copy-done" → status label shown, button re-enabled
 *
 * ── Why var and not let/const ────────────────────────────────────────────────
 *
 * `var` is used throughout for compatibility with older WebKit versions that
 * Tauri might target on some Linux systems.
 */

// ── Tauri IPC helpers ─────────────────────────────────────────────────────────
function invoke(cmd, args) {
    return window.__TAURI__.core.invoke(cmd, args);
}
function listen(event, handler) {
    return window.__TAURI__.event.listen(event, handler);
}

async function pickFolder() {
    try {
        if (!window.__TAURI__ || !window.__TAURI__.dialog) {
            alert('Folder picker unavailable: the Tauri dialog API did not load.');
            return null;
        }
        var result = await window.__TAURI__.dialog.open({
            directory: true,
            multiple:  false
        });
        return result;
    } catch(e) {
        alert('Folder picker error: ' + e);
        return null;
    }
}

// ── Path display helpers ──────────────────────────────────────────────────────
var homeDir = '';

function shortenPath(p) {
    if (homeDir && p.startsWith(homeDir)) {
        return '~' + p.slice(homeDir.length);
    }
    return p;
}

function expandPath(p) {
    if (homeDir && p.startsWith('~')) {
        return homeDir + p.slice(1);
    }
    return p;
}

// ── Volume info helpers ───────────────────────────────────────────────────────
// Uses SI decimal prefixes and French unit names (Go = giga-octet, To = téra-octet)
// — standard in French-speaking broadcast/film production environments.
function formatBytes(b) {
    if (b <= 0) return '0 B';
    if (b < 1e6)  return (b / 1e3).toFixed(0) + ' KB';
    if (b < 1e9)  return (b / 1e6).toFixed(1) + ' MB';
    if (b < 1e12) return (b / 1e9).toFixed(2) + ' GB';
    return (b / 1e12).toFixed(2) + ' TB';
}

function formatEta(secs) {
    secs = Math.round(secs);
    if (secs < 5)  return '< 5s';
    if (secs < 60) return secs + 's';
    var m = Math.floor(secs / 60);
    var s = secs % 60;
    if (m < 60)    return s ? m + 'm ' + s + 's' : m + 'm';
    var h = Math.floor(m / 60);
    m = m % 60;
    return m ? h + 'h ' + m + 'm' : h + 'h';
}

function escHtml(s) {
    return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;');
}

function createVolInfoEl() {
    var div = document.createElement('div');
    div.className = 'vol-info hidden';
    return div;
}

async function updateVolInfo(el, displayPath) {
    if (!el || !displayPath || !displayPath.trim()) { el && el.classList.add('hidden'); return; }
    var info;
    try { info = await invoke('get_volume_info', { path: expandPath(displayPath.trim()) }); }
    catch(e) { el.classList.add('hidden'); return; }
    if (!info.ok) { el.classList.add('hidden'); return; }

    var used    = info.total_bytes - info.free_bytes;
    var usedPct = info.total_bytes > 0 ? used / info.total_bytes : 0;
    var fillPct = Math.round(usedPct * 100);
    var barCls  = usedPct < 0.6 ? 'vol-bar-green' : (usedPct < 0.8 ? 'vol-bar-orange' : 'vol-bar-red');
    var typeName = [info.media_type, info.label].filter(Boolean).join(' – ') || 'Volume';

    el.innerHTML =
        '<div class="vol-info-row">' +
            '<span class="vol-type-name">' + escHtml(typeName) + '</span>' +
            '<span class="vol-stat">Size ' + formatBytes(info.total_bytes) + '</span>' +
            '<span class="vol-stat">Free ' + formatBytes(info.free_bytes) + '</span>' +
        '</div>' +
        '<div class="vol-bar-track">' +
            '<div class="vol-bar-fill ' + barCls + '" style="width:' + fillPct + '%"></div>' +
        '</div>';
    el.classList.remove('hidden');
}

function makeVolInfoWatcher(inputEl, volInfoEl) {
    var timer = null;
    inputEl.addEventListener('input', function() {
        clearTimeout(timer);
        timer = setTimeout(function() { updateVolInfo(volInfoEl, inputEl.value); }, 450);
    });
}

async function updateSrcInfo(el, displayPath) {
    if (!el || !displayPath || !displayPath.trim()) { el && el.classList.add('hidden'); return; }
    el.innerHTML = '<div class="vol-info-row"><span class="vol-type-name">…</span></div>';
    el.classList.remove('hidden');
    var expandedPath = expandPath(displayPath.trim());
    var info, size;
    try {
        var results = await Promise.all([
            invoke('get_volume_info', { path: expandedPath }),
            invoke('get_source_size', { path: expandedPath })
        ]);
        info = results[0]; size = results[1];
    } catch(e) { el.classList.add('hidden'); return; }
    if (!info.ok && size === 0) { el.classList.add('hidden'); return; }
    var typeName = [info.media_type, info.label].filter(Boolean).join(' – ') || '';
    el.innerHTML =
        '<div class="vol-info-row">' +
            (typeName ? '<span class="vol-type-name">' + escHtml(typeName) + '</span>' : '') +
            '<span class="vol-stat">' + formatBytes(size) + '</span>' +
        '</div>';
    el.classList.remove('hidden');
}

function makeSrcInfoWatcher(inputEl, srcInfoEl) {
    var timer = null;
    inputEl.addEventListener('input', function() {
        clearTimeout(timer);
        timer = setTimeout(function() { updateSrcInfo(srcInfoEl, inputEl.value); }, 450);
    });
}

// ── DOM references ────────────────────────────────────────────────────────────
var jobsContainer = document.getElementById('jobs-container');
var addJobBtn     = document.getElementById('add-job-btn');
var statusLabel   = document.getElementById('status-label');
var logView       = document.getElementById('log-view');
var copyBtn       = document.getElementById('copy-btn');
var pauseBtn      = document.getElementById('pause-btn');
var cancelBtn     = document.getElementById('cancel-btn');
var copyIsPaused  = false;
var userCancelledQueue = false;
var currentJobIndex    = -1;
var copyJobStartTime   = null;
var menuBtn            = document.getElementById('menu-btn');
var settingsOverlay    = document.getElementById('settings-overlay');
var verifyBtn          = document.getElementById('verify-btn');
if (verifyBtn) {
    verifyBtn.addEventListener('click', function() {
        invoke('open_verifier_window').catch(function(e) { console.error('open_verifier_window:', e); });
    });
}

// ── Job creation defaults ─────────────────────────────────────────────────────
// Loaded from settings on startup. Used to initialise the toggles on new job cards.
// Per-job toggle values are ephemeral (session only); these defaults persist.
var defaultHashAlgo = 'md5';
var defaultGenCsv   = false;
var defaultGenPdf   = false;
var defaultGenHtml  = false;
var defaultGenMhl   = false;

// ── Transport control helpers ─────────────────────────────────────────────────
// True while a copy/queue is running — used to warn on cancel and on app close.
var copyInProgress = false;
function setCopyInProgress(active) {
    copyInProgress = active;
    copyIsPaused = false;
    copyBtn.classList.toggle('hidden', active);
    pauseBtn.classList.toggle('hidden', !active);
    cancelBtn.classList.toggle('hidden', !active);
    pauseBtn.disabled  = false;
    cancelBtn.disabled = false;
    pauseBtn.innerHTML = '<svg width="18" height="18"><use href="#ico-pause"/></svg>';
    pauseBtn.title     = 'Pause transfer';
}

// ── Settings ──────────────────────────────────────────────────────────────────
var currentSettings = null;

async function loadSettings() {
    try {
        currentSettings = await invoke('get_settings');

        // Derive hash algorithm — handle old settings that stored gen_md5/gen_xxh booleans.
        var algo = currentSettings.hash_algo;
        if (!algo) algo = currentSettings.gen_xxh ? 'xxh128' : (currentSettings.gen_md5 ? 'md5' : 'none');
        if (algo === 'xxh3') algo = 'xxh128'; // migrate renamed algo
        defaultHashAlgo = algo;
        defaultGenCsv   = currentSettings.gen_csv  || false;
        defaultGenPdf   = currentSettings.gen_pdf  || false;
        defaultGenHtml  = currentSettings.gen_html || false;
        defaultGenMhl   = currentSettings.gen_mhl  || false;

        // Restore open-destinations toggle (now lives in Report tweaks tab).
        var chkOpenEl = document.getElementById('chk-open');
        if (chkOpenEl) chkOpenEl.checked = currentSettings.open_dest || false;

        // Apply saved skin.
        applySkin(currentSettings.skin || 'mint-y-aqua', false);

        // Apply saved theme.
        var saved = currentSettings.theme || 'default';
        if (saved === 'default') {
            try {
                var isDark = await invoke('is_system_dark_mode');
                document.body.className = isDark ? 'theme-dark' : 'theme-light';
                invoke('set_window_theme', { theme: 'default' }).catch(function() {});
                document.querySelectorAll('.appearance-btn[data-theme]').forEach(function(btn) {
                    btn.classList.toggle('appearance-btn-active', btn.dataset.theme === 'default');
                });
            } catch(e) {
                document.body.className = 'theme-default';
            }
        } else {
            applyTheme(saved, false);
        }

        var logoEl = document.getElementById('s-logo');
        if (logoEl) logoEl.value = currentSettings.logo_path || '';

    } catch(e) {
        console.error('get_settings:', e);
    }
}

async function persistSettings() {
    try {
        await invoke('save_settings', { newSettings: currentSettings });
    } catch(e) {
        console.error('save_settings:', e);
    }
}

// ── Theme / Skin ──────────────────────────────────────────────────────────────

function applyTheme(theme, save) {
    if (theme === 'default') {
        // On Linux, CSS prefers-color-scheme is unreliable in WebKitGTK;
        // query Rust (gsettings / GTK_THEME) for the actual system preference.
        invoke('is_system_dark_mode').then(function(isDark) {
            document.body.className = isDark ? 'theme-dark' : 'theme-light';
            invoke('set_window_theme', { theme: 'default' }).catch(function() {});
        }).catch(function() {
            document.body.className = 'theme-default'; // CSS @media fallback
        });
    } else {
        document.body.className = 'theme-' + theme;
        invoke('set_window_theme', { theme: theme === 'dark' ? 'dark' : 'light' }).catch(function() {});
    }
    if (currentSettings) currentSettings.theme = theme;
    document.querySelectorAll('.appearance-btn[data-theme]').forEach(function(btn) {
        btn.classList.toggle('appearance-btn-active', btn.dataset.theme === theme);
    });
    if (save) persistSettings();
}

function applySkin(skin, save) {
    var link = document.getElementById('theme-link');
    if (link) link.href = 'themes/' + skin + '.css';
    if (currentSettings) currentSettings.skin = skin;
    document.querySelectorAll('.appearance-btn[data-skin]').forEach(function(btn) {
        btn.classList.toggle('appearance-btn-active', btn.dataset.skin === skin);
    });
    if (save) persistSettings();
}

// ── Settings modal ────────────────────────────────────────────────────────────

// Hamburger → open settings (Appearance tab active by default).
menuBtn.addEventListener('click', function() {
    if (currentSettings) { populateReportFields(); populateStructureFields(); }
    settingsOverlay.classList.remove('hidden');
});

// Close on backdrop click.
settingsOverlay.addEventListener('click', function(e) {
    if (e.target === settingsOverlay) settingsOverlay.classList.add('hidden');
});

// Tab switching.
document.querySelectorAll('.settings-tab-btn').forEach(function(btn) {
    btn.addEventListener('click', function() {
        document.querySelectorAll('.settings-tab-btn').forEach(function(b) { b.classList.remove('active'); });
        btn.classList.add('active');
        document.querySelectorAll('.settings-panel').forEach(function(p) { p.classList.add('hidden'); });
        document.getElementById('stab-' + btn.dataset.tab).classList.remove('hidden');
    });
});

// Appearance tab — theme buttons.
document.querySelectorAll('.appearance-btn[data-theme]').forEach(function(btn) {
    btn.addEventListener('click', function() { applyTheme(btn.dataset.theme, true); });
});

// Appearance tab — skin buttons.
document.querySelectorAll('.appearance-btn[data-skin]').forEach(function(btn) {
    btn.addEventListener('click', function() { applySkin(btn.dataset.skin, true); });
});

// Close buttons (Appearance + About tabs have a "Close" button).
document.querySelectorAll('.settings-close-btn').forEach(function(btn) {
    btn.addEventListener('click', function() { settingsOverlay.classList.add('hidden'); });
});

// Report tweaks tab — populate fields from currentSettings.
function populateReportFields() {
    document.getElementById('s-company').value = currentSettings.company       || '';
    document.getElementById('s-contact').value = currentSettings.contact_name  || '';
    document.getElementById('s-email').value   = currentSettings.email         || '';
    document.getElementById('s-phone').value   = currentSettings.phone         || '';
    document.getElementById('s-project').value = currentSettings.project_title || '';
    var logoEl = document.getElementById('s-logo');
    if (logoEl) logoEl.value = currentSettings.logo_path || '';
    var a1 = document.getElementById('s-accent1');
    if (a1) a1.value = currentSettings.accent_color_1 || '#1F9EDE';
    document.querySelectorAll('[data-col]').forEach(function(cb) {
        cb.checked = !!currentSettings[cb.dataset.col];
    });
    var chkOpenEl = document.getElementById('chk-open');
    if (chkOpenEl) chkOpenEl.checked = currentSettings.open_dest || false;
}

// Logo picker — Browse.
var logoBtn = document.getElementById('s-logo-btn');
if (logoBtn) {
    logoBtn.addEventListener('click', async function() {
        try {
            var path = await window.__TAURI__.dialog.open({
                filters: [{ name: 'Image', extensions: ['png', 'jpg', 'jpeg'] }],
                multiple:  false,
                directory: false,
            });
            if (path) {
                currentSettings.logo_path = path;
                var logoEl = document.getElementById('s-logo');
                if (logoEl) logoEl.value = path;
                await invoke('save_settings', { newSettings: currentSettings });
            }
        } catch(e) {
            console.warn('Logo picker failed:', e);
        }
    });
}

// Logo picker — Clear.
var logoClear = document.getElementById('s-logo-clear');
if (logoClear) {
    logoClear.addEventListener('click', async function() {
        if (currentSettings) currentSettings.logo_path = '';
        var logoEl = document.getElementById('s-logo');
        if (logoEl) logoEl.value = '';
        await invoke('save_settings', { newSettings: currentSettings });
    });
}

// Accent colour picker — live save.
var accentEl = document.getElementById('s-accent1');
if (accentEl) {
    accentEl.addEventListener('change', async function() {
        if (!currentSettings) return;
        currentSettings.accent_color_1 = accentEl.value.toUpperCase();
        await persistSettings();
    });
}

// Report tweaks — Cancel.
document.getElementById('settings-cancel').addEventListener('click', function() {
    settingsOverlay.classList.add('hidden');
});

// Report tweaks — Save.
document.getElementById('settings-save').addEventListener('click', async function() {
    if (!currentSettings) return;
    currentSettings.project_title = document.getElementById('s-project').value;
    currentSettings.company       = document.getElementById('s-company').value;
    currentSettings.contact_name  = document.getElementById('s-contact').value;
    currentSettings.email         = document.getElementById('s-email').value;
    currentSettings.phone         = document.getElementById('s-phone').value;
    var logoEl = document.getElementById('s-logo');
    if (logoEl) currentSettings.logo_path = logoEl.value;
    var a1 = document.getElementById('s-accent1');
    if (a1) currentSettings.accent_color_1 = a1.value.toUpperCase();
    document.querySelectorAll('[data-col]').forEach(function(cb) {
        currentSettings[cb.dataset.col] = cb.checked;
    });
    var chkOpenEl = document.getElementById('chk-open');
    if (chkOpenEl) currentSettings.open_dest = chkOpenEl.checked;
    await persistSettings();
    settingsOverlay.classList.add('hidden');
});

// ── Job queue management ──────────────────────────────────────────────────────
var currentJobPrefix = '';

function addDestRowToJob(destListEl, initialValue) {
    var item = document.createElement('div');
    item.className = 'dest-item';

    var row = document.createElement('div');
    row.className = 'dest-row';

    var input = document.createElement('input');
    input.type = 'text';
    input.placeholder = 'Destination… (or drag & drop)';

    var browseBtn = document.createElement('button');
    browseBtn.className = 'icon-btn';
    browseBtn.title = 'Browse…';
    browseBtn.innerHTML = '<svg width="18" height="18"><use href="#ico-folder"/></svg>';

    var removeBtn = document.createElement('button');
    removeBtn.className = 'icon-btn icon-btn-danger';
    removeBtn.title = 'Remove';
    removeBtn.innerHTML = '<svg width="16" height="16"><use href="#ico-close"/></svg>';

    var volInfoEl = createVolInfoEl();

    browseBtn.addEventListener('click', async function() {
        var p = await pickFolder();
        if (p) { input.value = shortenPath(p); updateVolInfo(volInfoEl, input.value); }
    });
    removeBtn.addEventListener('click', function() { item.remove(); });
    makeVolInfoWatcher(input, volInfoEl);

    row.appendChild(input);
    row.appendChild(browseBtn);
    row.appendChild(removeBtn);
    item.appendChild(row);
    item.appendChild(volInfoEl);
    destListEl.appendChild(item);

    if (initialValue) { input.value = initialValue; updateVolInfo(volInfoEl, input.value); }
}

// Helper: create a toggle switch element for the per-job options row.
function makeJobToggle(cls, labelText, checked) {
    var lbl = document.createElement('label');
    lbl.className = 'toggle-label';
    var chk = document.createElement('input');
    chk.type = 'checkbox';
    chk.className = cls;
    chk.checked = !!checked;
    var track = document.createElement('span');
    track.className = 'toggle-track';
    var thumb = document.createElement('span');
    thumb.className = 'toggle-thumb';
    track.appendChild(thumb);
    lbl.appendChild(chk);
    lbl.appendChild(track);
    lbl.appendChild(document.createTextNode(labelText));
    return lbl;
}

// ── Job status (DaVinci-style queue state) ─────────────────────────────────────
// Persisted on the card via dataset.status ∈ {idle, running, done, failed}.
// Survives across copy launches because the cards themselves persist in the DOM.
function setJobStatus(card, status) {
    if (!card) return;
    card.dataset.status = status;
    // Tint the job title green/red once the job finishes.
    var nameInput = card.querySelector('.job-name-input');
    if (nameInput) {
        nameInput.classList.toggle('job-done',   status === 'done');
        nameInput.classList.toggle('job-failed', status === 'failed');
    }
    var badge = card.querySelector('.job-status-badge');
    if (!badge) return;
    badge.dataset.status = status;
    // Idle/"to do": show nothing — keeps the header clean until the job runs.
    if (status === 'idle') {
        badge.style.display = 'none';
        badge.innerHTML = '';
        return;
    }
    badge.style.display = '';
    var map = {
        running: { icon: '',           text: 'Running' },
        done:    { icon: '#ico-check', text: 'Done'    },
        failed:  { icon: '#ico-close', text: 'Failed'  },
    };
    var m = map[status] || map.running;
    badge.innerHTML = (m.icon ? '<svg><use href="' + m.icon + '"/></svg>' : '') + m.text;
}

function addJob() {
    var jobCard = document.createElement('section');
    jobCard.className = 'group job-group';

    // ── Job header ────────────────────────────────────────────────────────────
    var headerRow = document.createElement('div');
    headerRow.className = 'job-header-row';

    var label = document.createElement('input');
    label.type = 'text';
    label.className = 'job-label job-name-input';
    label.placeholder = 'Job';
    label.title = 'Job name (optional)';
    label.spellcheck = false;

    var removeJobBtn = document.createElement('button');
    removeJobBtn.className = 'icon-btn icon-btn-danger job-remove-btn';
    removeJobBtn.title = 'Remove this job';
    removeJobBtn.innerHTML = '<svg width="14" height="14"><use href="#ico-close"/></svg>';
    removeJobBtn.style.display = 'none';
    removeJobBtn.addEventListener('click', function(e) {
        e.stopPropagation();
        var wasSelected = jobCard.classList.contains('job-selected');
        jobCard.remove();
        renumberJobs();
        if (wasSelected) {
            setSelectedJob(jobsContainer.querySelector('.job-group') || null);
        }
        refreshPreviewSoon();
    });

    var statusBadge = document.createElement('span');
    statusBadge.className = 'job-status-badge';
    statusBadge.title = 'Job status — right-click the job to reset it';

    headerRow.appendChild(label);
    headerRow.appendChild(statusBadge);
    headerRow.appendChild(removeJobBtn);
    jobCard.appendChild(headerRow);

    setJobStatus(jobCard, 'idle');

    // Right-click anywhere on the card (except text fields) → status menu.
    jobCard.addEventListener('contextmenu', function(e) {
        if (e.target.closest('input, textarea, select')) return;
        e.preventDefault();
        showJobCtxMenu(e.clientX, e.clientY, jobCard);
    });

    // ── Card body ─────────────────────────────────────────────────────────────
    var card = document.createElement('div');
    card.className = 'card';

    var jobBody = document.createElement('div');
    jobBody.className = 'job-body';

    // ── Source section ────────────────────────────────────────────────────────
    var srcSection = document.createElement('div');
    srcSection.className = 'job-src-section';

    var srcLabel = document.createElement('div');
    srcLabel.className = 'job-subsection-label';
    srcLabel.textContent = 'Source';
    srcSection.appendChild(srcLabel);

    var srcRow = document.createElement('div');
    srcRow.className = 'row';

    var srcInputEl = document.createElement('input');
    srcInputEl.type = 'text';
    srcInputEl.className = 'job-src-input';
    srcInputEl.placeholder = 'Source… (or drag & drop)';

    var srcBrowseBtn = document.createElement('button');
    srcBrowseBtn.className = 'icon-btn';
    srcBrowseBtn.title = 'Browse…';
    srcBrowseBtn.innerHTML = '<svg width="18" height="18"><use href="#ico-folder"/></svg>';
    var srcVolInfo = createVolInfoEl();
    makeSrcInfoWatcher(srcInputEl, srcVolInfo);

    srcBrowseBtn.addEventListener('click', async function() {
        var p = await pickFolder();
        if (p) { srcInputEl.value = shortenPath(p); updateSrcInfo(srcVolInfo, srcInputEl.value); }
    });

    srcRow.appendChild(srcInputEl);
    srcRow.appendChild(srcBrowseBtn);
    srcSection.appendChild(srcRow);
    srcSection.appendChild(srcVolInfo);

    // Copy-as-subfolder toggle
    var subfolderRow = document.createElement('div');
    subfolderRow.className = 'row subfolder-row';
    var subfolderLbl = document.createElement('label');
    subfolderLbl.className = 'toggle-label subfolder-label';
    var subfolderChk = document.createElement('input');
    subfolderChk.type = 'checkbox';
    subfolderChk.className = 'job-subfolder-chk';
    var toggleTrack = document.createElement('span');
    toggleTrack.className = 'toggle-track';
    var toggleThumb = document.createElement('span');
    toggleThumb.className = 'toggle-thumb';
    toggleTrack.appendChild(toggleThumb);
    var subfolderTxt = document.createTextNode('Copy folder itself into destination');
    subfolderLbl.appendChild(subfolderChk);
    subfolderLbl.appendChild(toggleTrack);
    subfolderLbl.appendChild(subfolderTxt);
    subfolderRow.appendChild(subfolderLbl);
    srcSection.appendChild(subfolderRow);

    jobBody.appendChild(srcSection);

    // ── Vertical separator ────────────────────────────────────────────────────
    var vsep = document.createElement('div');
    vsep.className = 'job-vsep';
    vsep.innerHTML = '<div class="job-vsep-line"></div>';
    jobBody.appendChild(vsep);

    // ── Destinations section ──────────────────────────────────────────────────
    var destSection = document.createElement('div');
    destSection.className = 'job-dest-section';

    var destSectionLabel = document.createElement('div');
    destSectionLabel.className = 'job-subsection-label';
    destSectionLabel.textContent = 'Destinations';
    destSection.appendChild(destSectionLabel);

    var destListEl = document.createElement('div');
    destListEl.className = 'job-dest-list';
    destSection.appendChild(destListEl);

    var addDestBtn = document.createElement('button');
    addDestBtn.className = 'flat-btn';
    addDestBtn.textContent = '+ Add a destination';
    addDestBtn.addEventListener('click', function() { addDestRowToJob(destListEl); });
    destSection.appendChild(addDestBtn);

    jobBody.appendChild(destSection);
    card.appendChild(jobBody);

    // ── Per-job options row: hash + CSV + PDF + HTML ──────────────────────────
    var jobOptsRow = document.createElement('div');
    jobOptsRow.className = 'job-options-row';

    var hashSel = document.createElement('select');
    hashSel.className = 'job-hash-select';
    hashSel.title = 'Hash algorithm — controls verification and checksum file output';
    [
        ['none',   'No hash'],
        ['size',   'Size only'],
        ['md5',    '.MD5'],
        ['sha1',   '.SHA1'],
        ['xxh64',  '.XXH64'],
        ['xxh3',   '.XXH3-64'],
        ['xxh128', '.XXH128'],
        ['c4',     'C4 ID'],
    ].forEach(function(pair) {
        var opt = document.createElement('option');
        opt.value       = pair[0];
        opt.textContent = pair[1];
        hashSel.appendChild(opt);
    });
    hashSel.value = defaultHashAlgo;
    jobOptsRow.appendChild(hashSel);

    var csvLbl  = makeJobToggle('job-chk-csv',  '.CSV',  defaultGenCsv);
    var pdfLbl  = makeJobToggle('job-chk-pdf',  '.PDF',  defaultGenPdf);
    var htmlLbl = makeJobToggle('job-chk-html', '.HTML', defaultGenHtml);
    jobOptsRow.appendChild(csvLbl);
    jobOptsRow.appendChild(pdfLbl);
    jobOptsRow.appendChild(htmlLbl);

    csvLbl.querySelector('.job-chk-csv').addEventListener('change', function() {
        defaultGenCsv = this.checked;
        currentSettings.gen_csv = this.checked;
        invoke('save_settings', { newSettings: currentSettings }).catch(function(){});
    });
    pdfLbl.querySelector('.job-chk-pdf').addEventListener('change', function() {
        defaultGenPdf = this.checked;
        currentSettings.gen_pdf = this.checked;
        invoke('save_settings', { newSettings: currentSettings }).catch(function(){});
    });
    htmlLbl.querySelector('.job-chk-html').addEventListener('change', function() {
        defaultGenHtml = this.checked;
        currentSettings.gen_html = this.checked;
        invoke('save_settings', { newSettings: currentSettings }).catch(function(){});
    });

    var mhlLbl = makeJobToggle('job-chk-mhl', '.MHL', defaultGenMhl);
    var mhlChkInit = mhlLbl.querySelector('.job-chk-mhl');
    if (hashSel.value === 'none' || hashSel.value === 'size') {
        mhlChkInit.disabled = true;
        mhlLbl.classList.add('toggle-disabled');
    }
    jobOptsRow.appendChild(mhlLbl);

    mhlChkInit.addEventListener('change', function() {
        defaultGenMhl = this.checked;
        currentSettings.gen_mhl = this.checked;
        invoke('save_settings', { newSettings: currentSettings }).catch(function(){});
    });

    hashSel.addEventListener('change', function() {
        var mhlChkEl = jobOptsRow.querySelector('.job-chk-mhl');
        var mhlLblEl = mhlChkEl ? mhlChkEl.closest('.toggle-label') : null;
        var disable  = hashSel.value === 'none' || hashSel.value === 'size';
        if (mhlChkEl) mhlChkEl.disabled = disable;
        if (mhlLblEl) mhlLblEl.classList.toggle('toggle-disabled', disable);
    });

    var commentBtn = document.createElement('button');
    commentBtn.className = 'job-comment-btn';
    commentBtn.title = 'Add a note (shown in reports)';
    commentBtn.textContent = 'T';
    commentBtn.addEventListener('click', function() { openCommentModal(jobCard); });

    var templateInput = document.createElement('input');
    templateInput.type = 'text';
    templateInput.className = 'job-template-input';
    templateInput.placeholder = '#preset or %var';
    templateInput.title = 'Folder structure: type # for presets, % for variables';
    templateInput.spellcheck = false;
    attachTemplateAutocomplete(templateInput, jobCard);

    var structureBtn = document.createElement('button');
    structureBtn.className = 'job-structure-btn';
    structureBtn.title = 'Folder structure for this job';
    structureBtn.innerHTML = '<svg><use href="#ico-folder-tree"/></svg>';
    structureBtn.addEventListener('click', function() { openStructurePopup(jobCard); });

    var reportSettingsBtn = document.createElement('button');
    reportSettingsBtn.className = 'job-structure-btn job-report-settings-btn';
    reportSettingsBtn.title = 'Report settings for this job';
    reportSettingsBtn.innerHTML = '<svg><use href="#ico-report-settings"/></svg>';
    reportSettingsBtn.addEventListener('click', function(e) {
        e.stopPropagation();
        openReportSettingsPopup(jobCard);
    });

    // Trailing group after the MHL toggle: template / structure / note / report-settings.
    jobOptsRow.appendChild(templateInput);
    jobOptsRow.appendChild(structureBtn);
    jobOptsRow.appendChild(commentBtn);
    jobOptsRow.appendChild(reportSettingsBtn);

    card.appendChild(jobOptsRow);

    jobCard.appendChild(card);

    // ── Per-job progress bar ──────────────────────────────────────────────────
    var jobProgress = document.createElement('div');
    jobProgress.className = 'job-progress hidden';
    var jobProgressTrack = document.createElement('div');
    jobProgressTrack.className = 'job-progress-track';
    var jobProgressFillEl = document.createElement('div');
    jobProgressFillEl.className = 'job-progress-fill';
    jobProgressTrack.appendChild(jobProgressFillEl);
    var jobProgressInfoEl = document.createElement('div');
    jobProgressInfoEl.className = 'job-progress-info';
    var jobProgressTextEl = document.createElement('span');
    jobProgressTextEl.className = 'job-progress-text';
    var jobProgressEtaEl = document.createElement('span');
    jobProgressEtaEl.className = 'job-progress-eta';
    jobProgressInfoEl.appendChild(jobProgressTextEl);
    jobProgressInfoEl.appendChild(jobProgressEtaEl);
    jobProgress.appendChild(jobProgressTrack);
    jobProgress.appendChild(jobProgressInfoEl);
    jobCard.appendChild(jobProgress);

    jobsContainer.appendChild(jobCard);
    addDestRowToJob(destListEl);
    renumberJobs();

    jobCard.addEventListener('click', function() {
        if (jobCard.isConnected) setSelectedJob(jobCard);
    });
    setSelectedJob(jobCard);
    refreshPreviewSoon();
}

function renumberJobs() {
    var cards = jobsContainer.querySelectorAll('.job-group');
    var count = cards.length;
    cards.forEach(function(card, idx) {
        var lbl = card.querySelector('.job-label');
        if (lbl) lbl.placeholder = count > 1 ? 'Job ' + (idx + 1) : 'Job';
        var btn = card.querySelector('.job-remove-btn');
        if (btn) btn.style.display = count > 1 ? '' : 'none';
    });
}

// Returns [{src, dsts, copyAsSubfolder, hashAlgo, genCsv, genPdf, genHtml, genMhl, comment, location}]
function getJobs() {
    var result = [];
    jobsContainer.querySelectorAll('.job-group').forEach(function(card) {
        var srcEl       = card.querySelector('.job-src-input');
        var nameEl      = card.querySelector('.job-label');
        var subfolderEl = card.querySelector('.job-subfolder-chk');
        var hashSelEl   = card.querySelector('.job-hash-select');
        var csvEl       = card.querySelector('.job-chk-csv');
        var pdfEl       = card.querySelector('.job-chk-pdf');
        var htmlEl      = card.querySelector('.job-chk-html');
        var mhlEl       = card.querySelector('.job-chk-mhl');
        var src  = srcEl ? expandPath(srcEl.value.trim()) : '';
        var dsts = Array.from(card.querySelectorAll('.job-dest-list input[type="text"]'))
            .map(function(i) { return expandPath(i.value.trim()); })
            .filter(function(v) { return v.length > 0; });
        result.push({
            src:             src,
            dsts:            dsts,
            name:            nameEl ? nameEl.value.trim() : '',
            copyAsSubfolder: subfolderEl ? subfolderEl.checked : false,
            hashAlgo:        hashSelEl   ? hashSelEl.value     : 'none',
            genCsv:          csvEl       ? csvEl.checked       : false,
            genPdf:          pdfEl       ? pdfEl.checked       : false,
            genHtml:         htmlEl      ? htmlEl.checked      : false,
            genMhl:          mhlEl       ? (mhlEl.checked && !mhlEl.disabled) : false,
            comment:         card.dataset.comment     || '',
            mhl_comment:     card.dataset.mhl_comment || '',
            location:        card.dataset.location    || '',
            template:         card.dataset.template         || '',
            jobvars:          (function() { try { return JSON.parse(card.dataset.jobvars || '{}'); } catch(e) { return {}; } })(),
            checksumName:     card.dataset.checksumName     || '',
            reportName:       card.dataset.reportName       || '',
            reportSubfolder:  card.dataset.reportSubfolder  || '',
        });
    });
    return result;
}

addJobBtn.addEventListener('click', function() { addJob(); });

// ── Copy event listeners ──────────────────────────────────────────────────────
var unlisteners = [];

async function registerListeners() {
    unlisteners.forEach(function(fn) { fn(); });
    unlisteners = [];

    unlisteners.push(await listen('copy-progress', function(event) {
        var cards = jobsContainer.querySelectorAll('.job-group');
        var activeCard = cards[currentJobIndex];
        if (activeCard) {
            var fraction = event.payload.fraction;
            var fill  = activeCard.querySelector('.job-progress-fill');
            var text  = activeCard.querySelector('.job-progress-text');
            var etaEl = activeCard.querySelector('.job-progress-eta');
            if (fill) fill.style.width = Math.round(fraction * 100) + '%';
            if (text) text.textContent = event.payload.label;
            if (etaEl) {
                // ETA is computed in Rust (phase-aware slowest-medium model) and
                // sent as eta_secs. It is null during warmup / scan / done — in
                // those cases we leave the field empty rather than guessing.
                var etaSecs = event.payload.eta_secs;
                if (fraction > 0.01 && fraction < 0.99 && etaSecs != null) {
                    etaEl.textContent = 'Remaining time: ' + formatEta(etaSecs);
                } else if (fraction >= 0.99) {
                    etaEl.textContent = '';
                }
            }
        }
    }));

    unlisteners.push(await listen('copy-log', function(event) {
        logView.textContent += event.payload.line;
        logView.scrollTop = logView.scrollHeight;
    }));

    unlisteners.push(await listen('copy-prompt', async function(event) {
        await showPrompt(event.payload.kind, event.payload.items || [], event.payload.conflict_items || null);
    }));

    unlisteners.push(await listen('copy-paused', function() {
        copyIsPaused = true;
        pauseBtn.disabled = false;
        pauseBtn.innerHTML = '<svg width="18" height="18"><use href="#ico-play"/></svg>';
        pauseBtn.title = 'Resume transfer';
    }));

    unlisteners.push(await listen('copy-resumed', function() {
        copyIsPaused = false;
        pauseBtn.disabled = false;
        pauseBtn.innerHTML = '<svg width="18" height="18"><use href="#ico-pause"/></svg>';
        pauseBtn.title = 'Pause transfer';
    }));
}

// ── Launch copy ───────────────────────────────────────────────────────────────

// runJob starts one copy operation and resolves when copy-done fires.
async function runJob(job) {
    return new Promise(async function(resolve) {
        var unlisten = await listen('copy-done', function(event) {
            unlisten();
            resolve({ ok: event.payload.ok, summary: event.payload.summary });
        });

        try {
            var srcBase = (job.src || '').replace(/\\/g, '/').replace(/\/+$/, '').split('/').pop() || '';
            await invoke('start_copy', {
                args: {
                    src:               job.src,
                    destinations:      job.dsts,
                    hash_algo:         job.hashAlgo  || 'none',
                    gen_csv:           job.genCsv    || false,
                    gen_pdf:           job.genPdf    || false,
                    gen_html:          job.genHtml   || false,
                    gen_mhl:           job.genMhl    || false,
                    copy_as_subfolder: job.copyAsSubfolder || false,
                    comment:           job.comment      || '',
                    mhl_comment:       job.mhl_comment  || '',
                    location:          job.location     || '',
                    open_dest:         false,
                    checksum_name_override: resolveNameTemplate(job.checksumName    || '', srcBase),
                    report_name_override:   resolveNameTemplate(job.reportName      || '', srcBase),
                    report_subfolder:       resolveNameTemplate(job.reportSubfolder || '', srcBase),
                }
            });
        } catch(e) {
            unlisten();
            resolve({ ok: false, summary: 'Error: ' + e });
        }
    });
}

async function launchCopy() {
    userCancelledQueue = false;
    var cards = Array.prototype.slice.call(jobsContainer.querySelectorAll('.job-group'));
    var jobs  = getJobs();   // parallel to `cards` by index

    // ── Decide which jobs to run (skip already-done ones on request) ──────────
    var allIdx  = jobs.map(function(_, i) { return i; });
    var doneIdx = allIdx.filter(function(i) { return cards[i] && cards[i].dataset.status === 'done'; });
    var runSet  = allIdx;

    if (doneIdx.length > 0) {
        var choice = await showChoice(
            'Some jobs are already done.',
            doneIdx.length + ' of ' + jobs.length + ' job(s) are marked as done.\n\n' +
            'Choose to re-run everything, or only the jobs that are not done yet.',
            [
                { label: 'Cancel',                 value: null },
                { label: 'Start undone jobs only', value: 'undone' },
                { label: 'Start all jobs',         value: 'all', kind: 'suggested' },
            ]);
        if (choice === null) return;
        if (choice === 'all') {
            cards.forEach(function(c) { setJobStatus(c, 'idle'); });
            runSet = allIdx;
        } else {
            runSet = allIdx.filter(function(i) { return cards[i].dataset.status !== 'done'; });
        }
    }

    if (runSet.length === 0) {
        statusLabel.textContent = 'Nothing to run — all jobs are already done.';
        statusLabel.className = '';
        return;
    }

    var multi = runSet.length > 1;

    // ── Validate the jobs we are about to run ─────────────────────────────────
    for (var s = 0; s < runSet.length; s++) {
        var i = runSet[s];
        var prefix = (jobs.length > 1) ? 'Job ' + (i + 1) + ': p' : 'P';
        if (!jobs[i].src)         { alert(prefix + 'lease choose a source directory.'); return; }
        if (!jobs[i].dsts.length) { alert(prefix + 'lease add at least one destination.'); return; }
    }

    // Resolve per-job folder templates for the jobs we run: rewrite each
    // destination to root + '/' + <resolved template>. The copy engine then
    // builds the full hierarchy via create_dir_all — no backend change needed.
    for (var s = 0; s < runSet.length; s++) {
        var i = runSet[s];
        var res = resolveTemplate(jobs[i]);
        if (res.bad && res.bad.length) {
            var jp = (jobs.length > 1) ? 'Job ' + (i + 1) + ': ' : '';
            alert(jp + 'the folder template has unresolved tokens: ' + res.bad.join(', ') +
                  '\n\nFix it in the job structure popup or in Settings → Structure.');
            return;
        }
        if (res.path) {
            jobs[i].dsts = jobs[i].dsts.map(function(d) { return joinPath(d, res.path); });
        }
    }

    setCopyInProgress(true);
    currentJobIndex = -1;
    statusLabel.textContent = '';
    statusLabel.className   = '';
    logView.textContent     = '';

    // Reset progress bars and status for the jobs we will run.
    runSet.forEach(function(idx) {
        setJobStatus(cards[idx], 'idle');
        var p = cards[idx] ? cards[idx].querySelector('.job-progress') : null;
        if (!p) return;
        p.classList.add('hidden');
        var f = p.querySelector('.job-progress-fill');
        var t = p.querySelector('.job-progress-text');
        var e = p.querySelector('.job-progress-eta');
        if (f) {
            f.style.transition = 'none';
            f.style.width = '0%';
            f.classList.remove('job-progress-done', 'job-progress-error');
            void f.offsetWidth;
            f.style.transition = '';
        }
        if (t) t.textContent = '';
        if (e) e.textContent = '';
    });

    await registerListeners();

    var allOk       = true;
    var lastSummary = '';

    for (var s = 0; s < runSet.length; s++) {
        var i   = runSet[s];
        var job = jobs[i];
        var jobCards  = jobsContainer.querySelectorAll('.job-group');
        var jobProgEl = jobCards[i] ? jobCards[i].querySelector('.job-progress') : null;

        copyJobStartTime = null;
        currentJobIndex = i;
        setJobStatus(jobCards[i], 'running');

        if (jobProgEl) {
            jobProgEl.classList.remove('hidden');
            jobProgEl.querySelector('.job-progress-fill').style.width = '0%';
            jobProgEl.querySelector('.job-progress-fill').classList.remove('job-progress-error');
            jobProgEl.querySelector('.job-progress-text').textContent = 'Starting…';
            var etaSpan = jobProgEl.querySelector('.job-progress-eta');
            if (etaSpan) etaSpan.textContent = '';
        }

        if (multi) {
            currentJobPrefix = 'Job ' + (s + 1) + '/' + runSet.length;
            logView.textContent += '\n── ' + currentJobPrefix + ' — ' + job.src + '\n';
            logView.scrollTop = logView.scrollHeight;
        }

        var result = await runJob(job);

        setJobStatus(jobCards[i], result.ok ? 'done' : 'failed');
        if (jobProgEl) {
            var fill = jobProgEl.querySelector('.job-progress-fill');
            fill.style.width = '100%';
            if (result.ok) fill.classList.add('job-progress-done');
            else           fill.classList.add('job-progress-error');
            jobProgEl.querySelector('.job-progress-text').textContent = result.summary || (result.ok ? 'Done' : 'Error');
            var doneEta = jobProgEl.querySelector('.job-progress-eta');
            if (doneEta) doneEta.textContent = '';
        }

        if (!result.ok) allOk = false;
        lastSummary = result.summary;

        if (multi) {
            logView.textContent += (result.ok ? '✓ ' : '✗ ') + result.summary + '\n';
            logView.scrollTop = logView.scrollHeight;
        }

        if (userCancelledQueue) break;
    }

    userCancelledQueue = false;
    currentJobPrefix   = '';
    unlisteners.forEach(function(fn) { fn(); });
    unlisteners = [];

    statusLabel.textContent = multi
        ? (allOk ? 'All ' + runSet.length + ' jobs completed successfully.' : 'Queue finished with errors — check log.')
        : lastSummary;
    statusLabel.className = allOk ? 'success' : 'error';
    setCopyInProgress(false);

    // Auto-save log
    try {
        var logPath = await invoke('save_log', { content: logView.textContent });
        logView.textContent += '\nLog saved: ' + logPath + '\n';
        logView.scrollTop = logView.scrollHeight;
    } catch(e) {
        console.warn('save_log failed:', e);
    }

    // System notification
    try {
        await invoke('send_notification', {
            title: 'Bartleby',
            body:  statusLabel.textContent
        });
    } catch(e) {
        console.warn('Notification failed:', e);
    }

    // Open destinations (setting stored in currentSettings.open_dest)
    if (allOk && currentSettings && currentSettings.open_dest) {
        var allDsts = [];
        runSet.forEach(function(i) { allDsts = allDsts.concat(jobs[i].dsts); });
        var uniqueDsts = allDsts.filter(function(d, idx) { return allDsts.indexOf(d) === idx; });
        if (uniqueDsts.length > 0) {
            try { await invoke('open_destinations', { paths: uniqueDsts }); }
            catch(e) { console.error('open_destinations:', e); }
        }
    }
}

copyBtn.addEventListener('click', function() { launchCopy(); });

pauseBtn.addEventListener('click', async function() {
    if (!copyIsPaused) {
        pauseBtn.disabled = true;
        try { await invoke('pause_copy'); } catch(e) {}
    } else {
        pauseBtn.disabled = true;
        try { await invoke('resume_copy'); } catch(e) {}
    }
});

cancelBtn.addEventListener('click', async function() {
    var ok = await confirmDialog(
        'Cancel copy',
        'A copy is in progress. Cancel it?\n\nNothing already copied is deleted, but the current transfer will stop.',
        'Cancel copy', 'Keep copying', true);
    if (!ok || !copyInProgress) return;
    cancelBtn.disabled = true;
    pauseBtn.disabled  = true;
    userCancelledQueue = true;
    try { await invoke('cancel_copy'); } catch(e) {}
});

// ── Warn before quitting while a copy is running ───────────────────────────────
// The Rust main-window CloseRequested handler vetoes the close and emits this
// event; we decide here whether to quit (always via the quit_app command).
(async function setupCloseGuard() {
    try {
        await listen('app-close-requested', async function() {
            if (copyInProgress) {
                var ok = await confirmDialog(
                    'Quit Bartleby?',
                    'A copy is still in progress. Quitting now will cancel it.\n\nNothing already copied is deleted.',
                    'Quit', 'Stay', true);
                if (!ok) return;
                userCancelledQueue = true;
                try { await invoke('cancel_copy'); } catch(e) {}
            }
            try { await invoke('quit_app'); } catch(e) {}
        });
    } catch(e) { console.warn('close guard setup failed:', e); }
})();

// ── Prompt modal ──────────────────────────────────────────────────────────────
var promptOverlay = document.getElementById('prompt-overlay');

function showPrompt(kind, items, conflictItems) {
    return new Promise(function(resolve) {
        var title   = document.getElementById('prompt-title');
        var message = document.getElementById('prompt-message');
        var btnRow  = document.getElementById('prompt-btn-row');
        btnRow.innerHTML = '';
        message.innerHTML = '';
        message.removeAttribute('style');
        title.innerHTML = '';
        title.style.position = '';
        title.style.zIndex   = '';

        if (kind === 'mhl_conflict') {
            title.textContent = 'Existing MHL at destination';
            message.textContent =
                'A previous MHL was found at this destination:\n\n' +
                items[1] + '\n\n' +
                'Destination: ' + items[0] + '\n\n' +
                'This usually means a previous copy was attempted here. ' +
                '"Replace" removes the old MHL and writes a fresh one. ' +
                '"Keep both" appends a new generation alongside it.';
            addPromptBtn(btnRow, 'Skip MHL',  'cancel',   false, false, resolve);
            addPromptBtn(btnRow, 'Keep both', 'skip',     false, false, resolve);
            addPromptBtn(btnRow, 'Replace',   'continue', true,  false, resolve);
        } else if (kind === 'non_empty') {
            title.textContent = 'Non-empty destination';
            message.textContent =
                'The following destination(s) already contain files:\n\n' +
                items.join('\n') + '\n\n' +
                'Nothing will be deleted. You are about to copy new files alongside the existing ones. Continue?';
            addPromptBtn(btnRow, 'Cancel',   'cancel',   false, false, resolve);
            addPromptBtn(btnRow, 'Continue', 'continue', true,  false, resolve);
        } else {
            title.textContent = 'File conflicts detected';
            title.style.position = 'relative';
            title.style.zIndex   = '10';

            var helpBtn = document.createElement('button');
            helpBtn.className = 'conflict-help-btn';
            helpBtn.setAttribute('aria-label', 'Help');
            helpBtn.innerHTML = '<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><path d="M9.09 9a3 3 0 0 1 5.83 1c0 2-3 3-3 3"/><path d="M12 17h.01"/></svg>';

            var helpPopover = document.createElement('div');
            helpPopover.className = 'conflict-help-popover hidden';
            helpPopover.innerHTML =
                '<dl>' +
                '<dt>Cancel</dt>' +
                '<dd>Abort the copy entirely. Nothing is written to any destination.</dd>' +
                '<dt>Skip and keep</dt>' +
                '<dd>Identical files (same size &amp; date) are skipped. Differing files: the existing destination copy is renamed <em>_conflict_01</em>, then the source file is copied normally and can be verified.</dd>' +
                '<dt>Skip and replace</dt>' +
                '<dd>Identical files (same size &amp; date) are skipped. Files that differ are overwritten with the source version.</dd>' +
                '<dt>Replace all</dt>' +
                '<dd>All conflicting files are overwritten with the source, regardless of size or date.</dd>' +
                '</dl>';
            helpBtn.appendChild(helpPopover);

            var helpPinned = false;
            helpBtn.addEventListener('mouseenter', function() {
                helpPopover.classList.remove('hidden');
            });
            helpBtn.addEventListener('mouseleave', function() {
                if (!helpPinned) helpPopover.classList.add('hidden');
            });
            helpBtn.addEventListener('click', function(e) {
                e.stopPropagation();
                helpPinned = !helpPinned;
                helpPopover.classList.toggle('hidden', !helpPinned);
                helpPopover.classList.toggle('pinned', helpPinned);
            });
            document.addEventListener('click', function closeHelp() {
                helpPinned = false;
                helpPopover.classList.add('hidden');
                helpPopover.classList.remove('pinned');
                document.removeEventListener('click', closeHelp);
            });

            title.appendChild(helpBtn);

            message.style.background   = 'none';
            message.style.padding      = '0';
            message.style.maxHeight    = 'none';
            message.style.whiteSpace   = 'normal';
            message.style.borderRadius = '0';

            var countEl = document.createElement('p');
            countEl.style.cssText = 'margin:0 0 8px; font-size:12px; color:var(--text-dim)';
            countEl.textContent = (conflictItems || []).length + ' file(s) already exist in one or more destinations.';
            message.appendChild(countEl);

            var wrap  = document.createElement('div');
            wrap.className = 'conflict-table-wrap';
            var table = document.createElement('table');
            table.className = 'conflict-table';

            var thead = document.createElement('thead');
            var hrow  = document.createElement('tr');
            ['File', 'Size', 'Date'].forEach(function(col) {
                var th = document.createElement('th');
                th.textContent = col;
                hrow.appendChild(th);
            });
            thead.appendChild(hrow);
            table.appendChild(thead);

            var tbody = document.createElement('tbody');
            (conflictItems || []).forEach(function(item) {
                var tr = document.createElement('tr');

                var tdFile = document.createElement('td');
                tdFile.className   = 'conflict-filename';
                tdFile.textContent = item.rel_path;
                tr.appendChild(tdFile);

                var tdSize = document.createElement('td');
                tdSize.className   = 'conflict-status ' + (item.size_match ? 'match-ok' : 'match-fail');
                tdSize.textContent = item.size_match ? '✓' : '✗';
                tr.appendChild(tdSize);

                var tdDate = document.createElement('td');
                tdDate.className   = 'conflict-status ' + (item.date_match ? 'match-ok' : 'match-fail');
                tdDate.textContent = item.date_match ? '✓' : '✗';
                tr.appendChild(tdDate);

                tbody.appendChild(tr);
            });
            table.appendChild(tbody);
            wrap.appendChild(table);
            message.appendChild(wrap);

            addPromptBtn(btnRow, 'Cancel',           'cancel',    false, false, resolve);
            addPromptBtn(btnRow, 'Skip and keep',    'skip_keep', false, false, resolve);
            addPromptBtn(btnRow, 'Skip and replace', 'skip',      true,  false, resolve);
            addPromptBtn(btnRow, 'Replace all',      'continue',  true,  true,  resolve);
        }
        promptOverlay.classList.remove('hidden');
    });
}

function addPromptBtn(row, label, reply, suggested, danger, resolve) {
    var btn = document.createElement('button');
    btn.className = 'action-btn' + (suggested ? ' suggested' : '') + (danger ? ' danger' : '');
    btn.textContent = label;
    btn.addEventListener('click', async function() {
        promptOverlay.classList.add('hidden');
        try { await invoke('prompt_reply', { reply: reply }); } catch(e) {}
        resolve(reply);
    });
    row.appendChild(btn);
}

// ── Generic confirm / choice modal ─────────────────────────────────────────────
// showChoice(title, message, buttons) where buttons = [{label, value, kind}],
// kind ∈ {'suggested','danger', undefined}. Resolves to the chosen value, or null
// if dismissed via Escape. Independent of the copy-prompt overlay so it can be
// shown at any time (including while a copy prompt would be active).
function showChoice(title, message, buttons) {
    return new Promise(function(resolve) {
        var overlay = document.getElementById('confirm-overlay');
        var titleEl = document.getElementById('confirm-title');
        var msgEl   = document.getElementById('confirm-message');
        var row     = document.getElementById('confirm-btn-row');
        titleEl.textContent = title;
        msgEl.textContent   = message;
        row.innerHTML = '';

        function close(value) {
            overlay.classList.add('hidden');
            document.removeEventListener('keydown', onKey);
            resolve(value);
        }
        function onKey(e) { if (e.key === 'Escape') close(null); }

        buttons.forEach(function(b) {
            var btn = document.createElement('button');
            btn.className = 'action-btn' +
                (b.kind === 'suggested' ? ' suggested' : '') +
                (b.kind === 'danger'    ? ' danger'    : '');
            btn.textContent = b.label;
            btn.addEventListener('click', function() { close(b.value); });
            row.appendChild(btn);
        });

        document.addEventListener('keydown', onKey);
        overlay.classList.remove('hidden');
    });
}

// Two-button yes/no helper built on showChoice. Resolves true / false.
function confirmDialog(title, message, confirmLabel, cancelLabel, danger) {
    return showChoice(title, message, [
        { label: cancelLabel,  value: false },
        { label: confirmLabel, value: true, kind: danger ? 'danger' : 'suggested' },
    ]).then(function(v) { return v === true; });
}

// ── Drag & drop ───────────────────────────────────────────────────────────────
(async function setupDragDrop() {
    try {
        await window.__TAURI__.event.listen('tauri://drag-drop', function(event) {
            var paths = event.payload.paths;
            if (!paths || paths.length === 0) return;

            var droppedPath = paths[0];
            var dropY = event.payload.position ? event.payload.position.y : 0;

            var jobCards  = Array.from(jobsContainer.querySelectorAll('.job-group'));
            var targetCard = null;
            for (var j = 0; j < jobCards.length; j++) {
                var r = jobCards[j].getBoundingClientRect();
                if (dropY >= r.top && dropY <= r.bottom + 40) {
                    targetCard = jobCards[j];
                    break;
                }
            }
            if (!targetCard && jobCards.length > 0) targetCard = jobCards[jobCards.length - 1];
            if (!targetCard) return;

            var srcInputEl = targetCard.querySelector('.job-src-input');
            var destListEl = targetCard.querySelector('.job-dest-list');

            var inSource = false;
            if (srcInputEl) {
                var srcSection = srcInputEl.closest('.job-src-section');
                if (srcSection) {
                    var sr = srcSection.getBoundingClientRect();
                    var dropX = event.payload.position ? event.payload.position.x : 0;
                    inSource = dropX >= sr.left && dropX <= sr.right;
                }
            }

            if (inSource) {
                if (srcInputEl) {
                    srcInputEl.value = shortenPath(droppedPath);
                    var srcVi = targetCard.querySelector('.job-src-section .vol-info');
                    if (srcVi) updateSrcInfo(srcVi, srcInputEl.value);
                }
            } else if (destListEl) {
                var inputs = Array.from(destListEl.querySelectorAll('input[type="text"]'));
                var emptyInput = inputs.find(function(i) { return i.value.trim() === ''; });
                if (emptyInput) {
                    emptyInput.value = shortenPath(droppedPath);
                    var destItem = emptyInput.closest('.dest-item');
                    if (destItem) updateVolInfo(destItem.querySelector('.vol-info'), emptyInput.value);
                } else {
                    addDestRowToJob(destListEl, shortenPath(droppedPath));
                }
            }
        });

        await window.__TAURI__.event.listen('tauri://drag-cancelled', function() {});

    } catch(e) {
        console.warn('Drag & drop not available:', e);
    }
})();

// ── Right panel resizer ───────────────────────────────────────────────────────
(function() {
    var resizer    = document.getElementById('right-resizer');
    var rightPanel = document.getElementById('right-panel');
    var panels     = document.getElementById('main-panels');
    var resizing   = false;

    resizer.addEventListener('mousedown', function(e) {
        resizing = true;
        resizer.classList.add('active');
        document.body.style.cursor     = 'col-resize';
        document.body.style.userSelect = 'none';
        e.preventDefault();
    });

    document.addEventListener('mousemove', function(e) {
        if (!resizing) return;
        var rect = panels.getBoundingClientRect();
        var maxW = panels.offsetWidth - 6 - 320;   /* keep a usable minimum width for the jobs panel */
        var newW = Math.max(150, Math.min(Math.max(150, maxW), rect.right - e.clientX));
        rightPanel.style.flex  = 'none';
        rightPanel.style.width = newW + 'px';
    });

    document.addEventListener('mouseup', function() {
        if (!resizing) return;
        resizing = false;
        resizer.classList.remove('active');
        document.body.style.cursor     = '';
        document.body.style.userSelect = '';
        /* Bake the dragged width into flex-basis so CSS takes over cleanly */
        var w = rightPanel.offsetWidth;
        rightPanel.style.flex  = '0 0 ' + w + 'px';
        rightPanel.style.width = '';
    });
})();

// ── Log panel toggle ──────────────────────────────────────────────────────────
(function() {
    var mainPanels   = document.getElementById('main-panels');
    var rightPanel   = document.getElementById('right-panel');
    var rightResizer = document.getElementById('right-resizer');
    var toggleBtn    = document.getElementById('log-toggle-btn');
    var savedRightW  = 0;
    var animTimer    = null;
    var isCollapsed  = false;
    var DURATION     = 280;
    var EASE         = 'cubic-bezier(0.4,0,0.2,1)';

    function setCollapsed(c, instant) {
        if (c === isCollapsed) return;
        isCollapsed = c;
        if (animTimer) { clearTimeout(animTimer); animTimer = null; }

        toggleBtn.title = c ? 'Show log panel' : 'Hide log panel';
        var T = DURATION + 'ms ' + EASE;

        if (c) {
            savedRightW = rightPanel.offsetWidth || 300;
            rightResizer.classList.add('hidden');

            rightPanel.style.transition = '';
            rightPanel.style.flex       = 'none';
            rightPanel.style.minWidth   = '0';
            rightPanel.style.width      = savedRightW + 'px';

            void rightPanel.offsetWidth;

            if (!instant) { rightPanel.style.transition = 'width ' + T; }
            rightPanel.style.width = '0px';

        } else {
            var targetW = savedRightW || 300;

            rightPanel.style.transition = '';
            rightPanel.style.flex       = 'none';
            rightPanel.style.minWidth   = '0';
            rightPanel.style.width      = '0px';

            void rightPanel.offsetWidth;

            if (!instant) { rightPanel.style.transition = 'width ' + T; }
            rightPanel.style.width = targetW + 'px';

            var cleanup = function() {
                rightResizer.classList.remove('hidden');
                /* Bake targetW into flex-basis to preserve user-resized width */
                rightPanel.style.flex       = '0 0 ' + targetW + 'px';
                rightPanel.style.width      = '';
                rightPanel.style.minWidth   = '';
                rightPanel.style.transition = '';
                animTimer = null;
            };
            if (instant) { cleanup(); }
            else { animTimer = setTimeout(cleanup, DURATION + 16); }
        }

        try { localStorage.setItem('bartleby_log_collapsed', c ? '1' : '0'); } catch(e) {}
    }

    toggleBtn.addEventListener('click', function() { setCollapsed(!isCollapsed); });

    try {
        if (localStorage.getItem('bartleby_log_collapsed') === '1') setCollapsed(true, true);
    } catch(e) {}
})();

// ── Comment modal ─────────────────────────────────────────────────────────────
(function() {
    var overlay    = document.getElementById('comment-overlay');
    var editor     = document.getElementById('comment-editor');
    var saveBtn    = document.getElementById('comment-save');
    var cancelBtn  = document.getElementById('comment-cancel');
    var activeCard = null;

    // Formatting toolbar: mousedown so focus stays in the editor
    document.querySelectorAll('.comment-fmt-btn').forEach(function(btn) {
        btn.addEventListener('mousedown', function(e) {
            e.preventDefault();
            document.execCommand(btn.dataset.cmd, false, null);
        });
    });

    window.openCommentModal = function(jobCard) {
        activeCard = jobCard;
        var locEl = document.getElementById('comment-location');
        if (locEl) locEl.value = jobCard.dataset.location || '';
        editor.innerHTML = jobCard.dataset.comment || '';
        var mhlEl = document.getElementById('mhl-comment-editor');
        if (mhlEl) mhlEl.value = jobCard.dataset.mhl_comment || '';
        overlay.classList.remove('hidden');
        // Small delay so the overlay is visible before focus fires
        setTimeout(function() { editor.focus(); }, 30);
    };

    function doSave() {
        if (!activeCard) return;
        var locEl = document.getElementById('comment-location');
        var loc   = locEl ? locEl.value.trim() : '';
        activeCard.dataset.location = loc;
        var html  = editor.innerHTML.trim();
        // Treat markup that resolves to no visible text as empty
        var plain = html.replace(/<[^>]+>/g, '').replace(/\s/g, '');
        if (!plain) html = '';
        activeCard.dataset.comment = html;
        var mhlEl = document.getElementById('mhl-comment-editor');
        var mhlComment = mhlEl ? mhlEl.value.trim() : '';
        activeCard.dataset.mhl_comment = mhlComment;
        var btn = activeCard.querySelector('.job-comment-btn');
        if (btn) btn.classList.toggle('has-comment', html.length > 0 || loc.length > 0 || mhlComment.length > 0);
        overlay.classList.add('hidden');
        activeCard = null;
    }

    saveBtn.addEventListener('click', doSave);

    cancelBtn.addEventListener('click', function() {
        overlay.classList.add('hidden');
        activeCard = null;
    });

    overlay.addEventListener('click', function(e) {
        if (e.target === overlay) {
            overlay.classList.add('hidden');
            activeCard = null;
        }
    });

    // Ctrl+Enter / Cmd+Enter saves
    editor.addEventListener('keydown', function(e) {
        if ((e.ctrlKey || e.metaKey) && e.key === 'Enter') {
            e.preventDefault();
            doSave();
        }
    });
})();

// ══ FOLDER STRUCTURE — left side panel, templates, preview ═══════════════════

// ── Path / template helpers ───────────────────────────────────────────────────
function joinPath(a, b) {
    if (!a) return b || '';
    if (!b) return a;
    return a.replace(/[\/\\]+$/, '') + '/' + b.replace(/^[\/\\]+/, '');
}

function baseName(p) {
    var s = String(p || '').replace(/[\/\\]+$/, '');
    var i = Math.max(s.lastIndexOf('/'), s.lastIndexOf('\\'));
    return i >= 0 ? s.slice(i + 1) : s;
}

function pad2(n) { return n < 10 ? '0' + n : '' + n; }

// Minimal strftime — supports the tokens a DIT folder name realistically uses.
function formatStrftime(fmt) {
    var d = new Date();
    var doy = Math.floor((d - new Date(d.getFullYear(), 0, 0)) / 86400000);
    var map = {
        Y: '' + d.getFullYear(),
        y: ('' + d.getFullYear()).slice(-2),
        m: pad2(d.getMonth() + 1),
        d: pad2(d.getDate()),
        H: pad2(d.getHours()),
        M: pad2(d.getMinutes()),
        S: pad2(d.getSeconds()),
        j: ('00' + doy).slice(-3)
    };
    return String(fmt || '').replace(/%([A-Za-z%])/g, function(m, c) {
        if (c === '%') return '%';
        return (c in map) ? map[c] : m;
    });
}

// Expand #presetName references recursively (bounded depth). Unknown presets
// are recorded in `bad` and left literal.
function expandPresets(tmpl, presets, depth, bad) {
    if (depth > 8) return tmpl;
    return String(tmpl || '').replace(/#([A-Za-z0-9_-]+)/g, function(m, name) {
        var p = presets.find(function(pr) {
            return pr.name && pr.name.toLowerCase() === name.toLowerCase();
        });
        if (!p) { bad.push(m); return m; }
        return expandPresets(p.template, presets, depth + 1, bad);
    });
}

// Resolve a job's folder template into a relative path.
// Returns { path: "relative/path", bad: ["%foo", "#bar"] }.
function resolveTemplate(job) {
    var raw = (job && job.template ? job.template : '').trim();
    var bad = [];
    if (!raw) return { path: '', bad: bad };

    var presets = (currentSettings && currentSettings.folder_presets) || [];
    var tmpl = expandPresets(raw, presets, 0, bad);

    var jobvars = (job && job.jobvars) || {};
    var vars = {
        date:    formatStrftime((currentSettings && currentSettings.folder_var_date_format) || '%Y-%m-%d'),
        day:     (currentSettings && currentSettings.folder_shoot_day) || '',
        project: (currentSettings && currentSettings.project_title) || '',
    };
    ((currentSettings && currentSettings.folder_variables) || []).forEach(function(fv) {
        vars[fv.name.toLowerCase()] = jobvars[fv.name.toLowerCase()] || '';
    });

    var out = tmpl.replace(/%([A-Za-z]+)/g, function(m, name) {
        var key = name.toLowerCase();
        if (key in vars) {
            if (!vars[key]) { bad.push('%' + name); return '%' + name; }
            return vars[key];
        }
        bad.push('%' + name);
        return '%' + name;
    });

    out = out.replace(/\\/g, '/').replace(/\/+/g, '/').replace(/^\/+|\/+$/g, '');
    bad = bad.filter(function(v, i) { return bad.indexOf(v) === i; });
    return { path: out, bad: bad };
}

function highlightBad(text, bad) {
    var html = escHtml(text);
    (bad || []).forEach(function(tok) {
        html = html.split(escHtml(tok)).join('<span class="tok-bad">' + escHtml(tok) + '</span>');
    });
    return html;
}

// Resolve a name-override template (checksum / report).
// Returns the resolved filename string, or '' if template is blank.
// Supports %date, %day, %project, %source (= baseName of source path).
// Unresolved tokens are left as-is. Result is sanitized (no path separators).
function resolveNameTemplate(template, srcName) {
    var raw = (template || '').trim();
    if (!raw) return '';
    var vars = {
        date:    formatStrftime((currentSettings && currentSettings.folder_var_date_format) || '%Y-%m-%d'),
        day:     (currentSettings && currentSettings.folder_shoot_day) || '',
        project: (currentSettings && currentSettings.project_title) || '',
        source:  srcName || '',
    };
    var out = raw.replace(/%([A-Za-z]+)/g, function(m, name) {
        var key = name.toLowerCase();
        return (key in vars) ? vars[key] : m;
    });
    return out.replace(/[\/\\]/g, '_').trim();
}

// ── Job selection ─────────────────────────────────────────────────────────────
var selectedJobCard = null;

function setSelectedJob(card) {
    selectedJobCard = card;
    jobsContainer.querySelectorAll('.job-group').forEach(function(c) {
        c.classList.toggle('job-selected', c === card);
    });
}

// Sync the per-job template text field from card.dataset.template (e.g. after popup OK/cancel).
function syncJobTemplateInput(card) {
    if (!card) return;
    var inp = card.querySelector('.job-template-input');
    if (inp) inp.value = card.dataset.template || '';
}

// Refresh the preview tab when jobs/destinations change.
jobsContainer.addEventListener('input', function() { refreshPreviewSoon(); });

// ── Template field autocomplete ───────────────────────────────────────────────
(function() {
    var acEl        = document.getElementById('tmpl-ac');
    var acBlurTimer = null;
    var acPending   = false; // true while showing value sub-list for a variable

    function hide() {
        acEl.classList.add('hidden');
        acEl.innerHTML = '';
        acPending = false;
    }

    function position(input) {
        var r = input.getBoundingClientRect();
        acEl.style.left     = r.left + 'px';
        acEl.style.top      = (r.bottom + 2) + 'px';
        acEl.style.minWidth = r.width + 'px';
    }

    function show(input, items, onSelect) {
        acEl.innerHTML = '';
        if (!items.length) { hide(); return; }
        items.forEach(function(item) {
            var btn = document.createElement('button');
            btn.className   = 'tmpl-ac-item';
            btn.textContent = item;
            btn.addEventListener('mousedown', function(e) {
                e.preventDefault(); // block blur so click registers
                onSelect(item);
            });
            acEl.appendChild(btn);
        });
        position(input);
        acEl.classList.remove('hidden');
    }

    // Return { trigger, word, start, end } for a '#' or '%' token at the cursor,
    // or null if the cursor isn't inside such a token.
    function tokenAtCursor(input) {
        var val = input.value;
        var pos = input.selectionStart;
        var i = pos - 1;
        while (i >= 0 && /[A-Za-z0-9_-]/.test(val[i])) i--;
        if (i < 0 || (val[i] !== '#' && val[i] !== '%')) return null;
        return { trigger: val[i], word: val.slice(i + 1, pos), start: i, end: pos };
    }

    // Replace the token at [start,end) with text; return the new [start,end).
    function replaceToken(input, text, start, end) {
        var v = input.value;
        input.value = v.slice(0, start) + text + v.slice(end);
        var newEnd = start + text.length;
        input.setSelectionRange(newEnd, newEnd);
        return { start: start, end: newEnd };
    }

    function getAllVars() {
        var builtins = ['%date', '%day', '%project'];
        var custom = ((currentSettings && currentSettings.folder_variables) || [])
            .map(function(fv) { return '%' + fv.name; });
        return builtins.concat(custom);
    }

    function updateCard(input, jobCard) {
        jobCard.dataset.template = input.value;
        var sb = jobCard.querySelector('.job-structure-btn');
        if (sb) sb.classList.toggle('has-structure', !!input.value.trim());
        refreshPreviewSoon();
    }

    function handleInput(input, jobCard) {
        if (acPending) return;
        var token = tokenAtCursor(input);
        if (!token) { hide(); return; }
        var presets = (currentSettings && currentSettings.folder_presets) || [];
        var q = token.word.toLowerCase();

        if (token.trigger === '#') {
            var names = presets
                .filter(function(p) { return p.name && p.name.toLowerCase().indexOf(q) === 0; })
                .map(function(p) { return '#' + p.name; });
            show(input, names, function(item) {
                replaceToken(input, item, token.start, token.end);
                hide();
                updateCard(input, jobCard);
            });
        } else {
            var allVars = getAllVars().filter(function(v) {
                return v.toLowerCase().indexOf('%' + q) === 0;
            });
            show(input, allVars, function(item) {
                var pos = replaceToken(input, item, token.start, token.end);
                var varName = item.slice(1).toLowerCase();
                var fv = ((currentSettings && currentSettings.folder_variables) || [])
                    .find(function(v) { return v.name.toLowerCase() === varName; });
                var values = fv ? (fv.values || []) : [];
                if (values.length) {
                    acPending = true;
                    show(input, values, function(val) {
                        var v = input.value;
                        input.value = v.slice(0, pos.start) + val + v.slice(pos.end);
                        var newPos = pos.start + val.length;
                        input.setSelectionRange(newPos, newPos);
                        hide();
                        updateCard(input, jobCard);
                    });
                } else {
                    hide();
                    updateCard(input, jobCard);
                }
            });
        }
    }

    window.attachTemplateAutocomplete = function(input, jobCard) {
        input.addEventListener('input', function() {
            updateCard(input, jobCard);
            handleInput(input, jobCard);
        });

        input.addEventListener('blur', function() {
            acBlurTimer = setTimeout(hide, 160);
        });
        input.addEventListener('focus', function() {
            if (acBlurTimer) { clearTimeout(acBlurTimer); acBlurTimer = null; }
        });
        input.addEventListener('keydown', function(e) {
            if (acEl.classList.contains('hidden')) return;
            var items  = acEl.querySelectorAll('.tmpl-ac-item');
            var active = acEl.querySelector('.tmpl-ac-item.ac-active');
            var idx = -1;
            items.forEach(function(it, i) { if (it === active) idx = i; });
            if (e.key === 'ArrowDown') {
                e.preventDefault();
                if (active) active.classList.remove('ac-active');
                (items[idx + 1] || items[0]).classList.add('ac-active');
            } else if (e.key === 'ArrowUp') {
                e.preventDefault();
                if (active) active.classList.remove('ac-active');
                (items[idx - 1] || items[items.length - 1]).classList.add('ac-active');
            } else if (e.key === 'Enter' && active) {
                e.preventDefault();
                active.dispatchEvent(new MouseEvent('mousedown'));
            } else if (e.key === 'Escape') {
                hide();
            }
        });
    };
})();

// ── Side panel toggle ─────────────────────────────────────────────────────────
(function() {
    var sidePanel   = document.getElementById('side-panel');
    var leftResizer = document.getElementById('left-resizer');
    var toggleBtn   = document.getElementById('side-toggle-btn');
    var DURATION    = 280;
    var EASE        = 'cubic-bezier(0.4,0,0.2,1)';
    var isOpen      = false;
    var savedW      = 270;
    var animTimer   = null;

    try {
        var sw = parseInt(localStorage.getItem('bartleby_side_width'), 10);
        if (sw && sw > 140) savedW = sw;
    } catch(e) {}

    function setOpen(open, instant) {
        if (open === isOpen) return;
        isOpen = open;
        if (animTimer) { clearTimeout(animTimer); animTimer = null; }
        toggleBtn.classList.toggle('active', open);
        toggleBtn.title = open ? 'Hide explorer panel' : 'Show explorer panel';
        var T = DURATION + 'ms ' + EASE;

        if (open) {
            leftResizer.classList.remove('hidden');
            sidePanel.style.transition = '';
            sidePanel.style.flex = 'none';
            sidePanel.style.minWidth = '0';
            sidePanel.style.width = '0px';
            void sidePanel.offsetWidth;
            if (!instant) sidePanel.style.transition = 'width ' + T;
            sidePanel.style.width = savedW + 'px';
            var done = function() {
                sidePanel.style.flex = '0 0 ' + savedW + 'px';
                sidePanel.style.width = '';
                sidePanel.style.minWidth = '';
                sidePanel.style.transition = '';
                animTimer = null;
            };
            if (instant) done(); else animTimer = setTimeout(done, DURATION + 16);
            loadVolumes();
        } else {
            savedW = sidePanel.offsetWidth || savedW;
            try { localStorage.setItem('bartleby_side_width', String(savedW)); } catch(e) {}
            leftResizer.classList.add('hidden');
            sidePanel.style.transition = '';
            sidePanel.style.flex = 'none';
            sidePanel.style.minWidth = '0';
            sidePanel.style.width = savedW + 'px';
            void sidePanel.offsetWidth;
            if (!instant) sidePanel.style.transition = 'width ' + T;
            sidePanel.style.width = '0px';
        }
        try { localStorage.setItem('bartleby_side_open', open ? '1' : '0'); } catch(e) {}
    }

    toggleBtn.addEventListener('click', function() { setOpen(!isOpen); });

    leftResizer.classList.add('hidden');
    try {
        if (localStorage.getItem('bartleby_side_open') === '1') setOpen(true, true);
    } catch(e) {}
})();

// ── Left resizer ──────────────────────────────────────────────────────────────
(function() {
    var resizer   = document.getElementById('left-resizer');
    var sidePanel = document.getElementById('side-panel');
    var panels    = document.getElementById('main-panels');
    var resizing  = false;

    resizer.addEventListener('mousedown', function(e) {
        resizing = true;
        resizer.classList.add('active');
        document.body.style.cursor     = 'col-resize';
        document.body.style.userSelect = 'none';
        e.preventDefault();
    });
    document.addEventListener('mousemove', function(e) {
        if (!resizing) return;
        var rect = panels.getBoundingClientRect();
        var newW = Math.max(160, Math.min(480, e.clientX - rect.left));
        sidePanel.style.flex  = 'none';
        sidePanel.style.width = newW + 'px';
    });
    document.addEventListener('mouseup', function() {
        if (!resizing) return;
        resizing = false;
        resizer.classList.remove('active');
        document.body.style.cursor     = '';
        document.body.style.userSelect = '';
        var w = sidePanel.offsetWidth;
        sidePanel.style.flex  = '0 0 ' + w + 'px';
        sidePanel.style.width = '';
        try { localStorage.setItem('bartleby_side_width', String(w)); } catch(e) {}
    });
})();

// ── Side panel tabs ───────────────────────────────────────────────────────────
function switchSideTab(tab) {
    document.querySelectorAll('.side-tab-btn').forEach(function(b) {
        b.classList.toggle('active', b.dataset.stab === tab);
    });
    document.querySelectorAll('.side-tab-body').forEach(function(p) {
        p.classList.add('hidden');
    });
    var body = document.getElementById('sbody-' + tab);
    if (body) body.classList.remove('hidden');
    if (tab === 'preview') refreshPreview();
}
document.querySelectorAll('.side-tab-btn').forEach(function(btn) {
    btn.addEventListener('click', function() { switchSideTab(btn.dataset.stab); });
});

// ── Explorer tree (lazy loading) ──────────────────────────────────────────────
// Signature of the mounted-volume set, used to detect plug/unplug events.
var lastVolumeSig = null;
function volumeSig(vols) {
    return (vols || []).map(function(v) { return v.path; }).sort().join('\n');
}

function renderVolumes(vols) {
    var tree = document.getElementById('explorer-tree');
    tree.innerHTML = '';
    if (!vols || !vols.length) {
        tree.innerHTML = '<div class="tree-empty-msg">No volumes found.</div>';
        return;
    }
    vols.forEach(function(v) {
        tree.appendChild(buildTreeNode(v.path, v.name, true, v.media_type, true));
    });
}

async function loadVolumes() {
    var tree = document.getElementById('explorer-tree');
    tree.innerHTML = '<div class="tree-loading-msg">Loading volumes…</div>';
    var vols;
    try { vols = await invoke('list_volumes'); }
    catch(e) { tree.innerHTML = '<div class="tree-empty-msg">Could not list volumes.</div>'; return; }
    lastVolumeSig = volumeSig(vols);
    renderVolumes(vols);
}

// Re-fetch the volume list and rebuild the tree ONLY if a disk was plugged or
// removed. This keeps any folders the user has expanded intact during normal
// focus changes, while still surfacing drives mounted after Bartleby started.
async function refreshVolumesIfChanged() {
    if (!explorerPanelOpen()) return;
    var vols;
    try { vols = await invoke('list_volumes'); } catch(e) { return; }
    var sig = volumeSig(vols);
    if (sig === lastVolumeSig) return;
    lastVolumeSig = sig;
    renderVolumes(vols);
}

// The explorer panel is "open" when the side panel has width and its Explorer
// tab is the active one.
function explorerPanelOpen() {
    var panel = document.getElementById('side-panel');
    var body  = document.getElementById('sbody-explorer');
    return !!panel && panel.offsetWidth > 40 && !!body && !body.classList.contains('hidden');
}

// Manual refresh button + auto-refresh when the window regains focus.
(function setupVolumeRefresh() {
    var btn = document.getElementById('explorer-refresh');
    if (btn) btn.addEventListener('click', function() { loadVolumes(); });
    window.addEventListener('focus', function() { refreshVolumesIfChanged(); });
})();

// Maps a volume's media type to its explorer icon.
function volumeIcon(mediaType) {
    switch ((mediaType || '').toLowerCase()) {
        case 'hdd':   return 'ico-hard-drive';
        case 'ssd':   return 'ico-disk-ssd';
        case 'nvme':  return 'ico-disk-nvme';
        case 'sd':    return 'ico-card-sd';
        case 'flash': return 'ico-usb';
        default:      return 'ico-hard-drive';
    }
}

function buildTreeNode(path, name, isVolume, mediaType, isDir) {
    var node = document.createElement('div');
    node.className = 'tree-node';

    var row = document.createElement('div');
    row.className = 'tree-row' + (isVolume ? ' is-volume' : '') + (isDir ? '' : ' is-file');
    row.dataset.path = path;

    var chevron = document.createElement('span');
    chevron.className = 'tree-chevron' + (isDir ? '' : ' empty');
    chevron.innerHTML = '<svg><use href="#ico-chevron-right"/></svg>';

    var labelEl = document.createElement('span');
    labelEl.className = 'tree-label';
    labelEl.textContent = (isVolume && mediaType) ? (name + ' · ' + mediaType) : name;
    labelEl.title = path;

    var iconId = isVolume ? volumeIcon(mediaType) : (isDir ? 'ico-folder' : 'ico-file');
    row.appendChild(chevron);
    row.insertAdjacentHTML('beforeend',
        '<svg class="tree-icon"><use href="#' + iconId + '"/></svg>');
    row.appendChild(labelEl);

    var children = document.createElement('div');
    children.className = 'tree-children';

    node.appendChild(row);
    node.appendChild(children);

    // Files are leaf nodes: not expandable, no folder-action context menu.
    if (isDir) {
        var loaded = false;
        var toggle = function() {
            if (node.classList.contains('expanded')) {
                node.classList.remove('expanded');
                return;
            }
            node.classList.add('expanded');
            if (!loaded) {
                loaded = true;
                loadChildren(path, children, chevron);
            }
        };
        chevron.addEventListener('click', function(e) { e.stopPropagation(); toggle(); });
        row.addEventListener('click', function() { toggle(); });
        row.addEventListener('contextmenu', function(e) {
            e.preventDefault();
            showExplorerCtxMenu(e.clientX, e.clientY, {
                path: path, node: node, childrenEl: children, chevron: chevron
            });
        });
    }

    return node;
}

async function loadChildren(path, childrenEl, chevron) {
    chevron.classList.add('loading');
    childrenEl.innerHTML = '';
    var subs;
    try { subs = await invoke('list_dir', { path: path }); }
    catch(e) {
        chevron.classList.remove('loading');
        childrenEl.innerHTML = '<div class="tree-empty-msg">Cannot read folder.</div>';
        return;
    }
    chevron.classList.remove('loading');
    if (!subs || !subs.length) {
        childrenEl.innerHTML = '<div class="tree-empty-msg">empty</div>';
        return;
    }
    subs.forEach(function(s) {
        childrenEl.appendChild(buildTreeNode(s.path, s.name, false, '', s.is_dir));
    });
}

// ── Explorer context menu ─────────────────────────────────────────────────────
var explorerCtxTarget = null;

function showExplorerCtxMenu(x, y, target) {
    var menu = document.getElementById('explorer-ctx-menu');
    explorerCtxTarget = target;
    var hasJob = !!selectedJobCard;
    menu.querySelector('[data-act="set-source"]').disabled = !hasJob;
    menu.querySelector('[data-act="add-dest"]').disabled   = !hasJob;
    menu.classList.remove('hidden');
    menu.style.left = x + 'px';
    menu.style.top  = y + 'px';
    var r = menu.getBoundingClientRect();
    if (r.right  > window.innerWidth)  menu.style.left = (window.innerWidth  - r.width  - 6) + 'px';
    if (r.bottom > window.innerHeight) menu.style.top  = (window.innerHeight - r.height - 6) + 'px';
}

function hideExplorerCtxMenu() {
    document.getElementById('explorer-ctx-menu').classList.add('hidden');
}

document.addEventListener('click', function(e) {
    var menu = document.getElementById('explorer-ctx-menu');
    if (!menu.classList.contains('hidden') && !menu.contains(e.target)) hideExplorerCtxMenu();
});
document.addEventListener('contextmenu', function(e) {
    var menu = document.getElementById('explorer-ctx-menu');
    if (!menu.classList.contains('hidden') && !menu.contains(e.target) &&
        !e.target.closest('#explorer-tree')) {
        hideExplorerCtxMenu();
    }
});
document.addEventListener('keydown', function(e) {
    if (e.key === 'Escape') hideExplorerCtxMenu();
});

function ctxSetSource(path) {
    if (!selectedJobCard) return;
    var srcEl = selectedJobCard.querySelector('.job-src-input');
    if (!srcEl) return;
    srcEl.value = shortenPath(path);
    var vi = selectedJobCard.querySelector('.job-src-section .vol-info');
    if (vi) updateSrcInfo(vi, srcEl.value);
    refreshPreviewSoon();
}

function ctxAddDest(path) {
    if (!selectedJobCard) return;
    var destListEl = selectedJobCard.querySelector('.job-dest-list');
    if (!destListEl) return;
    var inputs = Array.from(destListEl.querySelectorAll('input[type="text"]'));
    var empty = inputs.find(function(i) { return i.value.trim() === ''; });
    if (empty) {
        empty.value = shortenPath(path);
        var di = empty.closest('.dest-item');
        if (di) updateVolInfo(di.querySelector('.vol-info'), empty.value);
    } else {
        addDestRowToJob(destListEl, shortenPath(path));
    }
    refreshPreviewSoon();
}

function ctxNewFolder(target) {
    target.node.classList.add('expanded');
    var childrenEl = target.childrenEl;
    var existing = childrenEl.querySelector('.tree-newfolder-row');
    if (existing) existing.remove();

    var row = document.createElement('div');
    row.className = 'tree-newfolder-row';
    var input = document.createElement('input');
    input.type = 'text';
    input.placeholder = 'New folder name…';
    input.spellcheck = false;
    row.appendChild(input);
    childrenEl.insertBefore(row, childrenEl.firstChild);
    input.focus();

    var done = false;
    function commit() {
        if (done) return;
        var name = input.value.trim();
        if (!name) { row.remove(); return; }
        done = true;
        invoke('create_folder', { path: joinPath(target.path, name) }).then(function() {
            loadChildren(target.path, childrenEl, target.chevron);
        }).catch(function(err) {
            done = false;
            alert('Could not create folder: ' + err);
            input.focus();
        });
    }
    input.addEventListener('keydown', function(e) {
        if (e.key === 'Enter')      { e.preventDefault(); commit(); }
        else if (e.key === 'Escape') { done = true; row.remove(); }
    });
    input.addEventListener('blur', function() { setTimeout(commit, 0); });
}

document.querySelectorAll('#explorer-ctx-menu .ctx-item').forEach(function(item) {
    item.addEventListener('click', function() {
        if (item.disabled || !explorerCtxTarget) { hideExplorerCtxMenu(); return; }
        var t = explorerCtxTarget;
        var act = item.dataset.act;
        if      (act === 'set-source') ctxSetSource(t.path);
        else if (act === 'add-dest')   ctxAddDest(t.path);
        else if (act === 'new-folder') ctxNewFolder(t);
        hideExplorerCtxMenu();
    });
});

// ── Job context menu (status) ──────────────────────────────────────────────────
var jobCtxTarget = null;

function showJobCtxMenu(x, y, card) {
    if (copyInProgress) return;   // don't let status be changed mid-queue
    var menu = document.getElementById('job-ctx-menu');
    jobCtxTarget = card;
    var status = card.dataset.status || 'idle';
    menu.querySelector('[data-act="reset"]').disabled = (status === 'idle');
    menu.classList.remove('hidden');
    menu.style.left = x + 'px';
    menu.style.top  = y + 'px';
    var r = menu.getBoundingClientRect();
    if (r.right  > window.innerWidth)  menu.style.left = (window.innerWidth  - r.width  - 6) + 'px';
    if (r.bottom > window.innerHeight) menu.style.top  = (window.innerHeight - r.height - 6) + 'px';
}

function hideJobCtxMenu() {
    document.getElementById('job-ctx-menu').classList.add('hidden');
    jobCtxTarget = null;
}

document.addEventListener('click', function(e) {
    var menu = document.getElementById('job-ctx-menu');
    if (!menu.classList.contains('hidden') && !menu.contains(e.target)) hideJobCtxMenu();
});
document.addEventListener('keydown', function(e) {
    if (e.key === 'Escape') hideJobCtxMenu();
});

document.querySelectorAll('#job-ctx-menu .ctx-item').forEach(function(item) {
    item.addEventListener('click', function() {
        if (item.disabled || !jobCtxTarget) { hideJobCtxMenu(); return; }
        var act = item.dataset.act;
        if (act === 'reset') setJobStatus(jobCtxTarget, 'idle');
        hideJobCtxMenu();
    });
});

// ── Per-job structure popup ───────────────────────────────────────────────────
(function() {
    var overlay     = document.getElementById('structure-overlay');
    var presetSel   = document.getElementById('structure-preset');
    var tmplInput   = document.getElementById('structure-template');
    var varDrops    = document.getElementById('structure-var-dropdowns');
    var previewEl   = document.getElementById('structure-preview');
    var okBtn       = document.getElementById('structure-popup-ok');
    var cancelBtn   = document.getElementById('structure-popup-cancel');
    var clearBtn    = document.getElementById('structure-clear');
    var activeCard  = null;

    function getJobvars() {
        var jv = {};
        varDrops.querySelectorAll('select').forEach(function(sel) {
            if (sel.value) jv[sel.dataset.varname] = sel.value;
        });
        return jv;
    }

    function refresh() {
        var res = resolveTemplate({ template: tmplInput.value, jobvars: getJobvars() });
        if (!res.path && !res.bad.length) {
            previewEl.textContent = '(no template — destination used as-is)';
        } else {
            previewEl.innerHTML = highlightBad('…/' + res.path + '/', res.bad);
        }
    }

    window.openStructurePopup = function(jobCard) {
        activeCard = jobCard;
        var presets = (currentSettings && currentSettings.folder_presets) || [];
        presetSel.innerHTML = '';
        var none = document.createElement('option');
        none.value = ''; none.textContent = '— Custom template —';
        presetSel.appendChild(none);
        presets.forEach(function(p) {
            var o = document.createElement('option');
            o.value = '#' + p.name; o.textContent = '#' + p.name;
            presetSel.appendChild(o);
        });
        tmplInput.value = jobCard.dataset.template || '';
        presetSel.value = presets.some(function(p) { return '#' + p.name === tmplInput.value; })
            ? tmplInput.value : '';

        // Build per-variable dropdowns from settings
        var savedJv = {};
        try { savedJv = JSON.parse(jobCard.dataset.jobvars || '{}'); } catch(e) {}
        varDrops.innerHTML = '';
        ((currentSettings && currentSettings.folder_variables) || []).forEach(function(fv) {
            if (!fv.values || !fv.values.length) return;
            var label = document.createElement('label');
            label.className = 'struct-field';
            var span = document.createElement('span');
            span.className = 'struct-field-label';
            span.textContent = '%' + fv.name;
            var sel = document.createElement('select');
            sel.className = 'modal-input';
            sel.dataset.varname = fv.name.toLowerCase();
            var ph = document.createElement('option');
            ph.value = ''; ph.textContent = '— ' + fv.name + ' —';
            sel.appendChild(ph);
            fv.values.forEach(function(val) {
                var o = document.createElement('option');
                o.value = val; o.textContent = val;
                sel.appendChild(o);
            });
            sel.value = savedJv[fv.name.toLowerCase()] || '';
            sel.addEventListener('change', refresh);
            label.appendChild(span);
            label.appendChild(sel);
            varDrops.appendChild(label);
        });

        refresh();
        overlay.classList.remove('hidden');
    };

    presetSel.addEventListener('change', function() {
        if (presetSel.value) tmplInput.value = presetSel.value;
        refresh();
    });
    tmplInput.addEventListener('input', function() {
        if (presetSel.value && tmplInput.value !== presetSel.value) presetSel.value = '';
        refresh();
    });

    okBtn.addEventListener('click', function() {
        if (!activeCard) return;
        var tmpl = tmplInput.value.trim();
        activeCard.dataset.template = tmpl;
        activeCard.dataset.jobvars  = JSON.stringify(getJobvars());
        var btn = activeCard.querySelector('.job-structure-btn');
        if (btn) btn.classList.toggle('has-structure', tmpl.length > 0);
        syncJobTemplateInput(activeCard);
        overlay.classList.add('hidden');
        activeCard = null;
        refreshPreviewSoon();
    });
    clearBtn.addEventListener('click', function() {
        tmplInput.value = ''; presetSel.value = '';
        varDrops.querySelectorAll('select').forEach(function(s) { s.value = ''; });
        refresh();
    });
    cancelBtn.addEventListener('click', function() {
        syncJobTemplateInput(activeCard);
        overlay.classList.add('hidden');
        activeCard = null;
    });
    overlay.addEventListener('click', function(e) {
        if (e.target === overlay) {
            syncJobTemplateInput(activeCard);
            overlay.classList.add('hidden');
            activeCard = null;
        }
    });
})();

// ── Per-job report settings popup ────────────────────────────────────────────
(function() {
    var overlay    = document.getElementById('report-settings-overlay');
    var csInput    = document.getElementById('rset-checksum-name');
    var rptInput   = document.getElementById('rset-report-name');
    var subInput   = document.getElementById('rset-report-subfolder');
    var okBtn      = document.getElementById('rset-ok');
    var cancelBtn  = document.getElementById('rset-cancel');
    var activeCard = null;

    window.openReportSettingsPopup = function(jobCard) {
        activeCard = jobCard;
        csInput.value  = jobCard.dataset.checksumName    || '';
        rptInput.value = jobCard.dataset.reportName      || '';
        subInput.value = jobCard.dataset.reportSubfolder || '';
        overlay.classList.remove('hidden');
        csInput.focus();
    };

    function commit() {
        if (!activeCard) return;
        activeCard.dataset.checksumName    = csInput.value.trim();
        activeCard.dataset.reportName      = rptInput.value.trim();
        activeCard.dataset.reportSubfolder = subInput.value.trim();
        var btn = activeCard.querySelector('.job-report-settings-btn');
        var hasCustom = csInput.value.trim() || rptInput.value.trim() || subInput.value.trim();
        if (btn) btn.classList.toggle('has-structure', !!hasCustom);
        overlay.classList.add('hidden');
        activeCard = null;
    }

    okBtn.addEventListener('click', commit);
    cancelBtn.addEventListener('click', function() {
        overlay.classList.add('hidden');
        activeCard = null;
    });
    overlay.addEventListener('click', function(e) {
        if (e.target === overlay) { overlay.classList.add('hidden'); activeCard = null; }
    });
    overlay.addEventListener('keydown', function(e) {
        if (e.key === 'Enter') commit();
        if (e.key === 'Escape') { overlay.classList.add('hidden'); activeCard = null; }
    });
})();

// ── Structure settings tab ────────────────────────────────────────────────────
function updateDateSample() {
    var fmt = document.getElementById('s-date-format').value || '%Y-%m-%d';
    document.getElementById('s-date-sample').textContent = 'Sample: ' + formatStrftime(fmt);
}

function addStructValueRow(container, val) {
    var row = document.createElement('div');
    row.className = 'struct-value-row';
    var input = document.createElement('input');
    input.type = 'text';
    input.className = 'struct-value-input';
    input.value = val || '';
    var rm = document.createElement('button');
    rm.className = 'icon-btn icon-btn-danger';
    rm.type = 'button';
    rm.title = 'Remove';
    rm.innerHTML = '<svg width="14" height="14"><use href="#ico-close"/></svg>';
    rm.addEventListener('click', function() { row.remove(); });
    row.appendChild(input);
    row.appendChild(rm);
    container.appendChild(row);
    return input;
}

function addStructPresetRow(container, preset) {
    var row = document.createElement('div');
    row.className = 'struct-preset-row';
    var nameInput = document.createElement('input');
    nameInput.type = 'text';
    nameInput.className = 'struct-preset-name';
    nameInput.placeholder = 'name';
    nameInput.value = (preset && preset.name) || '';
    var tmplInput = document.createElement('input');
    tmplInput.type = 'text';
    tmplInput.className = 'struct-preset-tmpl';
    tmplInput.placeholder = 'template, e.g. IMAGE/%date/%camera';
    tmplInput.spellcheck = false;
    tmplInput.value = (preset && preset.template) || '';
    var rm = document.createElement('button');
    rm.className = 'icon-btn icon-btn-danger';
    rm.type = 'button';
    rm.title = 'Remove';
    rm.innerHTML = '<svg width="14" height="14"><use href="#ico-close"/></svg>';
    rm.addEventListener('click', function() { row.remove(); });
    row.appendChild(nameInput);
    row.appendChild(tmplInput);
    row.appendChild(rm);
    container.appendChild(row);
}

function populateStructureFields() {
    if (!currentSettings) return;
    document.getElementById('s-date-format').value = currentSettings.folder_var_date_format || '%Y-%m-%d';
    document.getElementById('s-shoot-day').value   = currentSettings.folder_shoot_day || '';
    updateDateSample();
    var fvarList = document.getElementById('s-fvar-list');
    fvarList.innerHTML = '';
    (currentSettings.folder_variables || []).forEach(function(fv) { addFolderVarCard(fvarList, fv); });
    var presetList = document.getElementById('s-preset-list');
    presetList.innerHTML = '';
    (currentSettings.folder_presets || []).forEach(function(p) { addStructPresetRow(presetList, p); });
}

function addFolderVarCard(container, fvar) {
    var card = document.createElement('div');
    card.className = 'card modal-card folder-var-card';

    var header = document.createElement('div');
    header.className = 'folder-var-card-header';

    var prefix = document.createElement('span');
    prefix.className = 'folder-var-prefix';
    prefix.textContent = '%';

    var nameInp = document.createElement('input');
    nameInp.type = 'text';
    nameInp.className = 'modal-input fvar-name';
    nameInp.placeholder = 'variable name (e.g. cam, type, reel)';
    nameInp.spellcheck = false;
    nameInp.value = (fvar && fvar.name) || '';

    var rmCard = document.createElement('button');
    rmCard.className = 'icon-btn icon-btn-danger';
    rmCard.type = 'button';
    rmCard.title = 'Remove variable';
    rmCard.innerHTML = '<svg width="14" height="14"><use href="#ico-close"/></svg>';
    rmCard.addEventListener('click', function() { card.remove(); });

    header.appendChild(prefix);
    header.appendChild(nameInp);
    header.appendChild(rmCard);
    card.appendChild(header);

    var valList = document.createElement('div');
    valList.className = 'struct-value-list folder-var-values';
    ((fvar && fvar.values) || []).forEach(function(v) { addStructValueRow(valList, v); });
    card.appendChild(valList);

    var addValBtn = document.createElement('button');
    addValBtn.className = 'flat-btn';
    addValBtn.type = 'button';
    addValBtn.textContent = '+ Add value';
    addValBtn.addEventListener('click', function() { addStructValueRow(valList, '').focus(); });
    card.appendChild(addValBtn);

    container.appendChild(card);
    return nameInp;
}

function collectFolderVars() {
    var vars = [];
    document.querySelectorAll('#s-fvar-list .folder-var-card').forEach(function(card) {
        var name = card.querySelector('.fvar-name').value.trim();
        if (!name) return;
        var values = [];
        card.querySelectorAll('.struct-value-input').forEach(function(inp) {
            var v = inp.value.trim();
            if (v) values.push(v);
        });
        vars.push({ name: name, values: values });
    });
    return vars;
}

function collectStructPresets() {
    var presets = [];
    document.querySelectorAll('#s-preset-list .struct-preset-row').forEach(function(row) {
        var name = row.querySelector('.struct-preset-name').value.trim();
        var tmpl = row.querySelector('.struct-preset-tmpl').value.trim();
        if (name) presets.push({ name: name, template: tmpl });
    });
    return presets;
}

(function() {
    var dateFmt = document.getElementById('s-date-format');
    if (dateFmt) dateFmt.addEventListener('input', updateDateSample);

    var fvarAdd = document.getElementById('s-fvar-add');
    if (fvarAdd) fvarAdd.addEventListener('click', function() {
        addFolderVarCard(document.getElementById('s-fvar-list'), null).focus();
    });

    var presetAdd = document.getElementById('s-preset-add');
    if (presetAdd) presetAdd.addEventListener('click', function() {
        addStructPresetRow(document.getElementById('s-preset-list'), null);
    });

    var cancelBtn = document.getElementById('structure-cancel');
    if (cancelBtn) cancelBtn.addEventListener('click', function() {
        settingsOverlay.classList.add('hidden');
    });
    var saveBtn = document.getElementById('structure-save');
    if (saveBtn) saveBtn.addEventListener('click', async function() {
        if (!currentSettings) return;
        currentSettings.folder_var_date_format =
            document.getElementById('s-date-format').value.trim() || '%Y-%m-%d';
        currentSettings.folder_shoot_day  = document.getElementById('s-shoot-day').value.trim();
        currentSettings.folder_variables  = collectFolderVars();
        currentSettings.folder_presets    = collectStructPresets();
        jobsContainer.querySelectorAll('.job-group').forEach(function(card) {
            syncJobTemplateInput(card);
        });
        await persistSettings();
        settingsOverlay.classList.add('hidden');
        refreshPreviewSoon();
    });
})();

// ── Live structure preview tab ────────────────────────────────────────────────
var PREVIEW_STATUS = {
    will_create: { cls: 'st-create',     text: 'will be created' },
    empty:       { cls: 'st-empty',      text: 'exists — empty' },
    non_empty:   { cls: 'st-nonempty',   text: 'exists — NOT empty' },
    not_mounted: { cls: 'st-notmounted', text: 'volume not mounted' }
};

var previewTimer = null;
function refreshPreviewSoon() {
    clearTimeout(previewTimer);
    previewTimer = setTimeout(function() {
        var body = document.getElementById('sbody-preview');
        if (body && !body.classList.contains('hidden')) refreshPreview();
    }, 350);
}

async function refreshPreview() {
    var container = document.getElementById('preview-content');
    if (!container) return;
    var jobs = getJobs();
    if (!jobs.length) {
        container.innerHTML = '<div class="preview-empty-msg">No jobs.</div>';
        return;
    }

    var rows = [];
    jobs.forEach(function(job, ji) {
        var res = resolveTemplate(job);
        job.dsts.forEach(function(root) {
            var full = res.path ? joinPath(root, res.path) : root;
            if (job.copyAsSubfolder && job.src) full = joinPath(full, baseName(job.src));
            rows.push({ jobIdx: ji, root: root, full: full, res: res });
        });
    });
    if (!rows.length) {
        container.innerHTML = '<div class="preview-empty-msg">No destinations set.</div>';
        return;
    }

    var statuses;
    try {
        statuses = await invoke('dest_path_status', {
            items: rows.map(function(r) { return { root: r.root, full: r.full }; })
        });
    } catch(e) {
        statuses = rows.map(function() { return 'will_create'; });
    }
    rows.forEach(function(r, i) { r.status = statuses[i] || 'will_create'; });

    var multi = jobs.length > 1;
    container.innerHTML = '';
    jobs.forEach(function(job, ji) {
        var section = document.createElement('div');
        section.className = 'preview-job';
        var title = document.createElement('div');
        title.className = 'preview-job-title';
        title.textContent = job.name || (multi ? 'Job ' + (ji + 1) : 'Job');
        section.appendChild(title);

        var jobRows = rows.filter(function(r) { return r.jobIdx === ji; });
        if (!jobRows.length) {
            var none = document.createElement('div');
            none.className = 'preview-empty-msg';
            none.textContent = 'No destinations.';
            section.appendChild(none);
        }
        jobRows.forEach(function(r) {
            var row = document.createElement('div');
            row.className = 'preview-dest';
            var pathEl = document.createElement('div');
            pathEl.className = 'preview-path';
            var shown = shortenPath(r.full);
            if (r.res.bad && r.res.bad.length) pathEl.innerHTML = highlightBad(shown, r.res.bad);
            else pathEl.textContent = shown;
            var st = PREVIEW_STATUS[r.status] || PREVIEW_STATUS.will_create;
            var statusEl = document.createElement('div');
            statusEl.className = 'preview-status ' + st.cls;
            statusEl.textContent = st.text;
            row.appendChild(pathEl);
            row.appendChild(statusEl);
            section.appendChild(row);
        });
        container.appendChild(section);
    });
}

(function() {
    var refreshBtn = document.getElementById('preview-refresh');
    if (refreshBtn) refreshBtn.addEventListener('click', refreshPreview);
})();

// ── Initialisation ────────────────────────────────────────────────────────────

document.addEventListener('DOMContentLoaded', async function() {
    try { homeDir = await invoke('get_home_dir'); } catch(e) {}
    invoke('get_app_version').then(function(v) {
        var el = document.getElementById('about-version');
        if (el) el.textContent = v;
    }).catch(function() {});
    await loadSettings();
    addJob(); // first (empty) job card
});
