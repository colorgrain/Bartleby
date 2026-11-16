/**
 * appearance.js — window chrome + colour themes, shared by every Bartleby window
 * (loaded by index.html and verifier.html before their own script).
 *
 * ── Window chrome ────────────────────────────────────────────────────────────
 * Bartleby's windows are undecorated and transparent (tauri.conf.json /
 * WebviewWindowBuilder in main.rs): the OS draws nothing, so the title bar,
 * the minimise / maximise / close buttons, the rounded corners and the resize
 * edges are all drawn here, in one of three styles chosen in Settings:
 *
 *   adwaita — GNOME/libadwaita: round grey buttons on the right, 12 px corners
 *   breeze  — KDE Plasma: chevron glyphs, circle on hover, red close, 6 px corners
 *   macos   — traffic lights on the left, 10 px corners
 *   windows — Windows 11 caption buttons on the right, 8 px corners
 *
 * The style is `settings.skin`; it also selects the palette file
 * `themes/{skin}.css`. `body[data-os]` drives the CSS in style.css.
 *
 * ── Colour themes ────────────────────────────────────────────────────────────
 * `settings.theme` is "default" (follow OS) | "light" | "dark" | "custom:<name>".
 * A custom theme stores four colours: background (window), foreground
 * (side panels, log, cards, dialogs), text and accent. Every other CSS token
 * is derived from them by derivePalette() and written as inline custom
 * properties on <body>, overriding the palette file.
 *
 * `var` instead of let/const: same WebKit compatibility rule as main.js.
 */

var BartlebyAppearance = (function() {

    var OS_STYLES = ['adwaita', 'breeze', 'macos', 'windows'];

    // Tokens a custom theme overrides — cleared again when leaving it.
    var TOKENS = ['--bg', '--surface', '--card-bg', '--card-border', '--text', '--text-dim',
        '--accent', '--accent-fg', '--accent-hover', '--accent-text',
        '--titlebar-bg', '--titlebar-txt', '--titlebar-border', '--titlebar-shadow',
        '--input-bg', '--input-border', '--input-focus',
        '--separator', '--scrollbar', '--shadow'];

    function tauriWindow() {
        return window.__TAURI__.window.getCurrentWindow();
    }

    function invoke(cmd, args) {
        return window.__TAURI__.core.invoke(cmd, args);
    }

    // Settings::load() (settings.rs) already migrates old values; this is
    // only a safety net for anything unexpected.
    function normalizeSkin(skin) {
        if (skin === 'windows11') return 'windows';
        if (OS_STYLES.indexOf(skin) >= 0) return skin;
        var p = (navigator.platform || '').toLowerCase();
        if (p.indexOf('mac') === 0) return 'macos';
        if (p.indexOf('win') === 0) return 'windows';
        return 'adwaita';
    }

    // ── Colour maths ─────────────────────────────────────────────────────────

    function parseColor(str) {
        // Accepts #rgb, #rrggbb, rgb(), rgba(). Returns {r,g,b,a} or null.
        if (!str) return null;
        str = String(str).trim();
        var m;
        if (str.charAt(0) === '#') {
            var h = str.slice(1);
            if (h.length === 3) h = h.charAt(0) + h.charAt(0) + h.charAt(1) + h.charAt(1) + h.charAt(2) + h.charAt(2);
            if (!/^[0-9a-fA-F]{6}$/.test(h)) return null;
            return { r: parseInt(h.slice(0, 2), 16), g: parseInt(h.slice(2, 4), 16), b: parseInt(h.slice(4, 6), 16), a: 1 };
        }
        m = str.match(/^rgba?\(\s*([\d.]+)[\s,]+([\d.]+)[\s,]+([\d.]+)(?:[\s,/]+([\d.]+%?))?\s*\)$/);
        if (m) {
            var a = m[4] === undefined ? 1 : (m[4].slice(-1) === '%' ? parseFloat(m[4]) / 100 : parseFloat(m[4]));
            return { r: +m[1], g: +m[2], b: +m[3], a: a };
        }
        return null;
    }

    function toHex(c) {
        function h(v) { var s = Math.round(Math.max(0, Math.min(255, v))).toString(16); return s.length < 2 ? '0' + s : s; }
        return '#' + h(c.r) + h(c.g) + h(c.b);
    }

    // Linear blend: t = 0 → a, t = 1 → b.
    function mix(a, b, t) {
        return { r: a.r + (b.r - a.r) * t, g: a.g + (b.g - a.g) * t, b: a.b + (b.b - a.b) * t, a: 1 };
    }

    function rgba(c, alpha) {
        return 'rgba(' + Math.round(c.r) + ', ' + Math.round(c.g) + ', ' + Math.round(c.b) + ', ' + alpha + ')';
    }

    // Relative luminance (WCAG), 0 = black, 1 = white.
    function luminance(c) {
        function ch(v) { v /= 255; return v <= 0.03928 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4); }
        return 0.2126 * ch(c.r) + 0.7152 * ch(c.g) + 0.0722 * ch(c.b);
    }

    function isDarkColor(hex) {
        var c = parseColor(hex);
        return c ? luminance(c) < 0.18 : false;
    }

    var WHITE = { r: 255, g: 255, b: 255, a: 1 };
    var BLACK = { r: 0, g: 0, b: 0, a: 1 };

    /**
     * Derives the full token set from the four base colours of a custom theme:
     * c = { bg, fg, text, accent } — fg is the panel/card surface colour.
     */
    function derivePalette(c) {
        var bg = parseColor(c.bg) || WHITE;
        var fg = parseColor(c.fg) || bg;
        var tx = parseColor(c.text) || BLACK;
        var ac = parseColor(c.accent) || { r: 53, g: 132, b: 228, a: 1 };
        var dark = luminance(bg) < 0.18;
        return {
            dark: dark,
            vars: {
                '--bg':              toHex(bg),
                '--card-bg':         toHex(fg),
                '--surface':         toHex(fg),
                '--card-border':     toHex(mix(fg, dark ? BLACK : tx, dark ? 0.35 : 0.16)),
                '--text':            toHex(tx),
                '--text-dim':        toHex(mix(tx, bg, 0.45)),
                '--accent':          toHex(ac),
                '--accent-fg':       luminance(ac) > 0.45 ? '#000000' : '#ffffff',
                '--accent-hover':    toHex(mix(ac, BLACK, 0.18)),
                '--accent-text':     toHex(dark ? mix(ac, WHITE, 0.30) : mix(ac, BLACK, 0.20)),
                '--titlebar-bg':     toHex(bg),
                '--titlebar-txt':    toHex(tx),
                '--titlebar-border': toHex(mix(bg, dark ? BLACK : tx, dark ? 0.35 : 0.15)),
                '--titlebar-shadow': 'none',
                '--input-bg':        toHex(dark ? mix(fg, WHITE, 0.05) : mix(fg, WHITE, 0.70)),
                '--input-border':    toHex(mix(fg, tx, dark ? 0.20 : 0.25)),
                '--input-focus':     toHex(ac),
                '--separator':       rgba(tx, dark ? 0.10 : 0.12),
                '--scrollbar':       rgba(tx, dark ? 0.18 : 0.20),
                '--shadow':          dark ? 'rgba(0, 0, 0, 0.40)' : 'rgba(0, 0, 0, 0.08)'
            }
        };
    }

    // ── Theme application ────────────────────────────────────────────────────

    function clearOverrides() {
        TOKENS.forEach(function(t) { document.body.style.removeProperty(t); });
        document.body.style.removeProperty('color-scheme');
    }

    function setMode(dark) {
        document.body.classList.remove('theme-dark', 'theme-light', 'theme-default');
        document.body.classList.add(dark ? 'theme-dark' : 'theme-light');
        // Native side: file dialogs, WebView colour-scheme, scrollbars.
        invoke('set_window_theme', { theme: dark ? 'dark' : 'light' }).catch(function() {});
    }

    /** Applies {bg, fg, text, accent} directly (live preview and custom themes). */
    function applyColors(c) {
        var p = derivePalette(c);
        Object.keys(p.vars).forEach(function(k) { document.body.style.setProperty(k, p.vars[k]); });
        document.body.style.setProperty('color-scheme', p.dark ? 'dark' : 'light');
        setMode(p.dark);
    }

    function findCustom(settings, theme) {
        if (!theme || theme.indexOf('custom:') !== 0) return null;
        var name = theme.slice(7);
        var list = settings.custom_themes || [];
        for (var i = 0; i < list.length; i++) if (list[i].name === name) return upgradeCustom(list[i]);
        return null;
    }

    // Themes saved by the first version had 3 colours, with `fg` holding the
    // text colour. Move it to `text` and derive a panel colour from the background.
    function upgradeCustom(t) {
        if (t.text) return t;
        var bg = parseColor(t.bg) || WHITE;
        var dark = luminance(bg) < 0.18;
        return { name: t.name, bg: t.bg, text: t.fg, accent: t.accent,
                 fg: toHex(mix(bg, WHITE, dark ? 0.06 : 0.65)) };
    }

    function setSkin(skin) {
        skin = normalizeSkin(skin);
        var link = document.getElementById('theme-link');
        if (link && link.getAttribute('href') !== 'themes/' + skin + '.css') link.href = 'themes/' + skin + '.css';
        document.body.setAttribute('data-os', skin);
        placeControls();
        return skin;
    }

    /**
     * Applies skin + colour theme from a settings object. Resolves once the
     * light/dark mode is known (it may need an IPC round-trip for "default").
     */
    function apply(settings) {
        settings = settings || {};
        setSkin(settings.skin);
        clearOverrides();
        var theme = settings.theme || 'default';
        var custom = findCustom(settings, theme);
        if (custom) {
            applyColors(custom);
            return Promise.resolve();
        }
        if (theme === 'light' || theme === 'dark') {
            setMode(theme === 'dark');
            return Promise.resolve();
        }
        // "default" (or an unknown/deleted custom theme): follow the OS.
        // prefers-color-scheme is unreliable in WebKitGTK, so ask Rust.
        return invoke('is_system_dark_mode').then(function(isDark) {
            setMode(isDark);
            invoke('set_window_theme', { theme: 'default' }).catch(function() {});
        }).catch(function() {
            document.body.classList.remove('theme-dark', 'theme-light');
            document.body.classList.add('theme-default'); // CSS @media fallback
        });
    }

    /** Current effective base colours {bg, fg, text, accent} as #rrggbb (for the pickers). */
    function currentColors() {
        var cs = getComputedStyle(document.body);
        var bg = parseColor(cs.getPropertyValue('--bg')) || WHITE;
        function solid(name) {
            var c = parseColor(cs.getPropertyValue(name));
            if (!c) return '#000000';
            return toHex(c.a < 1 ? mix(bg, c, c.a) : c); // flatten alpha over the background
        }
        return { bg: toHex(bg), fg: solid('--card-bg'), text: solid('--text'), accent: solid('--accent') };
    }

    // ── Window chrome ────────────────────────────────────────────────────────

    var controlsEl = null;
    var chromeOpts = null;

    var ICONS = {
        min:     '<svg viewBox="0 0 10 10"><path d="M1.5 5h7"/></svg>',
        max:     '<svg viewBox="0 0 10 10"><rect x="1.5" y="1.5" width="7" height="7" rx="0.5"/></svg>',
        restore: '<svg viewBox="0 0 10 10"><rect x="1.5" y="3" width="5.5" height="5.5" rx="0.5"/><path d="M3 3V1.5h5.5V7H7"/></svg>',
        close:   '<svg viewBox="0 0 10 10"><path d="M2 2l6 6M8 2L2 8"/></svg>'
    };
    // Breeze draws chevrons: ▾ minimise, ▴ maximise, ◇ restore.
    var BREEZE_ICONS = {
        min:     '<svg viewBox="0 0 10 10"><path d="M1.5 3.5L5 7l3.5-3.5"/></svg>',
        max:     '<svg viewBox="0 0 10 10"><path d="M1.5 6.5L5 3l3.5 3.5"/></svg>',
        restore: '<svg viewBox="0 0 10 10"><path d="M5 1.5L8.5 5 5 8.5 1.5 5z"/></svg>',
        close:   ICONS.close
    };

    function icon(action) {
        var set = document.body.getAttribute('data-os') === 'breeze' ? BREEZE_ICONS : ICONS;
        return set[action];
    }

    var isMaximized = false;

    function renderIcons() {
        if (!controlsEl) return;
        ['min', 'close'].forEach(function(a) { controlsEl.querySelector('.win-' + a).innerHTML = icon(a); });
        var b = controlsEl.querySelector('.win-max');
        b.innerHTML = icon(isMaximized ? 'restore' : 'max');
        b.title = isMaximized ? 'Restore' : 'Maximize';
    }

    function makeButton(action, title) {
        var b = document.createElement('button');
        b.className = 'win-btn win-' + action;
        b.type = 'button';
        b.title = title;
        b.setAttribute('aria-label', title);
        b.addEventListener('click', function(e) {
            e.stopPropagation();
            var w = tauriWindow();
            if (action === 'min') w.minimize();
            else if (action === 'max') w.toggleMaximize();
            else if (chromeOpts && chromeOpts.onClose) chromeOpts.onClose();
            else w.close();
        });
        // Keep a mousedown on a button from starting a window drag.
        b.addEventListener('mousedown', function(e) { e.stopPropagation(); });
        return b;
    }

    // macOS puts the controls first (left), Linux and Windows last (right).
    // The DOM order of the three buttons is the visual order for each OS.
    function placeControls() {
        if (!controlsEl || !chromeOpts) return;
        var os = document.body.getAttribute('data-os') || 'adwaita';
        var bar = chromeOpts.titlebar;
        var order = os === 'macos' ? ['close', 'min', 'max'] : ['min', 'max', 'close'];
        order.forEach(function(a) { controlsEl.appendChild(controlsEl.querySelector('.win-' + a)); });
        if (os === 'macos') bar.insertBefore(controlsEl, bar.firstChild);
        else bar.appendChild(controlsEl);
        renderIcons();
    }

    var RESIZE_EDGES = {
        n: 'North', s: 'South', e: 'East', w: 'West',
        ne: 'NorthEast', nw: 'NorthWest', se: 'SouthEast', sw: 'SouthWest'
    };

    function addResizeHandles() {
        Object.keys(RESIZE_EDGES).forEach(function(k) {
            var d = document.createElement('div');
            d.className = 'win-resize win-resize-' + k;
            d.addEventListener('mousedown', function(e) {
                if (e.button !== 0) return;
                e.preventDefault();
                tauriWindow().startResizeDragging(RESIZE_EDGES[k]).catch(function() {});
            });
            document.body.appendChild(d);
        });
    }

    function syncMaximized() {
        tauriWindow().isMaximized().then(function(max) {
            isMaximized = max;
            document.body.classList.toggle('win-maximized', max);
            renderIcons();
        }).catch(function() {});
    }

    /**
     * Turns `opts.titlebar` into the window's title bar: drag region, window
     * buttons, plus resize edges around the window.
     *   opts.title    — text shown in the bar (optional)
     *   opts.onClose  — what the close button does (default: window.close())
     */
    function initChrome(opts) {
        chromeOpts = opts;
        var bar = opts.titlebar;
        bar.setAttribute('data-tauri-drag-region', '');
        bar.classList.add('win-titlebar');

        // A flexible, draggable title area between the app's own buttons.
        var title = document.createElement('div');
        title.className = 'win-title';
        title.setAttribute('data-tauri-drag-region', '');
        title.textContent = opts.title || '';
        var anchor = opts.titleAfter ? opts.titleAfter.nextSibling : bar.firstChild;
        bar.insertBefore(title, anchor);

        controlsEl = document.createElement('div');
        controlsEl.className = 'win-controls';
        controlsEl.appendChild(makeButton('close', 'Close'));
        controlsEl.appendChild(makeButton('min', 'Minimize'));
        controlsEl.appendChild(makeButton('max', 'Maximize'));
        placeControls();

        addResizeHandles();
        syncMaximized();
        tauriWindow().onResized(function() { syncMaximized(); }).catch(function() {});
    }

    return {
        apply:          apply,
        applyColors:    applyColors,
        clearOverrides: clearOverrides,
        currentColors:  currentColors,
        derivePalette:  derivePalette,
        isDarkColor:    isDarkColor,
        normalizeSkin:  normalizeSkin,
        initChrome:     initChrome
    };
})();
