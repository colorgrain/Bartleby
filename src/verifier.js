/* ── verifier.js — Bartleby verification window ──────────────────────────── */

'use strict';

var invoke = window.__TAURI__.core.invoke;
var listen = window.__TAURI__.event.listen;

var currentFile   = null;   // absolute path to the loaded checksum / MHL file
var lastResult    = null;   // VerifyResult from the last completed verification
var currentIsMhl  = false;  // whether the loaded file is an MHL

// ── Theme / skin on startup ───────────────────────────────────────────────────

(function applyStoredAppearance() {
    invoke('get_settings').then(function(s) {
        var skin = s.skin || 'mint-y-aqua';
        document.getElementById('theme-link').href = 'themes/' + skin + '.css';

        var theme = s.theme || 'default';
        if (theme === 'default') {
            invoke('is_system_dark_mode').then(function(isDark) {
                document.body.className = isDark ? 'theme-dark' : 'theme-light';
            }).catch(function() {
                document.body.className = 'theme-light';
            });
        } else {
            document.body.className = 'theme-' + theme;
        }

        var company = s.company || '';
        var name    = s.contact_name || '';
        if (company || name) {
            var html = '<div class="verifier-id">';
            if (company) html += '<div class="verifier-id-company">' + esc(company) + '</div>';
            if (name)    html += '<div class="verifier-id-name">'    + esc(name)    + '</div>';
            html += '</div>';
            document.getElementById('verifier-header').innerHTML = html;
        }
    }).catch(function() {});
})();

// ── Drag-and-drop on the input row ───────────────────────────────────────────

var filePathRow = document.getElementById('file-path-row');

filePathRow.addEventListener('dragover', function(e) {
    e.preventDefault();
    filePathRow.classList.add('drag-over');
});
filePathRow.addEventListener('dragleave', function(e) {
    if (!filePathRow.contains(e.relatedTarget)) {
        filePathRow.classList.remove('drag-over');
    }
});
filePathRow.addEventListener('drop', function(e) {
    e.preventDefault();
    filePathRow.classList.remove('drag-over');
});

// Tauri v2 native drag-drop event provides absolute paths
listen('tauri://drag-drop', function(event) {
    filePathRow.classList.remove('drag-over');
    var paths = event.payload && event.payload.paths;
    if (paths && paths.length > 0) {
        setFile(paths[0]);
    }
});

// ── Browse button ─────────────────────────────────────────────────────────────

document.getElementById('browse-btn').addEventListener('click', browseFile);

function browseFile() {
    window.__TAURI__.dialog.open({
        multiple:  false,
        filters: [{
            name:       'Checksum / MHL files',
            extensions: ['md5','sha1','xxh64','xxh3','xxh128','c4','mhl'],
        }],
    }).then(function(path) {
        if (path) setFile(path);
    }).catch(function() {});
}

// ── Set the active file ───────────────────────────────────────────────────────

function setFile(path) {
    currentFile = path;
    document.getElementById('file-path-input').value = path;
    clearAll();
    invoke('parse_verification_file', { filePath: path }).then(function(result) {
        renderFileList(result);
    }).catch(function(e) {
        showError('Could not parse file: ' + e);
    });
}

// ── Verify button ─────────────────────────────────────────────────────────────

document.getElementById('verify-action-btn').addEventListener('click', function() {
    if (!currentFile) return;
    startVerification(currentFile);
});

function startVerification(filePath) {
    resetVerificationColumns();
    showProgress(true);
    setProgress(0, 'Starting…');
    setVerifyActive(true);

    invoke('start_verification', { filePath: filePath }).catch(function(e) {
        showProgress(false);
        setVerifyActive(false);
        showError('Could not start verification: ' + e);
    });
}

function setVerifyActive(active) {
    document.getElementById('verify-action-btn').classList.toggle('hidden', active);
    document.getElementById('verify-pause-btn').classList.toggle('hidden', !active);
    document.getElementById('verify-stop-btn').classList.toggle('hidden', !active);
}

// ── Pause / Stop buttons ──────────────────────────────────────────────────────

var verifyIsPaused = false;

document.getElementById('verify-pause-btn').addEventListener('click', function() {
    if (verifyIsPaused) {
        invoke('resume_verification').catch(function() {});
    } else {
        invoke('pause_verification').catch(function() {});
    }
});

document.getElementById('verify-stop-btn').addEventListener('click', function() {
    invoke('cancel_verification').catch(function() {});
});

listen('verify-paused', function() {
    verifyIsPaused = true;
    var btn = document.getElementById('verify-pause-btn');
    btn.innerHTML = '<svg width="18" height="18"><use href="#ico-play"/></svg>';
    btn.title = 'Resume verification';
});

listen('verify-resumed', function() {
    verifyIsPaused = false;
    var btn = document.getElementById('verify-pause-btn');
    btn.innerHTML = '<svg width="18" height="18"><use href="#ico-pause"/></svg>';
    btn.title = 'Pause verification';
});

listen('verify-cancelled', function() {
    verifyIsPaused = false;
    showProgress(false);
    setVerifyActive(false);
    var btn = document.getElementById('verify-pause-btn');
    btn.innerHTML = '<svg width="18" height="18"><use href="#ico-pause"/></svg>';
    btn.title = 'Pause verification';
});

// ── Progress events ───────────────────────────────────────────────────────────

listen('verify-progress', function(event) {
    var p = event.payload;
    setProgress(p.fraction, p.label);
});

// Per-file result: update just the result columns of the matching row
listen('verify-entry', function(event) {
    var e   = event.payload;
    var row = document.querySelector('#results-tbody tr[data-idx="' + e.index + '"]');
    if (!row) return;

    var rowCls = {ok:'row-ok', mismatch:'row-fail', missing:'row-miss'}[e.status] || 'row-fail';
    row.className = rowCls;

    var statusCell   = row.querySelector('.cell-status');
    var computedCell = row.querySelector('.cell-computed');
    var sizeCell     = row.querySelector('.cell-size');
    var mtimeCell    = row.querySelector('.cell-mtime');

    if (statusCell)   statusCell.innerHTML   = statusBadge(e.status);
    if (computedCell) {
        computedCell.title     = e.computed || '';
        computedCell.innerHTML = e.computed ? truncHash(e.computed) : '—';
    }
    if (sizeCell)  sizeCell.innerHTML  = tickIcon(e.size_ok);
    if (mtimeCell) mtimeCell.innerHTML = tickIcon(e.mtime_ok);
});

listen('verify-done', function(event) {
    lastResult = event.payload;
    verifyIsPaused = false;
    setProgress(1.0, 'Done');
    setTimeout(function() { showProgress(false); }, 400);
    setVerifyActive(false);
    // Table rows already updated by verify-entry events — just refresh summary
    renderSummary(lastResult);
    showActionRow(lastResult);
});

listen('verify-error', function(event) {
    verifyIsPaused = false;
    showProgress(false);
    setVerifyActive(false);
    showError(event.payload);
});

// ── Progress bar helpers ──────────────────────────────────────────────────────

function showProgress(visible) {
    document.getElementById('progress-section').style.display = visible ? 'block' : 'none';
}

function setProgress(fraction, label) {
    document.getElementById('prog-fill').style.width = Math.round(fraction * 100) + '%';
    document.getElementById('prog-label').textContent = label;
}

// ── Render from pre-parsed file list ─────────────────────────────────────────

function clearAll() {
    lastResult = null;
    currentIsMhl = false;
    document.getElementById('summary-section').style.display  = 'none';
    document.getElementById('mhl-meta').style.display         = 'none';
    document.getElementById('results-section').style.display  = 'none';
    document.getElementById('action-row').style.display       = 'none';
    document.getElementById('post-verify-mhl-btn').style.display = 'none';
    document.getElementById('results-thead').innerHTML = '';
    document.getElementById('results-tbody').innerHTML = '';
    document.getElementById('meta-grid').innerHTML     = '';
    showProgress(false);
}

function renderFileList(r) {
    currentIsMhl = r.file_type === 'mhl';
    if (r.mhl_chain && r.mhl_chain.length > 0) {
        renderMhlChain(r.mhl_chain);
    } else if (r.mhl_meta) {
        renderMhlChain([r.mhl_meta]);
    }
    renderPendingTable(r);
}

// Render the MHL generation chain as a table — one row per generation
function renderMhlChain(chain) {
    var grid = document.getElementById('meta-grid');
    grid.innerHTML = '';

    var wrap = document.createElement('div');
    wrap.className = 'mhl-chain-wrap';

    var table = document.createElement('table');
    table.className = 'mhl-chain-table';

    var thead = document.createElement('thead');
    thead.innerHTML = '<tr>' +
        '<th>Gen</th><th>Date</th><th>Process / Hash</th>' +
        '<th>Author</th><th>Software</th><th>Location</th><th>Comment</th>' +
        '</tr>';
    table.appendChild(thead);

    var tbody = document.createElement('tbody');
    var currentGen = chain.length > 0 ? chain[chain.length - 1].generation : 0;

    chain.forEach(function(meta) {
        var tr = document.createElement('tr');
        if (meta.generation === currentGen) tr.className = 'mhl-gen-current';

        // Gen
        var cells = '<td class="mhl-td-gen">' + String(meta.generation).padStart(4, '0') + '</td>';

        // Date
        var dateStr = meta.finish_date
            ? meta.finish_date.replace('T', ' ').replace(/\.\d+Z$/, 'Z').slice(0, 19)
            : '';
        cells += '<td>' + esc(dateStr) + '</td>';

        // Process / Hash
        var processLines = [];
        if (meta.process)    processLines.push(esc(meta.process));
        if (meta.hash_algo)  processLines.push('<span class="mhl-dim">' + esc(meta.hash_algo.toUpperCase()) + '</span>');
        cells += '<td class="mhl-td-process">' + processLines.join('<br>') + '</td>';

        // Author — company / name / email / phone each on its own line
        var authorLines = [];
        if (meta.author_company) authorLines.push(esc(meta.author_company));
        if (meta.author_name)    authorLines.push(esc(meta.author_name));
        if (meta.author_email)   authorLines.push('<span class="mhl-dim">' + esc(meta.author_email) + '</span>');
        if (meta.author_phone)   authorLines.push('<span class="mhl-dim">' + esc(meta.author_phone) + '</span>');
        cells += '<td>' + authorLines.join('<br>') + '</td>';

        // Software
        cells += '<td>' + esc(meta.creator || '') + '</td>';

        // Location
        cells += '<td>' + esc(meta.location || '') + '</td>';

        // Comment
        cells += '<td>' + esc(meta.comment || '') + '</td>';

        tr.innerHTML = cells;
        tbody.appendChild(tr);
    });

    table.appendChild(tbody);
    wrap.appendChild(table);
    grid.appendChild(wrap);
    document.getElementById('mhl-meta').style.display = 'block';
}

// Build the table with all files visible immediately, result columns as "pending"
function renderPendingTable(r) {
    var isMhl   = r.file_type === 'mhl';
    var algoUp  = (r.algo || '').toUpperCase();
    var thead   = document.getElementById('results-thead');
    var tbody   = document.getElementById('results-tbody');

    var headerCells = '<th>File</th><th>Status</th>' +
        '<th>Expected ' + esc(algoUp) + '</th>' +
        '<th>Computed ' + esc(algoUp) + '</th>';
    if (isMhl) { headerCells += '<th>Size</th><th>Mtime</th>'; }
    thead.innerHTML = '<tr>' + headerCells + '</tr>';

    var rows = '';
    r.entries.forEach(function(e, idx) {
        var expHash = truncHash(e.expected_hash);
        rows += '<tr class="row-pending" data-idx="' + idx + '">' +
            '<td class="cell-path">' + esc(e.rel_path) + '</td>' +
            '<td class="cell-status"><span class="badge badge-pending">…</span></td>' +
            '<td class="cell-hash" title="' + esc(e.expected_hash) + '">' + expHash + '</td>' +
            '<td class="cell-hash cell-computed">—</td>';
        if (isMhl) {
            rows += '<td class="cell-size">—</td><td class="cell-mtime">—</td>';
        }
        rows += '</tr>';
    });
    tbody.innerHTML = rows;

    document.getElementById('results-section').style.display = 'block';
}

// Reset only the verification result columns (called before re-verification)
function resetVerificationColumns() {
    document.getElementById('summary-section').style.display = 'none';
    document.getElementById('action-row').style.display      = 'none';
    document.getElementById('post-verify-mhl-btn').style.display = 'none';
    lastResult = null;

    var rows = document.querySelectorAll('#results-tbody tr');
    rows.forEach(function(row) {
        row.className = 'row-pending';
        var statusCell   = row.querySelector('.cell-status');
        var computedCell = row.querySelector('.cell-computed');
        var sizeCell     = row.querySelector('.cell-size');
        var mtimeCell    = row.querySelector('.cell-mtime');
        if (statusCell)   statusCell.innerHTML   = '<span class="badge badge-pending">…</span>';
        if (computedCell) { computedCell.textContent = '—'; computedCell.title = ''; }
        if (sizeCell)  sizeCell.textContent  = '—';
        if (mtimeCell) mtimeCell.textContent  = '—';
    });
}

function renderSummary(r) {
    document.getElementById('chip-total').textContent = r.total + ' files';
    document.getElementById('chip-ok').textContent    = r.ok_count + ' passed';
    document.getElementById('chip-fail').textContent  = r.fail_count + ' failed';
    document.getElementById('chip-miss').textContent  = r.missing_count + ' missing';
    document.getElementById('summary-section').style.display = 'block';
}

function showActionRow(r) {
    var actionRow = document.getElementById('action-row');
    actionRow.style.display = 'flex';
    var mhlBtn = document.getElementById('post-verify-mhl-btn');
    mhlBtn.style.display = (r.file_type === 'mhl') ? 'inline-flex' : 'none';
}

// ── Badge / tick helpers ──────────────────────────────────────────────────────

function statusBadge(status) {
    var map = {
        ok:       '<span class="badge badge-ok">✓ OK</span>',
        mismatch: '<span class="badge badge-fail">✗ MISMATCH</span>',
        missing:  '<span class="badge badge-miss">⚠ MISSING</span>',
    };
    return map[status] || '<span class="badge badge-err">! ERROR</span>';
}

function tickIcon(val) {
    if (val === true)  return '<span class="tick-ok">✓</span>';
    if (val === false) return '<span class="tick-fail">✗</span>';
    return '—';
}

function truncHash(h) {
    if (!h || h.length <= 16) return esc(h || '');
    return esc(h.slice(0, 8) + '…' + h.slice(-8));
}

function esc(s) {
    if (!s) return '';
    return String(s)
        .replace(/&/g, '&amp;')
        .replace(/</g, '&lt;')
        .replace(/>/g, '&gt;')
        .replace(/"/g, '&quot;');
}

// ── Error display ─────────────────────────────────────────────────────────────

function showError(msg) {
    var prog = document.getElementById('progress-section');
    prog.style.display = 'block';
    document.getElementById('prog-fill').style.width = '0%';
    document.getElementById('prog-label').textContent = '⚠ ' + msg;
}

// ── Save HTML report ──────────────────────────────────────────────────────────

document.getElementById('save-html-btn').addEventListener('click', function() {
    if (!lastResult) return;

    var defaultName = 'verify_report.html';
    var src = lastResult.file_path;
    if (src) {
        var base = src.replace(/\\/g, '/').split('/').pop().replace(/\.[^.]+$/, '');
        defaultName = 'verify_' + base + '.html';
    }

    window.__TAURI__.dialog.save({
        defaultPath: defaultName,
        filters: [{ name: 'HTML', extensions: ['html'] }],
    }).then(function(outputPath) {
        if (!outputPath) return;
        invoke('save_verify_html', {
            result:     lastResult,
            outputPath: outputPath,
        }).then(function() {
            setTransientStatus('HTML report saved.');
        }).catch(function(e) {
            showError('Could not save report: ' + e);
        });
    }).catch(function() {});
});

// ── Generate post-verify MHL ──────────────────────────────────────────────────

document.getElementById('post-verify-mhl-btn').addEventListener('click', function() {
    if (!lastResult || lastResult.file_type !== 'mhl') return;

    invoke('generate_post_verify_mhl', {
        verifiedMhl: lastResult.file_path,
        result:      lastResult,
    }).then(function(outPath) {
        setTransientStatus('Post-verify MHL saved: ' + outPath);
    }).catch(function(e) {
        showError('Could not generate MHL: ' + e);
    });
});

// ── Transient status helper ───────────────────────────────────────────────────

var statusTimer = null;

function setTransientStatus(msg) {
    var prog = document.getElementById('progress-section');
    prog.style.display = 'block';
    document.getElementById('prog-fill').style.width = '100%';
    document.getElementById('prog-label').textContent = msg;
    clearTimeout(statusTimer);
    statusTimer = setTimeout(function() {
        prog.style.display = 'none';
    }, 4000);
}
