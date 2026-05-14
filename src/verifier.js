/* ── verifier.js — Bartleby verification window ──────────────────────────── */

'use strict';

var invoke = window.__TAURI__.core.invoke;
var listen = window.__TAURI__.event.listen;

var currentFile   = null;   // absolute path to the loaded checksum / MHL file
var lastResult    = null;   // VerifyResult from the last completed verification

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
    resetResults();
}

// ── Verify button ─────────────────────────────────────────────────────────────

document.getElementById('verify-action-btn').addEventListener('click', function() {
    if (!currentFile) return;
    startVerification(currentFile);
});

function startVerification(filePath) {
    resetResults();
    showProgress(true);
    setProgress(0, 'Starting…');
    document.getElementById('verify-action-btn').disabled = true;

    invoke('start_verification', { filePath: filePath }).catch(function(e) {
        showProgress(false);
        document.getElementById('verify-action-btn').disabled = false;
        showError('Could not start verification: ' + e);
    });
}

// ── Progress events ───────────────────────────────────────────────────────────

listen('verify-progress', function(event) {
    var p = event.payload;
    setProgress(p.fraction, p.label);
});

listen('verify-done', function(event) {
    lastResult = event.payload;
    setProgress(1.0, 'Done');
    setTimeout(function() { showProgress(false); }, 400);
    document.getElementById('verify-action-btn').disabled = false;
    renderResult(lastResult);
});

listen('verify-error', function(event) {
    showProgress(false);
    document.getElementById('verify-action-btn').disabled = false;
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

// ── Render result ─────────────────────────────────────────────────────────────

function resetResults() {
    lastResult = null;
    document.getElementById('summary-section').style.display = 'none';
    document.getElementById('mhl-meta').style.display        = 'none';
    document.getElementById('results-section').style.display = 'none';
    document.getElementById('action-row').style.display      = 'none';
    document.getElementById('post-verify-mhl-btn').style.display = 'none';
    document.getElementById('results-thead').innerHTML = '';
    document.getElementById('results-tbody').innerHTML = '';
    document.getElementById('meta-grid').innerHTML     = '';
}

function renderResult(r) {
    renderSummary(r);
    if (r.mhl_meta) renderMhlMeta(r.mhl_meta);
    renderTable(r);
    showActionRow(r);
}

function renderSummary(r) {
    document.getElementById('chip-total').textContent = r.total + ' files';
    document.getElementById('chip-ok').textContent    = r.ok_count + ' passed';
    document.getElementById('chip-fail').textContent  = r.fail_count + ' failed';
    document.getElementById('chip-miss').textContent  = r.missing_count + ' missing';
    document.getElementById('summary-section').style.display = 'block';
}

function renderMhlMeta(meta) {
    var grid = document.getElementById('meta-grid');
    grid.innerHTML = '';

    var items = [];
    if (meta.creator)      items.push(['Creator',     esc(meta.creator)]);
    if (meta.finish_date)  items.push(['Date',         esc(meta.finish_date)]);
    if (meta.process)      items.push(['Process',      esc(meta.process)]);
    if (meta.generation)   items.push(['Generation',   String(meta.generation).padStart(4, '0')]);
    if (meta.author_name)  items.push(['Author',       esc(meta.author_name)]);
    if (meta.author_email) items.push(['Email',        esc(meta.author_email)]);
    if (meta.location)     items.push(['Location',     esc(meta.location)]);
    if (meta.comment)      items.push(['Comment',      esc(meta.comment)]);
    if (meta.parent_ref)   items.push(['Parent MHL',   '<span class="chain-ref">' + esc(meta.parent_ref) + '</span>']);

    items.forEach(function(pair) {
        var div = document.createElement('div');
        div.className = 'meta-item';
        div.innerHTML = '<label>' + pair[0] + '</label><span>' + pair[1] + '</span>';
        grid.appendChild(div);
    });

    if (items.length > 0) {
        document.getElementById('mhl-meta').style.display = 'block';
    }
}

function renderTable(r) {
    var isMhl    = r.file_type === 'mhl';
    var algoUp   = (r.algo || '').toUpperCase();
    var thead    = document.getElementById('results-thead');
    var tbody    = document.getElementById('results-tbody');

    var headerCells = '<th>File</th><th>Status</th>' +
        '<th>Expected ' + esc(algoUp) + '</th>' +
        '<th>Computed ' + esc(algoUp) + '</th>';
    if (isMhl) { headerCells += '<th>Size</th><th>Mtime</th>'; }
    thead.innerHTML = '<tr>' + headerCells + '</tr>';

    var rows = '';
    r.entries.forEach(function(e) {
        var rowCls   = {ok:'row-ok', mismatch:'row-fail', missing:'row-miss'}[e.status] || 'row-fail';
        var badge    = statusBadge(e.status);
        var expHash  = truncHash(e.expected);
        var compHash = e.computed ? truncHash(e.computed) : '—';
        var row      = '<tr class="' + rowCls + '">' +
            '<td class="cell-path">' + esc(e.rel_path) + '</td>' +
            '<td>' + badge + '</td>' +
            '<td class="cell-hash" title="' + esc(e.expected) + '">' + expHash + '</td>' +
            '<td class="cell-hash" title="' + esc(e.computed) + '">' + compHash + '</td>';
        if (isMhl) {
            row += '<td>' + tickIcon(e.size_ok) + '</td>' +
                   '<td>' + tickIcon(e.mtime_ok) + '</td>';
        }
        row += '</tr>';
        rows += row;
    });
    tbody.innerHTML = rows;

    document.getElementById('results-section').style.display = 'block';
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
    if (!h || h.length <= 16) return esc(h);
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
