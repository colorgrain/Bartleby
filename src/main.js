/**
 * main.js — Bartleby frontend logic
 *
 * This file is the single JavaScript file that drives the entire Bartleby UI.
 * It runs inside Tauri's embedded WebView (a system WebKit/WebView2 instance).
 *
 * ── Architecture overview ────────────────────────────────────────────────────
 *
 * Bartleby uses Tauri's IPC bridge to communicate between JS and Rust:
 *
 *   JS → Rust : invoke("command_name", args)
 *               Calls a Rust function tagged #[tauri::command].
 *               Returns a Promise that resolves with the Rust return value.
 *
 *   Rust → JS : listen("event-name", handler)
 *               Subscribes to events emitted by Rust via win.emit(…).
 *               Used for streaming progress updates during a copy operation.
 *
 * ── Data flow during a copy ──────────────────────────────────────────────────
 *
 *   1. User clicks "Copy"
 *   2. launchCopy() validates inputs, derives verify from checkbox states,
 *      registers Tauri event listeners
 *   3. invoke("start_copy", args) — Rust starts the background copy thread
 *      and returns immediately (the Promise resolves with no value)
 *   4. Rust emits "copy-progress" events → progress bar updates
 *   5. Rust emits "copy-log" events per file → log panel grows
 *   6. If a prompt is needed, Rust emits "copy-prompt" → showPrompt() modal
 *   7. User responds → invoke("prompt_reply") → Rust unblocks
 *   8. Rust emits "copy-done" → status label shown, button re-enabled
 *
 * ── Architecture notes ───────────────────────────────────────────────────────
 *
 *   1. Folder picker → window.__TAURI__.dialog.open() on the JS side.
 *      This avoids crashes caused by UI access from a Rust worker thread.
 *   2. System theme → class="theme-default" is already set on <body> in HTML.
 *      JS only changes it when the user explicitly selects Light or Dark mode.
 *   3. Icons: SVG symbols are inlined in index.html for instant rendering,
 *      referenced via <use href="#ico-..."> without additional HTTP requests.
 *
 * ── Two-level appearance system ──────────────────────────────────────────────
 *
 *   "skin"  (applySkin)  — which CSS colour palette to use.
 *                          Controls which file is loaded via <link id="theme-link">.
 *                          Values: "mint-y-aqua" | "macos" | "windows11" | "adwaita"
 *                          Stored in settings.json as "skin".
 *
 *   "theme" (applyTheme) — light / dark / default within that palette.
 *                          Controls body.className: "theme-light" | "theme-dark" | "theme-default"
 *                          Stored in settings.json as "theme".
 *
 * ── Why var and not let/const ────────────────────────────────────────────────
 *
 * `var` is used throughout for compatibility with older WebKit versions that
 * Tauri might target on some Linux systems. In a module-based setup, const/let
 * would be preferred, but `var` is safe for a single script file with no imports.
 */

// ── Tauri IPC helpers ─────────────────────────────────────────────────────────
//
// Thin wrappers so call sites don't need to repeat window.__TAURI__.core.invoke
// and window.__TAURI__.event.listen everywhere.
function invoke(cmd, args) {
    return window.__TAURI__.core.invoke(cmd, args);
}
function listen(event, handler) {
    return window.__TAURI__.event.listen(event, handler);
}

// Opens a native folder picker using the Tauri dialog plugin (JS-side call).
// This is the correct Tauri v2 approach — calling dialog from the JS context
// avoids blocking the Rust main thread and prevents WebView-from-worker crashes.
async function pickFolder() {
    try {
        // window.__TAURI__.dialog.open() with directory:true opens a folder picker.
        // Returns an absolute path string, or null if the user cancelled.
        var result = await window.__TAURI__.dialog.open({
            directory: true,
            multiple:  false
        });
        return result; // string path if folder selected, null if cancelled
    } catch(e) {
        console.error('pickFolder error:', e);
        return null;
    }
}

// ── DOM references ────────────────────────────────────────────────────────────
//
// All DOM elements are looked up once at script load time and stored in
// module-level variables. This avoids repeated getElementById() calls in
// event handlers and ensures we fail fast if an expected element is missing.
var jobsContainer = document.getElementById('jobs-container');
var addJobBtn     = document.getElementById('add-job-btn');
var progressFill  = document.getElementById('progress-fill');
var progressText = document.getElementById('progress-text');
var statusLabel  = document.getElementById('status-label');
var logView      = document.getElementById('log-view');
// Hash algorithm dropdown. Values: "none" | "md5" | "xxh3".
// Drives both verify mode and which checksum file is written.
var hashSelect   = document.getElementById('hash-select');
var chkCsv       = document.getElementById('chk-csv');
var chkPdf       = document.getElementById('chk-pdf');
var chkHtml      = document.getElementById('chk-html');
var gearBtn      = document.getElementById('gear-btn');
var chkOpen      = document.getElementById('chk-open');
// Single "Copy" action button. Disabled during active copy to prevent double-click.
// verify mode is derived automatically: verify = chkMd5.checked || chkXxh.checked.
var copyBtn      = document.getElementById('copy-btn');
var menuBtn      = document.getElementById('menu-btn');
var menuPopup    = document.getElementById('menu-popup');

// ── Settings ──────────────────────────────────────────────────────────────────
//
// currentSettings holds the in-memory copy of the settings.json state.
// It is loaded from Rust on DOMContentLoaded via invoke('get_settings').
// Any mutation (checkbox toggle, settings dialog save) calls persistSettings()
// to write the change back to disk via invoke('save_settings').
//
// @type {object|null}
var currentSettings = null;

// Loads settings from Rust backend and applies them to the UI.
// Called once on DOMContentLoaded. If get_settings fails, the UI starts
// with all checkboxes unchecked (their HTML default state).
async function loadSettings() {
    try {
        currentSettings = await invoke('get_settings');

        // Restore hash algorithm selection. Fall back to 'md5' for old settings files
        // that stored gen_md5/gen_xxh booleans instead of hash_algo.
        if (hashSelect) {
            var algo = currentSettings.hash_algo;
            if (!algo) algo = currentSettings.gen_xxh ? 'xxh3' : (currentSettings.gen_md5 ? 'md5' : 'none');
            hashSelect.value = algo;
        }
        chkCsv.checked  = currentSettings.gen_csv;
        chkPdf.checked  = currentSettings.gen_pdf;
        chkHtml.checked = currentSettings.gen_html || false;
        chkOpen.checked = currentSettings.open_dest || false;

        // Apply saved skin (CSS palette file).
        var savedSkin = currentSettings.skin || 'mint-y-aqua';
        applySkin(savedSkin, false); // false = don't re-save to disk

        // Apply saved theme (light / dark / default).
        var saved = currentSettings.theme || 'default';

        if (saved === 'default') {
            // Ask the Rust backend for the real OS theme (light or dark).
            // prefers-color-scheme is unreliable inside Tauri's WebView on Linux
            // because GTK and WebKit do not always agree on the system colour scheme.
            try {
                var isDark = await invoke('is_system_dark_mode');
                // Apply dark or light visually, but keep 'default' in currentSettings
                // so the theme menu continues to show "Follow system" as the active option.
                document.body.className = isDark ? 'theme-dark' : 'theme-light';
                // Mark "Follow system" as active in the theme menu.
                document.querySelectorAll('.menu-item[data-theme]').forEach(function(btn) {
                    btn.classList.toggle('menu-item-active', btn.dataset.theme === 'default');
                });
            } catch(e) {
                // Fallback: rely on CSS prefers-color-scheme media query.
                document.body.className = 'theme-default';
            }
        } else {
            applyTheme(saved, false);
        }

        // Load the logo path into the settings modal input.
        var logoEl = document.getElementById('s-logo');
        if (logoEl) logoEl.value = currentSettings.logo_path || '';

    } catch(e) {
        console.error('get_settings:', e);
    }
}

// Persists currentSettings to disk via the Rust save_settings command.
// Called after any settings mutation (checkbox change, dialog save, logo pick).
async function persistSettings() {
    try {
        await invoke('save_settings', { newSettings: currentSettings });
    } catch(e) {
        console.error('save_settings:', e);
    }
}

// ── Theme ─────────────────────────────────────────────────────────────────────
//
// applyTheme sets body.className to "theme-{theme}" which activates the
// corresponding CSS block in the active skin file.
// If save=true, the change is persisted to settings.json.
function applyTheme(theme, save) {
    document.body.className = 'theme-' + theme;
    if (currentSettings) currentSettings.theme = theme;
    // Update the check mark in the theme menu to reflect the new selection.
    document.querySelectorAll('.menu-item[data-theme]').forEach(function(btn) {
        btn.classList.toggle('menu-item-active', btn.dataset.theme === theme);
    });
    if (save) persistSettings();
}

// ── Skin ──────────────────────────────────────────────────────────────────────
//
// applySkin swaps the CSS palette file loaded by <link id="theme-link">.
// Each skin is a self-contained CSS file at src/themes/{skin}.css that
// defines all CSS custom properties (--bg, --accent, --radius, etc.).
// The theme (light/dark) is orthogonal: it selects a block within that file.
function applySkin(skin, save) {
    var link = document.getElementById('theme-link');
    if (link) link.href = 'themes/' + skin + '.css';
    if (currentSettings) currentSettings.skin = skin;
    // Update the check mark in the skin submenu.
    document.querySelectorAll('.menu-item[data-skin]').forEach(function(btn) {
        btn.classList.toggle('menu-item-active', btn.dataset.skin === skin);
    });
    if (save) persistSettings();
}

// ── Hamburger menu ────────────────────────────────────────────────────────────
//
// The hamburger button toggles the popup menu visibility.
// Clicking anywhere outside the menu closes it (document click listener).
// e.stopPropagation() on the button prevents the document click from
// immediately closing the menu that was just opened.
menuBtn.addEventListener('click', function(e) {
    e.stopPropagation();
    menuPopup.classList.toggle('hidden');
});
document.addEventListener('click', function() {
    menuPopup.classList.add('hidden');
});

// Wire theme menu items — each button has data-theme="default|light|dark".
document.querySelectorAll('.menu-item[data-theme]').forEach(function(btn) {
    btn.addEventListener('click', function() {
        applyTheme(btn.dataset.theme, true);
    });
});

// Wire skin menu items — each button has data-skin="mint-y-aqua|macos|…".
document.querySelectorAll('.menu-item[data-skin]').forEach(function(btn) {
    btn.addEventListener('click', function() {
        applySkin(btn.dataset.skin, true);
    });
});

document.getElementById('about-menu-item').addEventListener('click', function() {
    document.getElementById('about-overlay').classList.remove('hidden');
});

// ── About modal ───────────────────────────────────────────────────────────────
document.getElementById('about-close').addEventListener('click', function() {
    document.getElementById('about-overlay').classList.add('hidden');
});
document.getElementById('about-overlay').addEventListener('click', function(e) {
    if (e.target === e.currentTarget)
        document.getElementById('about-overlay').classList.add('hidden');
});

// ── Job queue management ──────────────────────────────────────────────────────
//
// The UI groups source + destinations into "jobs". Each job is an independent
// transfer unit. Jobs share the same hash/report settings and run sequentially
// when the user clicks "Copy".
//
// addJob()          — creates a full job card and appends it to #jobs-container.
// renumberJobs()    — updates "Job 1 / Job 2 / …" labels after add or remove.
// getJobs()         — returns [{src, dsts}] for all job cards.
// addDestRowToJob() — appends one destination row to a job's destination list.
//
// currentJobPrefix is set to "Job N/M" during multi-job runs so the progress
// text shows which job is active.
var currentJobPrefix = '';

function addDestRowToJob(destListEl, initialValue) {
    var row = document.createElement('div');
    row.className = 'dest-row';

    var input = document.createElement('input');
    input.type = 'text';
    input.placeholder = 'Destination directory… (or drag & drop)';
    if (initialValue) input.value = initialValue;

    var browseBtn = document.createElement('button');
    browseBtn.className = 'icon-btn';
    browseBtn.title = 'Browse…';
    browseBtn.innerHTML = '<svg width="18" height="18"><use href="#ico-folder"/></svg>';
    browseBtn.addEventListener('click', async function() {
        var p = await pickFolder();
        if (p) input.value = p;
    });

    var removeBtn = document.createElement('button');
    removeBtn.className = 'icon-btn icon-btn-danger';
    removeBtn.title = 'Remove';
    removeBtn.innerHTML = '<svg width="16" height="16"><use href="#ico-close"/></svg>';
    removeBtn.addEventListener('click', function() { row.remove(); });

    row.appendChild(input);
    row.appendChild(browseBtn);
    row.appendChild(removeBtn);
    destListEl.appendChild(row);
}

function addJob() {
    var jobCard = document.createElement('section');
    jobCard.className = 'group job-group';

    // ── Job header: "Job N" label + remove button ─────────────────────────────
    var headerRow = document.createElement('div');
    headerRow.className = 'job-header-row';

    var label = document.createElement('label');
    label.className = 'group-label job-label';

    var removeJobBtn = document.createElement('button');
    removeJobBtn.className = 'icon-btn icon-btn-danger job-remove-btn';
    removeJobBtn.title = 'Remove this job';
    removeJobBtn.innerHTML = '<svg width="14" height="14"><use href="#ico-close"/></svg>';
    removeJobBtn.style.display = 'none'; // hidden until a 2nd job is added
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

    // Source sub-section
    var srcLabel = document.createElement('div');
    srcLabel.className = 'job-subsection-label';
    srcLabel.textContent = 'Source';
    card.appendChild(srcLabel);

    var srcRow = document.createElement('div');
    srcRow.className = 'row';

    var srcInputEl = document.createElement('input');
    srcInputEl.type = 'text';
    srcInputEl.className = 'job-src-input';
    srcInputEl.placeholder = 'Source directory… (or drag & drop)';

    var srcBrowseBtn = document.createElement('button');
    srcBrowseBtn.className = 'icon-btn';
    srcBrowseBtn.title = 'Browse…';
    srcBrowseBtn.innerHTML = '<svg width="18" height="18"><use href="#ico-folder"/></svg>';
    srcBrowseBtn.addEventListener('click', async function() {
        var p = await pickFolder();
        if (p) srcInputEl.value = p;
    });

    srcRow.appendChild(srcInputEl);
    srcRow.appendChild(srcBrowseBtn);
    card.appendChild(srcRow);

    // Visual separator between source and destinations
    var sep = document.createElement('hr');
    sep.className = 'dest-sep';
    sep.style.margin = '8px 0';
    card.appendChild(sep);

    // Destinations sub-section
    var destSectionLabel = document.createElement('div');
    destSectionLabel.className = 'job-subsection-label';
    destSectionLabel.textContent = 'Destinations';
    card.appendChild(destSectionLabel);

    var destListEl = document.createElement('div');
    destListEl.className = 'job-dest-list';
    card.appendChild(destListEl);

    var addDestBtn = document.createElement('button');
    addDestBtn.className = 'flat-btn';
    addDestBtn.textContent = '+ Add a destination';
    addDestBtn.addEventListener('click', function() { addDestRowToJob(destListEl); });
    card.appendChild(addDestBtn);

    jobCard.appendChild(card);
    jobsContainer.appendChild(jobCard);

    // Start each new job with one empty destination row
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

// Returns an array of { src, dsts } objects — one entry per job card.
// Empty source/destination strings are filtered out (dsts only).
function getJobs() {
    var result = [];
    jobsContainer.querySelectorAll('.job-group').forEach(function(card) {
        var srcEl = card.querySelector('.job-src-input');
        var src   = srcEl ? srcEl.value.trim() : '';
        var dsts  = Array.from(card.querySelectorAll('.job-dest-list input[type="text"]'))
            .map(function(i) { return i.value.trim(); })
            .filter(function(v) { return v.length > 0; });
        result.push({ src: src, dsts: dsts });
    });
    return result;
}

addJobBtn.addEventListener('click', function() { addJob(); });

// ── Checkboxes ────────────────────────────────────────────────────────────────
//
// Each checkbox persists its state to settings.json immediately on change.
// This ensures the user's preferred combination is restored on the next launch.
if (hashSelect) {
    hashSelect.addEventListener('change', async function() {
        if (currentSettings) { currentSettings.hash_algo = hashSelect.value; await persistSettings(); }
    });
}
chkCsv.addEventListener('change', async function() {
    if (currentSettings) { currentSettings.gen_csv = chkCsv.checked; await persistSettings(); }
});
chkPdf.addEventListener('change', async function() {
    if (currentSettings) { currentSettings.gen_pdf = chkPdf.checked; await persistSettings(); }
});
chkHtml.addEventListener('change', async function() {
    if (currentSettings) { currentSettings.gen_html = chkHtml.checked; await persistSettings(); }
});
chkOpen.addEventListener('change', async function() {
    if (currentSettings) { currentSettings.open_dest = chkOpen.checked; await persistSettings(); }
});

// ── Settings modal ────────────────────────────────────────────────────────────
var settingsOverlay = document.getElementById('settings-overlay');

// Opens the Settings modal and populates all fields from currentSettings.
gearBtn.addEventListener('click', function() {
    if (!currentSettings) return;
    document.getElementById('s-company').value = currentSettings.company       || '';
    document.getElementById('s-contact').value = currentSettings.contact_name  || '';
    document.getElementById('s-email').value   = currentSettings.email         || '';
    document.getElementById('s-phone').value   = currentSettings.phone         || '';
    document.getElementById('s-project').value = currentSettings.project_title || '';
    // Load the current logo path into the read-only text input.
    var logoEl = document.getElementById('s-logo');
    if (logoEl) logoEl.value = currentSettings.logo_path || '';
    // Load accent colours — fall back to Bartleby defaults if not yet set.
    // <input type="color"> expects a 6-digit hex string with leading "#".
    var a1 = document.getElementById('s-accent1');
    var a2 = document.getElementById('s-accent2');
    if (a1) a1.value = currentSettings.accent_color_1 || '#1F9EDE';
    if (a2) a2.value = currentSettings.accent_color_2 || '#99C7DE';
    // Restore column visibility checkboxes from settings.
    document.querySelectorAll('[data-col]').forEach(function(cb) {
        cb.checked = !!currentSettings[cb.dataset.col];
    });
    settingsOverlay.classList.remove('hidden');
});

// Logo picker — Browse button.
// Opens a native file picker filtered to JPEG and PNG image files.
// The selected path is stored in currentSettings.logo_path and displayed
// in the read-only text input. Saved immediately so it persists on modal close.
var logoBtn = document.getElementById('s-logo-btn');
if (logoBtn) {
    logoBtn.addEventListener('click', async function() {
        try {
            var path = await window.__TAURI__.dialog.open({
                // Restrict to JPEG and PNG — the two formats supported by the PDF logo renderer.
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

// Logo picker — Clear button.
// Removes the logo path from settings and clears the display field.
var logoClear = document.getElementById('s-logo-clear');
if (logoClear) {
    logoClear.addEventListener('click', async function() {
        if (currentSettings) currentSettings.logo_path = '';
        var logoEl = document.getElementById('s-logo');
        if (logoEl) logoEl.value = '';
        await invoke('save_settings', { newSettings: currentSettings });
    });
}

// ── Accent colour pickers — live save on change ──────────────────────────────
// Saving immediately on `input` (not just on dialog Save) means the colour
// is persisted even if the user closes the dialog without clicking Save.
// `input` fires continuously while the colour picker is open (on most browsers).
// `change` fires once when the picker is closed — we use both for robustness.
['s-accent1', 's-accent2'].forEach(function(id) {
    var el = document.getElementById(id);
    if (!el) return;
    el.addEventListener('change', async function() {
        if (!currentSettings) return;
        var a1 = document.getElementById('s-accent1');
        var a2 = document.getElementById('s-accent2');
        if (a1) currentSettings.accent_color_1 = a1.value.toUpperCase();
        if (a2) currentSettings.accent_color_2 = a2.value.toUpperCase();
        await persistSettings();
    });
});

document.getElementById('settings-cancel').addEventListener('click', function() {
    settingsOverlay.classList.add('hidden');
});
settingsOverlay.addEventListener('click', function(e) {
    if (e.target === settingsOverlay) settingsOverlay.classList.add('hidden');
});

document.getElementById('settings-save').addEventListener('click', async function() {
    if (!currentSettings) return;
    currentSettings.project_title = document.getElementById('s-project').value;
    currentSettings.company       = document.getElementById('s-company').value;
    currentSettings.contact_name  = document.getElementById('s-contact').value;
    currentSettings.email         = document.getElementById('s-email').value;
    currentSettings.phone         = document.getElementById('s-phone').value;
    // logo_path is managed by the Browse/Clear buttons and already in currentSettings.
    var logoEl = document.getElementById('s-logo');
    if (logoEl) currentSettings.logo_path = logoEl.value;
    // Read accent colours from the native color pickers.
    // <input type="color">.value always returns a lowercase 7-char string like "#1f9ede".
    // We uppercase it for consistency with the Rust default strings.
    var a1 = document.getElementById('s-accent1');
    var a2 = document.getElementById('s-accent2');
    if (a1) currentSettings.accent_color_1 = a1.value.toUpperCase();
    if (a2) currentSettings.accent_color_2 = a2.value.toUpperCase();
    // Save column visibility flags.
    document.querySelectorAll('[data-col]').forEach(function(cb) {
        currentSettings[cb.dataset.col] = cb.checked;
    });
    await persistSettings();
    settingsOverlay.classList.add('hidden');
});

// ── Prompt modal ──────────────────────────────────────────────────────────────
//
// showPrompt() displays a modal dialog asking the user to resolve a conflict:
//   - "non_empty"  : destination folder already contains files
//   - "conflicts"  : specific files already exist in the destination
//
// Returns a Promise that resolves with the user's reply string ("cancel",
// "skip", or "continue") after the user clicks a button. The reply is also
// sent to Rust via invoke('prompt_reply') to unblock the copy thread.
var promptOverlay = document.getElementById('prompt-overlay');

function showPrompt(kind, items) {
    return new Promise(function(resolve) {
        var title   = document.getElementById('prompt-title');
        var message = document.getElementById('prompt-message');
        var btnRow  = document.getElementById('prompt-btn-row');
        btnRow.innerHTML = '';

        if (kind === 'non_empty') {
            title.textContent = 'Non-empty destination';
            message.textContent =
                'The following destination(s) already contain files:\n\n' +
                items.join('\n') + '\n\nContinue anyway?';
            addPromptBtn(btnRow, 'Cancel',   'cancel',   false, false, resolve);
            addPromptBtn(btnRow, 'Continue', 'continue', true,  false, resolve);
        } else {
            title.textContent = 'File conflicts detected';
            var preview = items.slice(0, 10).join('\n');
            var more = items.length > 10 ? '\n… and ' + (items.length - 10) + ' more.' : '';
            message.textContent =
                items.length + ' file(s) already exist:\n\n' +
                preview + more + '\n\nWhat would you like to do?';
            addPromptBtn(btnRow, 'Cancel',  'cancel',   false, false, resolve);
            addPromptBtn(btnRow, 'Skip',    'skip',     true,  false, resolve);
            addPromptBtn(btnRow, 'Replace', 'continue', true,  true,  resolve);
        }
        promptOverlay.classList.remove('hidden');
    });
}

// Creates and appends a button to the prompt button row.
// suggested=true applies the "suggested" CSS class (primary action style).
// danger=true applies the "danger" CSS class (destructive action style).
function addPromptBtn(row, label, reply, suggested, danger, resolve) {
    var btn = document.createElement('button');
    btn.className = 'action-btn' + (suggested ? ' suggested' : '') + (danger ? ' danger' : '');
    btn.textContent = label;
    btn.addEventListener('click', async function() {
        promptOverlay.classList.add('hidden');
        // Send the reply to Rust to unblock the waiting copy thread.
        try { await invoke('prompt_reply', { reply: reply }); } catch(e) {}
        resolve(reply);
    });
    row.appendChild(btn);
}

// ── Copy event listeners ──────────────────────────────────────────────────────
//
// unlisteners holds the cleanup functions returned by listen().
// Calling each function unregisters the corresponding event listener.
// This prevents duplicate handlers when launchCopy() is called multiple times.
var unlisteners = [];

// registerListeners registers the shared event listeners that stay active for
// the full duration of a queue run (all jobs). copy-done is intentionally
// excluded here — it is handled per-job by a one-shot listener inside runJob().
async function registerListeners() {
    unlisteners.forEach(function(fn) { fn(); });
    unlisteners = [];

    unlisteners.push(await listen('copy-progress', function(event) {
        progressFill.style.width = Math.round(event.payload.fraction * 100) + '%';
        // When running multiple jobs, prefix the label with "Job N/M — …"
        var label = event.payload.label;
        if (currentJobPrefix) label = currentJobPrefix + ' — ' + label;
        progressText.textContent = label;
    }));

    unlisteners.push(await listen('copy-log', function(event) {
        logView.textContent += event.payload.line;
        logView.scrollTop = logView.scrollHeight;
    }));

    unlisteners.push(await listen('copy-prompt', async function(event) {
        await showPrompt(event.payload.kind, event.payload.items);
    }));
}

// ── Launch copy ───────────────────────────────────────────────────────────────

// runJob starts a single copy operation and returns a Promise that resolves
// with { ok, summary } when the Rust copy-done event fires.
// A one-shot copy-done listener is registered inside the Promise and removed
// as soon as the event arrives, so sequential jobs never overlap.
async function runJob(src, dsts) {
    var hashAlgo = hashSelect ? hashSelect.value : 'none';
    var verify   = hashAlgo !== 'none';

    return new Promise(async function(resolve) {
        var unlisten = await listen('copy-done', function(event) {
            unlisten(); // remove this one-shot listener immediately
            resolve({ ok: event.payload.ok, summary: event.payload.summary });
        });

        try {
            await invoke('start_copy', {
                args: {
                    src:          src,
                    destinations: dsts,
                    verify:       verify,
                    gen_md5:      hashAlgo === 'md5',
                    gen_xxh:      hashAlgo === 'xxh3',
                    gen_csv:      chkCsv.checked,
                    gen_pdf:      chkPdf.checked,
                    gen_html:     chkHtml.checked,
                    open_dest:    false // handled at the end of launchCopy()
                }
            });
        } catch(e) {
            unlisten();
            resolve({ ok: false, summary: 'Error: ' + e });
        }
    });
}

// launchCopy validates all jobs, then runs them one after the other.
// Progress/log/prompt listeners are shared across the entire queue;
// each individual job's copy-done is awaited via runJob().
async function launchCopy() {
    var jobs  = getJobs();
    var multi = jobs.length > 1;

    // Validate every job before starting anything
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

    copyBtn.disabled = true;
    progressFill.style.width = '0%';
    progressText.textContent = 'Starting…';
    statusLabel.textContent  = '';
    statusLabel.className    = '';
    logView.textContent      = '';

    // Reset all job label colours before starting a new run
    jobsContainer.querySelectorAll('.job-label').forEach(function(lbl) {
        lbl.classList.remove('job-done');
    });

    await registerListeners();

    var allOk      = true;
    var lastSummary = '';

    for (var i = 0; i < jobs.length; i++) {
        var job = jobs[i];

        if (multi) {
            // Show which job is running and log a separator line
            currentJobPrefix = 'Job ' + (i + 1) + '/' + jobs.length;
            progressFill.style.width = '0%';
            logView.textContent += '\n── ' + currentJobPrefix + ' — ' + job.src + '\n';
            logView.scrollTop = logView.scrollHeight;
        }

        var result = await runJob(job.src, job.dsts);

        // Turn the job label green (or leave neutral on error) once the job finishes
        var jobCards = jobsContainer.querySelectorAll('.job-group');
        if (jobCards[i]) {
            var doneLbl = jobCards[i].querySelector('.job-label');
            if (doneLbl && result.ok) doneLbl.classList.add('job-done');
        }

        if (!result.ok) allOk = false;
        lastSummary = result.summary;

        if (multi) {
            logView.textContent += (result.ok ? '✓ ' : '✗ ') + result.summary + '\n';
            logView.scrollTop = logView.scrollHeight;
        }
    }

    // Tear down shared listeners now that all jobs are done
    currentJobPrefix = '';
    unlisteners.forEach(function(fn) { fn(); });
    unlisteners = [];

    // Final UI state
    statusLabel.textContent = multi
        ? (allOk ? 'All ' + jobs.length + ' jobs completed successfully.' : 'Queue finished with errors — check log.')
        : lastSummary;
    statusLabel.className    = allOk ? 'success' : 'error';
    progressFill.style.width = '100%';
    progressText.textContent = allOk ? 'Done' : 'Finished with errors';
    copyBtn.disabled = false;

    // ── Auto-save log to ~/.config/bartleby/logs/YYYY-MM-DD_HH-MM-SS.txt ──────
    try {
        var logPath = await invoke('save_log', { content: logView.textContent });
        logView.textContent += '\nLog saved: ' + logPath + '\n';
        logView.scrollTop = logView.scrollHeight;
    } catch(e) {
        console.warn('save_log failed:', e);
    }

    // ── System notification ────────────────────────────────────────────────────
    try {
        await invoke('send_notification', {
            title: 'Bartleby',
            body:  statusLabel.textContent
        });
    } catch(e) {
        console.warn('Notification failed:', e);
    }

    // ── Open destinations ─────────────────────────────────────────────────────
    if (allOk && chkOpen && chkOpen.checked) {
        var allDsts = [];
        jobs.forEach(function(j) { allDsts = allDsts.concat(j.dsts); });
        // Deduplicate: same destination folder used in several jobs → open only once
        var uniqueDsts = allDsts.filter(function(d, idx) { return allDsts.indexOf(d) === idx; });
        if (uniqueDsts.length > 0) {
            try { await invoke('open_destinations', { paths: uniqueDsts }); }
            catch(e) { console.error('open_destinations:', e); }
        }
    }
}

// Wire the single Copy button to launchCopy().
// verify is derived inside launchCopy() from the checkbox states.
copyBtn.addEventListener('click', function() { launchCopy(); });

// ── Drag & drop — folder drag from OS file manager ────────────────────────────
//
// Tauri v2 emits 'tauri://drag-drop' when folders are dragged onto the window.
// We identify which job card the drop landed in by comparing the drop Y position
// against each card's bounding rect. Within the matched card we check whether the
// drop is over the source row (→ fill the source input) or below it (→ fill/add
// a destination row). getBoundingClientRect() is reliable here because it
// reflects the element's painted position even when no hover events fire.
(async function setupDragDrop() {
    try {
        await window.__TAURI__.event.listen('tauri://drag-drop', function(event) {
            var paths = event.payload.paths;
            if (!paths || paths.length === 0) return;

            var droppedPath = paths[0];
            var dropY = event.payload.position ? event.payload.position.y : 0;

            // Find the job card that contains the drop Y coordinate
            var jobCards = Array.from(jobsContainer.querySelectorAll('.job-group'));
            var targetCard = null;
            for (var j = 0; j < jobCards.length; j++) {
                var r = jobCards[j].getBoundingClientRect();
                if (dropY >= r.top && dropY <= r.bottom + 40) {
                    targetCard = jobCards[j];
                    break;
                }
            }
            // Default to the last card when the drop is below all cards
            if (!targetCard && jobCards.length > 0) {
                targetCard = jobCards[jobCards.length - 1];
            }
            if (!targetCard) return;

            var srcInputEl = targetCard.querySelector('.job-src-input');
            var destListEl = targetCard.querySelector('.job-dest-list');

            // If the drop Y is over the source row, fill the source input;
            // otherwise fill an empty destination row or create a new one.
            var inSource = false;
            if (srcInputEl) {
                var srcRow = srcInputEl.closest('.row');
                if (srcRow) {
                    var sr = srcRow.getBoundingClientRect();
                    inSource = dropY >= sr.top && dropY <= sr.bottom + 30;
                }
            }

            if (inSource) {
                if (srcInputEl) srcInputEl.value = droppedPath;
            } else if (destListEl) {
                var inputs = Array.from(destListEl.querySelectorAll('input[type="text"]'));
                var emptyInput = inputs.find(function(i) { return i.value.trim() === ''; });
                if (emptyInput) {
                    emptyInput.value = droppedPath;
                } else {
                    addDestRowToJob(destListEl, droppedPath);
                }
            }
        });

        await window.__TAURI__.event.listen('tauri://drag-cancelled', function() {});

    } catch(e) {
        console.warn('Drag & drop not available:', e);
    }
})();

// ── Initialisation ────────────────────────────────────────────────────────────
//
// DOMContentLoaded fires after the HTML is fully parsed but before images load.
// We load settings here so checkboxes and theme are applied before the user
// sees the UI — avoiding a flash of wrong theme or unchecked checkboxes.
document.addEventListener('DOMContentLoaded', function() {
    loadSettings();
    addJob(); // create the first (empty) job card on startup
});
