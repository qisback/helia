/* Download widget: a split button with a menu of installers and portable versions.
 *
 * Fills every <div class="dl"> from a manifest made by tools/downloads.py:
 *   data-dl-manifest='{...}'        the manifest inline (12hobbies.studio), or
 *   data-dl-src="downloads.json"    a same-origin file written when the docs were built.
 * The plain list already inside the div stays when scripts or the manifest are missing.
 * The browser sends nothing anywhere. The last choice is kept in localStorage (this browser
 * only; ignored when storage is blocked).
 * Kept identical in 12hobbies-studio (theme/twelvehobbies/assets/js) and this repository
 * (docs/book/src/javascripts). SPDX-License-Identifier: GPL-3.0-or-later
 */
(function () {
  'use strict';

  /* Which system and chip is the visitor on? os: windows, linux, macos, mobile or other;
     arch: x64 or arm64. cpu is userAgentData's architecture when the browser gives it. */
  function detect(ua, touchPoints, cpu) {
    ua = ua || '';
    var os = 'other';
    if (/Android|iPhone|iPad|iPod/i.test(ua)) os = 'mobile';
    else if (/Macintosh|Mac OS X/i.test(ua)) os = touchPoints > 1 ? 'mobile' : 'macos';
    else if (/Windows/i.test(ua)) os = 'windows';
    else if (/CrOS/i.test(ua)) os = 'other';
    else if (/Linux|X11|Ubuntu|Fedora|Debian/i.test(ua)) os = 'linux';
    var arch = /aarch64|arm64|armv8|armv7|\barm\b/i.test(ua) ? 'arm64' : 'x64';
    if (cpu === 'arm') arch = 'arm64';
    else if (cpu === 'x86') arch = 'x64';
    return { os: os, arch: arch };
  }

  /* The option to put on the big button: the installer for Windows and Mac; on Linux the
     AppImage, or the .deb / .rpm when the browser names the distribution. */
  function choose(m, env, ua) {
    var opts = (m.options || []).filter(function (o) {
      return o.os === env.os && (o.os !== 'linux' || o.arch === env.arch);
    });
    if (!opts.length) return null;
    var score = function (o) {
      var r = o.rank;
      if (o.os === 'linux') {
        if (/Ubuntu|Debian/i.test(ua || '') && o.kind === 'deb') r = -1;
        if (/Fedora|SUSE|Red Hat|CentOS/i.test(ua || '') && o.kind === 'rpm') r = -1;
      }
      return r;
    };
    opts.sort(function (a, b) { return score(a) - score(b); });
    return opts[0];
  }

  function el(tag, cls, text) {
    var n = document.createElement(tag);
    if (cls) n.className = cls;
    if (text !== undefined) n.textContent = text;
    return n;
  }
  function link(href, text) {
    var a = el('a', '', text);
    a.href = href;
    a.rel = 'noopener';
    return a;
  }
  function store(key, val) {
    try { if (val === undefined) return window.localStorage.getItem(key); window.localStorage.setItem(key, val); } catch (e) { /* blocked */ }
    return null;
  }

  var uid = 0;

  /* Menu groups: one per system (Linux per chip), installers then portable. */
  function groups(m) {
    var out = [];
    var add = function (os, arch) {
      var title = m.os_titles[os] + (os === 'linux' ? ' (' + (arch === 'arm64' ? 'ARM64' : '64-bit Intel or AMD') + ')' : '');
      var items = m.options.filter(function (o) { return o.os === os && (os !== 'linux' || o.arch === arch); });
      if (items.length) out.push({ title: title, items: items });
    };
    add('windows'); add('linux', 'x64'); add('linux', 'arm64'); add('macos');
    (m.coming_soon || []).forEach(function (c) {
      out.push({ title: c.os_title, soon: c });
    });
    return out;
  }

  function render(root, m, opt) {
    var env = detect(navigator.userAgent, navigator.maxTouchPoints || 0, root._cpu);
    var auto = !opt;
    if (!opt && env.os !== 'mobile') opt = choose(m, env, navigator.userAgent);
    var out = el('div', 'dl-body');
    var id = 'dl-menu-' + (++uid);

    out.appendChild(el('p', 'dl-version', 'Version ' + m.version + ', released on ' + m.date + '.'));
    if (opt) {
      var named = auto ? (env.os === 'macos' ? 'a Mac' : m.os_titles[env.os]) : '';
      out.appendChild(el('p', 'dl-detect', auto
        ? 'It looks like you are using ' + named + '. Not right? Open the list to choose another download.'
        : 'Showing your choice. Open the list to choose another download.'));
    } else if (env.os === 'mobile') {
      out.appendChild(el('p', 'dl-msg', m.product + ' is for computers. Open this page on your Windows, Linux or Mac computer, or choose a download below to save for later.'));
    } else {
      out.appendChild(el('p', 'dl-msg', (m.no_build && m.no_build[env.os]) || 'We could not tell which computer you are using. Choose yours from the list.'));
    }

    var split = el('div', 'dl-split');
    var main;
    if (opt) {
      main = el('a', 'dl-btn', opt.button);
      main.href = opt.url;
      main.rel = 'noopener';
    } else {
      main = el('button', 'dl-btn', 'Choose your download');
      main.type = 'button';
    }
    split.appendChild(main);
    var caret = el('button', 'dl-caret');
    caret.type = 'button';
    caret.setAttribute('aria-haspopup', 'menu');
    caret.setAttribute('aria-expanded', 'false');
    caret.setAttribute('aria-controls', id);
    caret.setAttribute('aria-label', 'Choose a different download');
    caret.appendChild(el('span', 'dl-arrow'));
    caret.firstChild.setAttribute('aria-hidden', 'true');
    split.appendChild(caret);

    var menu = el('div', 'dl-menu');
    menu.id = id;
    menu.setAttribute('role', 'menu');
    menu.setAttribute('aria-label', 'Downloads for ' + m.product);
    menu.hidden = true;
    var items = [];
    groups(m).forEach(function (g) {
      var box = el('div', 'dl-group');
      box.setAttribute('role', 'group');
      box.setAttribute('aria-label', g.title);
      var h = el('div', 'dl-gh', g.title);
      h.setAttribute('aria-hidden', 'true');
      box.appendChild(h);
      if (g.soon) {
        var soon = el('div', 'dl-item is-soon');
        soon.setAttribute('role', 'menuitem');
        soon.setAttribute('aria-disabled', 'true');
        soon.appendChild(el('span', 'dl-t', g.soon.title + ' (coming soon)'));
        soon.appendChild(el('span', 'dl-d', g.soon.desc));
        box.appendChild(soon);
      } else {
        ['installer', 'portable'].forEach(function (type) {
          g.items.filter(function (o) { return o.type === type; }).forEach(function (o) {
            var b = el('button', 'dl-item');
            b.type = 'button';
            b.setAttribute('role', 'menuitemradio');
            b.setAttribute('aria-checked', opt && o.id === opt.id ? 'true' : 'false');
            b.tabIndex = -1;
            b.appendChild(el('span', 'dl-t', o.title + ' · ' + o.size));
            b.appendChild(el('span', 'dl-d', o.desc));
            b.addEventListener('click', function () {
              store('dl-choice-' + m.product.toLowerCase(), o.id);
              render(root, m, o);
              var c = root.querySelector('.dl-caret');
              if (c) c.focus();
            });
            items.push(b);
            box.appendChild(b);
          });
        });
      }
      menu.appendChild(box);
    });
    split.appendChild(menu);
    out.appendChild(split);

    function isOpen() { return !menu.hidden; }
    function close(focus) {
      menu.hidden = true;
      caret.setAttribute('aria-expanded', 'false');
      if (focus) caret.focus();
    }
    function open() {
      menu.hidden = false;
      caret.setAttribute('aria-expanded', 'true');
      var first = items.filter(function (b) { return b.getAttribute('aria-checked') === 'true'; })[0] || items[0];
      if (first) first.focus();
    }
    function toggle(e) { if (e) e.preventDefault(); if (isOpen()) close(true); else open(); }
    caret.addEventListener('click', toggle);
    if (!opt) main.addEventListener('click', toggle);
    caret.addEventListener('keydown', function (e) {
      if (e.key === 'ArrowDown') { e.preventDefault(); open(); }
    });
    menu.addEventListener('keydown', function (e) {
      var i = items.indexOf(document.activeElement);
      if (e.key === 'ArrowDown') { e.preventDefault(); items[(i + 1) % items.length].focus(); }
      else if (e.key === 'ArrowUp') { e.preventDefault(); items[(i - 1 + items.length) % items.length].focus(); }
      else if (e.key === 'Home') { e.preventDefault(); items[0].focus(); }
      else if (e.key === 'End') { e.preventDefault(); items[items.length - 1].focus(); }
      else if (e.key === 'Escape') { e.preventDefault(); close(true); }
      else if (e.key === 'Tab') { close(false); }
    });
    root._outside = root._outside || function (e) {
      if (!root.contains(e.target)) { var mm = root.querySelector('.dl-menu'); if (mm && !mm.hidden) { mm.hidden = true; var c = root.querySelector('.dl-caret'); if (c) c.setAttribute('aria-expanded', 'false'); } }
    };
    document.removeEventListener('click', root._outside);
    document.addEventListener('click', root._outside);

    if (opt) {
      out.appendChild(el('p', 'dl-file', opt.file + ' (' + opt.size + ')' + (opt.note ? '. ' + opt.note : '')));
      var ol = el('ol', 'dl-steps');
      opt.steps.forEach(function (s) { ol.appendChild(el('li', '', s)); });
      out.appendChild(ol);
    }

    if (m.advanced && m.advanced.length) {
      var adv = el('div', 'dl-advanced');
      adv.appendChild(el('h4', '', 'For advanced users'));
      var al = el('ul');
      m.advanced.forEach(function (it) {
        var li = el('li');
        li.appendChild(link(it.url, it.label));
        al.appendChild(li);
      });
      adv.appendChild(al);
      out.appendChild(adv);
    }

    root.textContent = '';
    root.appendChild(out);
    root.classList.add('dl-ready');
  }

  function start(root) {
    var inline = root.getAttribute('data-dl-manifest');
    var src = root.getAttribute('data-dl-src');
    function go(m) {
      if (!m || !m.options) return;
      var cpu = navigator.userAgentData && navigator.userAgentData.getHighEntropyValues
        ? navigator.userAgentData.getHighEntropyValues(['architecture']).then(function (v) { return v.architecture; })
        : Promise.resolve('');
      cpu.catch(function () { return ''; }).then(function (a) {
        root._cpu = a;
        var saved = store('dl-choice-' + m.product.toLowerCase());
        var opt = null;
        (m.options || []).forEach(function (o) { if (o.id === saved) opt = o; });
        render(root, m, opt);
      });
    }
    try {
      if (inline) go(JSON.parse(inline));
      else if (src && window.fetch) fetch(src, { credentials: 'omit' }).then(function (r) { return r.ok ? r.json() : null; }).then(go).catch(function () {});
    } catch (e) { /* keep the plain list */ }
  }

  if (typeof module !== 'undefined' && module.exports) {
    module.exports = { detect: detect, choose: choose };
  }
  if (typeof document !== 'undefined') {
    var run = function () {
      var roots = document.querySelectorAll('.dl[data-dl-manifest], .dl[data-dl-src]');
      for (var i = 0; i < roots.length; i++) start(roots[i]);
    };
    if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', run);
    else run();
  }
})();
