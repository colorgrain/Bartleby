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
        var result = await window.__TAURI__.dialog.open({
            directory: true,
            multiple:  false
        });
        return result;
    } catch(e) {
        console.error('pickFolder error:', e);
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
function formatBytes(b) {
    if (b <= 0) return '0 o';
    if (b < 1e12) return (b / 1e9).toFixed(1) + ' Go';
    return (b / 1e12).toFixed(2) + ' To';
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
function setCopyInProgress(active) {
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
                syncWindowBackground();
                invoke('set_window_theme', { theme: isDark ? 'dark' : 'light' }).catch(function() {});
                document.querySelectorAll('.appearance-btn[data-theme]').forEach(function(btn) {
                    btn.classList.toggle('appearance-btn-active', btn.dataset.theme === 'default');
                });
            } catch(e) {
                document.body.className = 'theme-default';
                syncWindowBackground();
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

/* Sync the Tauri WebView background colour with the current skin/theme --bg.
   Called after every theme or skin change so that newly exposed areas during
   window resize show the correct colour instead of the stale initial value. */
async function syncWindowBackground() {
    var bg = getComputedStyle(document.body).getPropertyValue('--bg').trim();
    if (!bg || bg.charAt(0) !== '#' || bg.length < 7) return;
    document.documentElement.style.backgroundColor = bg;
    try {
        var r = parseInt(bg.slice(1, 3), 16);
        var g = parseInt(bg.slice(3, 5), 16);
        var b = parseInt(bg.slice(5, 7), 16);
        await invoke('set_webview_bg', { r: r, g: g, b: b });
    } catch(e) {}
}

function applyTheme(theme, save) {
    document.body.className = 'theme-' + theme;
    syncWindowBackground();
    invoke('set_window_theme', { theme: theme === 'dark' ? 'dark' : 'light' }).catch(function() {});
    if (currentSettings) currentSettings.theme = theme;
    document.querySelectorAll('.appearance-btn[data-theme]').forEach(function(btn) {
        btn.classList.toggle('appearance-btn-active', btn.dataset.theme === theme);
    });
    if (save) persistSettings();
}

function applySkin(skin, save) {
    var link = document.getElementById('theme-link');
    if (link) {
        link.href = 'themes/' + skin + '.css';
        link.onload = syncWindowBackground;
    }
    if (currentSettings) currentSettings.skin = skin;
    document.querySelectorAll('.appearance-btn[data-skin]').forEach(function(btn) {
        btn.classList.toggle('appearance-btn-active', btn.dataset.skin === skin);
    });
    if (save) persistSettings();
}

// ── Settings modal ────────────────────────────────────────────────────────────

// Hamburger → open settings (Appearance tab active by default).
menuBtn.addEventListener('click', function() {
    if (currentSettings) populateReportFields();
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

function addJob() {
    var jobCard = document.createElement('section');
    jobCard.className = 'group job-group';

    // ── Job header ────────────────────────────────────────────────────────────
    var headerRow = document.createElement('div');
    headerRow.className = 'job-header-row';

    var label = document.createElement('label');
    label.className = 'group-label job-label';

    var removeJobBtn = document.createElement('button');
    removeJobBtn.className = 'icon-btn icon-btn-danger job-remove-btn';
    removeJobBtn.title = 'Remove this job';
    removeJobBtn.innerHTML = '<svg width="14" height="14"><use href="#ico-close"/></svg>';
    removeJobBtn.style.display = 'none';
    removeJobBtn.addEventListener('click', function() {
        jobCard.remove();
        renumberJobs();
    });

    headerRow.appendChild(label);
    headerRow.appendChild(removeJobBtn);
    jobCard.appendChild(headerRow);

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
    makeVolInfoWatcher(srcInputEl, srcVolInfo);

    srcBrowseBtn.addEventListener('click', async function() {
        var p = await pickFolder();
        if (p) { srcInputEl.value = shortenPath(p); updateVolInfo(srcVolInfo, srcInputEl.value); }
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

    jobOptsRow.appendChild(makeJobToggle('job-chk-csv',  '.CSV',  defaultGenCsv));
    jobOptsRow.appendChild(makeJobToggle('job-chk-pdf',  '.PDF',  defaultGenPdf));
    jobOptsRow.appendChild(makeJobToggle('job-chk-html', '.HTML', defaultGenHtml));

    var mhlLbl = makeJobToggle('job-chk-mhl', '.MHL', defaultGenMhl);
    var mhlChkInit = mhlLbl.querySelector('.job-chk-mhl');
    if (hashSel.value === 'none' || hashSel.value === 'size') {
        mhlChkInit.disabled = true;
        mhlLbl.classList.add('toggle-disabled');
    }
    jobOptsRow.appendChild(mhlLbl);

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
    jobOptsRow.appendChild(commentBtn);

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
    var jobProgressTextEl = document.createElement('span');
    jobProgressTextEl.className = 'job-progress-text';
    jobProgress.appendChild(jobProgressTrack);
    jobProgress.appendChild(jobProgressTextEl);
    jobCard.appendChild(jobProgress);

    jobsContainer.appendChild(jobCard);
    addDestRowToJob(destListEl);
    renumberJobs();
}

function renumberJobs() {
    var cards = jobsContainer.querySelectorAll('.job-group');
    var count = cards.length;
    cards.forEach(function(card, idx) {
        var lbl = card.querySelector('.job-label');
        if (lbl) lbl.textContent = count > 1 ? 'Job ' + (idx + 1) : 'Job';
        var btn = card.querySelector('.job-remove-btn');
        if (btn) btn.style.display = count > 1 ? '' : 'none';
    });
}

// Returns [{src, dsts, copyAsSubfolder, hashAlgo, genCsv, genPdf, genHtml, genMhl, comment, location}]
function getJobs() {
    var result = [];
    jobsContainer.querySelectorAll('.job-group').forEach(function(card) {
        var srcEl       = card.querySelector('.job-src-input');
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
            copyAsSubfolder: subfolderEl ? subfolderEl.checked : false,
            hashAlgo:        hashSelEl   ? hashSelEl.value     : 'none',
            genCsv:          csvEl       ? csvEl.checked       : false,
            genPdf:          pdfEl       ? pdfEl.checked       : false,
            genHtml:         htmlEl      ? htmlEl.checked      : false,
            genMhl:          mhlEl       ? (mhlEl.checked && !mhlEl.disabled) : false,
            comment:         card.dataset.comment     || '',
            mhl_comment:     card.dataset.mhl_comment || '',
            location:        card.dataset.location    || '',
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
            var fill = activeCard.querySelector('.job-progress-fill');
            var text = activeCard.querySelector('.job-progress-text');
            if (fill) fill.style.width = Math.round(event.payload.fraction * 100) + '%';
            if (text) text.textContent = event.payload.label;
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
                    open_dest:         false // handled at the end of launchCopy()
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
    var jobs  = getJobs();
    var multi = jobs.length > 1;

    for (var i = 0; i < jobs.length; i++) {
        var prefix = multi ? 'Job ' + (i + 1) + ': p' : 'P';
        if (!jobs[i].src) {
            alert(prefix + 'lease choose a source directory.');
            return;
        }
        if (!jobs[i].dsts.length) {
            alert(prefix + 'lease add at least one destination.');
            return;
        }
    }

    setCopyInProgress(true);
    currentJobIndex = -1;
    statusLabel.textContent = '';
    statusLabel.className   = '';
    logView.textContent     = '';

    jobsContainer.querySelectorAll('.job-label').forEach(function(lbl) {
        lbl.classList.remove('job-done');
    });
    jobsContainer.querySelectorAll('.job-progress').forEach(function(p) {
        p.classList.add('hidden');
        var f = p.querySelector('.job-progress-fill');
        var t = p.querySelector('.job-progress-text');
        if (f) {
            f.style.transition = 'none';
            f.style.width = '0%';
            f.classList.remove('job-progress-done', 'job-progress-error');
            void f.offsetWidth;
            f.style.transition = '';
        }
        if (t) t.textContent = '';
    });

    await registerListeners();

    var allOk       = true;
    var lastSummary = '';

    for (var i = 0; i < jobs.length; i++) {
        var job = jobs[i];
        var jobCards  = jobsContainer.querySelectorAll('.job-group');
        var jobProgEl = jobCards[i] ? jobCards[i].querySelector('.job-progress') : null;

        currentJobIndex = i;

        if (jobProgEl) {
            jobProgEl.classList.remove('hidden');
            jobProgEl.querySelector('.job-progress-fill').style.width = '0%';
            jobProgEl.querySelector('.job-progress-fill').classList.remove('job-progress-error');
            jobProgEl.querySelector('.job-progress-text').textContent = 'Starting…';
        }

        if (multi) {
            currentJobPrefix = 'Job ' + (i + 1) + '/' + jobs.length;
            logView.textContent += '\n── ' + currentJobPrefix + ' — ' + job.src + '\n';
            logView.scrollTop = logView.scrollHeight;
        }

        var result = await runJob(job);

        if (jobCards[i]) {
            var doneLbl = jobCards[i].querySelector('.job-label');
            if (doneLbl && result.ok) doneLbl.classList.add('job-done');
        }
        if (jobProgEl) {
            var fill = jobProgEl.querySelector('.job-progress-fill');
            fill.style.width = '100%';
            if (result.ok) fill.classList.add('job-progress-done');
            else           fill.classList.add('job-progress-error');
            jobProgEl.querySelector('.job-progress-text').textContent = result.summary || (result.ok ? 'Done' : 'Error');
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
        ? (allOk ? 'All ' + jobs.length + ' jobs completed successfully.' : 'Queue finished with errors — check log.')
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
        jobs.forEach(function(j) { allDsts = allDsts.concat(j.dsts); });
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
    cancelBtn.disabled = true;
    pauseBtn.disabled  = true;
    userCancelledQueue = true;
    try { await invoke('cancel_copy'); } catch(e) {}
});

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
                items.join('\n') + '\n\nContinue anyway?';
            addPromptBtn(btnRow, 'Cancel',   'cancel',   false, false, resolve);
            addPromptBtn(btnRow, 'Continue', 'continue', true,  false, resolve);
        } else {
            title.textContent = 'File conflicts detected';

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

            addPromptBtn(btnRow, 'Cancel',                   'cancel',   false, false, resolve);
            addPromptBtn(btnRow, 'Skip — size & date match', 'skip',     true,  false, resolve);
            addPromptBtn(btnRow, 'Replace all',              'continue', true,  true,  resolve);
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
                    if (srcVi) updateVolInfo(srcVi, srcInputEl.value);
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
        var maxW = panels.offsetWidth - 6 - 672;   /* leave ≥672px for left panel (card min-width + padding) */
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

// ── Initialisation ────────────────────────────────────────────────────────────
document.addEventListener('DOMContentLoaded', async function() {
    try { homeDir = await invoke('get_home_dir'); } catch(e) {}
    await loadSettings();
    addJob(); // first (empty) job card
});
