/// Per-tab init script: reports shortcuts, link hovers, favicons and implements find-in-page.
///
/// `token` is baked into the closure (it is never stored on `window`), so a page
/// cannot read another tab's token and forge events pretending to come from it.
pub fn init_script(id: u32, token: &str) -> String {
  format!(
    "window.__NANO_TAB_ID__={};\n{}",
    id,
    TEMPLATE.replace(TEMPLATE_TOKEN, token)
  )
}

const TEMPLATE_TOKEN: &str = "__NANO_TOKEN__";

const TEMPLATE: &str = r#"
(function(){
  if (window.top !== window) return;
  var TAB = window.__NANO_TAB_ID__;
  var TOK = '__NANO_TOKEN__';
  function post(name, data){
    try {
      if (window.__TAURI__) { data.id = TAB; data.tok = TOK; window.__TAURI__.event.emit('nano://page-' + name, data); }
    } catch(e) {}
  }

  /* ---------- favicon ---------- */
  function reportIcon(){
    try {
      var l = document.querySelector('link[rel~="icon"]') || document.querySelector('link[rel="shortcut icon"]');
      post('icon', { icon: l ? l.href : (location.origin + '/favicon.ico') });
    } catch(e) {}
  }
  if (document.readyState === 'complete') reportIcon();
  else document.addEventListener('DOMContentLoaded', reportIcon);

  /* ---------- link hover (status bubble) ---------- */
  var hoverUrl = '';
  var lastPost = 0;
  document.addEventListener('mouseover', function(e){
    try {
      var a = e.target && e.target.closest ? e.target.closest('a[href]') : null;
      var now = Date.now();
      if (a && a.href && a.href !== hoverUrl && now - lastPost > 120) {
        hoverUrl = a.href; lastPost = now; post('hover', { url: hoverUrl });
      } else if (!a && hoverUrl) {
        hoverUrl = ''; lastPost = now; post('hover', { url: '' });
      }
    } catch(err) {}
  }, true);

  /* ---------- find in page (CSS Custom Highlights) ---------- */
  // NOTE: this script runs at document creation, when documentElement/head do
  // not exist yet, so the stylesheet has to be injected lazily.
  function ensureFindStyle(){
    try {
      if (document.getElementById('__nano-find-style')) return;
      var root = document.head || document.documentElement || document.body;
      if (!root) return;
      var st = document.createElement('style');
      st.id = '__nano-find-style';
      st.textContent = '::highlight(nanofind){background:#ffe082;color:#202124}::highlight(nanofindactive){background:#fb8c00;color:#fff}';
      root.appendChild(st);
    } catch(e) {}
  }
  document.addEventListener('DOMContentLoaded', ensureFindStyle);
  window.addEventListener('load', ensureFindStyle);

  var findRanges = [], findIndex = 0, findQuery = '';
  window.__nanoFind = function(o){
    try {
      ensureFindStyle();
      CSS.highlights.delete('nanofind'); CSS.highlights.delete('nanofindactive');
      findRanges = []; findIndex = o.index || 0; findQuery = o.query || '';
      if (findQuery) {
        var q = findQuery.toLowerCase();
        var walker = document.createTreeWalker(document.body || document.documentElement, NodeFilter.SHOW_TEXT, {
          acceptNode: function(n){
            if (!n.nodeValue || !n.nodeValue.trim()) return NodeFilter.FILTER_REJECT;
            var p = n.parentElement;
            if (!p) return NodeFilter.FILTER_REJECT;
            var t = p.tagName;
            if (t === 'SCRIPT' || t === 'STYLE' || t === 'NOSCRIPT' || t === 'IFRAME' || t === 'svg') return NodeFilter.FILTER_REJECT;
            return NodeFilter.FILTER_ACCEPT;
          }
        });
        var ns = [];
        while (walker.nextNode()) { ns.push(walker.currentNode); if (ns.length > 8000) break; }
        for (var i = 0; i < ns.length && findRanges.length < 2000; i++) {
          var s = ns[i].nodeValue.toLowerCase();
          var j = s.indexOf(q);
          while (j !== -1 && findRanges.length < 2000) {
            var r = document.createRange();
            r.setStart(ns[i], j); r.setEnd(ns[i], j + findQuery.length);
            findRanges.push(r);
            j = s.indexOf(q, j + findQuery.length);
          }
        }
        if (findRanges.length) {
          if (typeof Highlight === 'function') {
            var hl = new (Function.prototype.bind.apply(Highlight, [null].concat(findRanges)));
            CSS.highlights.set('nanofind', hl);
            var act = new Highlight(findRanges[findIndex]);
            CSS.highlights.set('nanofindactive', act);
          }
          var rr = findRanges[findIndex].getBoundingClientRect();
          window.scrollTo(0, rr.top + window.scrollY - window.innerHeight / 2);
        }
      }
      post('find', { count: findRanges.length, index: findRanges.length ? (findIndex % findRanges.length) : 0 });
    } catch(e) { post('find', { count: 0, index: 0 }); }
  };
  window.__nanoFindMove = function(delta){
    try {
      if (!findRanges.length) return;
      findIndex = (findIndex + delta) % findRanges.length;
      if (findIndex < 0) findIndex += findRanges.length;
      if (typeof Highlight === 'function') {
        CSS.highlights.delete('nanofindactive');
        CSS.highlights.set('nanofindactive', new Highlight(findRanges[findIndex]));
      }
      var rr = findRanges[findIndex].getBoundingClientRect();
      window.scrollTo(0, rr.top + window.scrollY - window.innerHeight / 2);
      post('find', { count: findRanges.length, index: findIndex });
    } catch(e) {}
  };
  window.__nanoFindClear = function(){
    try { CSS.highlights.delete('nanofind'); CSS.highlights.delete('nanofindactive'); findRanges = []; findQuery = ''; } catch(e) {}
  };

  /* ---------- keyboard shortcuts ---------- */
  var handled = false;
  document.addEventListener('keydown', function(e){
    if (e.key === 'F11') { post('shortcut', { key: 'f11' }); e.preventDefault(); return; }
    if (e.key === 'F5') {
      if (e.shiftKey || e.ctrlKey || e.metaKey) { post('shortcut', { key: 'hard-reload' }); e.preventDefault(); }
      return; /* plain F5 reloads natively */
    }
    if (e.key === 'F12') { post('shortcut', { key: 'f12' }); e.preventDefault(); return; }
    var c = e.ctrlKey || e.metaKey;
    if (e.altKey && !c) {
      if (e.key === 'ArrowLeft') { post('shortcut', { key: 'alt-left' }); e.preventDefault(); }
      else if (e.key === 'ArrowRight') { post('shortcut', { key: 'alt-right' }); e.preventDefault(); }
      return;
    }
    if (!c) return;
    var k = (e.key || '').toLowerCase();
    if (e.shiftKey) {
      switch (k) {
        case 'tab': post('shortcut', { key: 'ctrl-shift-tab' }); e.preventDefault(); return;
        case 't': post('shortcut', { key: 'ctrl-shift-t' }); e.preventDefault(); return;
        case 'i': post('shortcut', { key: 'ctrl-shift-i' }); e.preventDefault(); return;
        case 'o': post('shortcut', { key: 'ctrl-shift-o' }); e.preventDefault(); return;
        case 'j': post('shortcut', { key: 'ctrl-shift-j' }); e.preventDefault(); return;
        case 'a': post('shortcut', { key: 'ctrl-shift-a' }); e.preventDefault(); return;
        case 'r': post('shortcut', { key: 'hard-reload' }); e.preventDefault(); return;
      }
      if (k >= '1' && k <= '9') return; /* ctrl+shift+digit: page passthrough */
      if (k === 'delete') return;
      return;
    }
    switch (k) {
      case 't': post('shortcut', { key: 'ctrl-t' }); e.preventDefault(); return;
      case 'w': post('shortcut', { key: 'ctrl-w' }); e.preventDefault(); return;
      case 'l': post('shortcut', { key: 'ctrl-l' }); e.preventDefault(); return;
      case 'f': post('shortcut', { key: 'ctrl-f' }); e.preventDefault(); return;
      case 'd': post('shortcut', { key: 'ctrl-d' }); e.preventDefault(); return;
      case 'h': post('shortcut', { key: 'ctrl-h' }); e.preventDefault(); return;
      case 'j': post('shortcut', { key: 'ctrl-j' }); e.preventDefault(); return;
      case 'tab': post('shortcut', { key: 'ctrl-tab' }); e.preventDefault(); return;
      case '+': case '=': post('shortcut', { key: 'ctrl-zoom-in' }); e.preventDefault(); return;
      case '-': case '_': post('shortcut', { key: 'ctrl-zoom-out' }); e.preventDefault(); return;
      case '0': post('shortcut', { key: 'ctrl-zoom-reset' }); e.preventDefault(); return;
      case 'r': return; /* native reload */
      case 'p': return; /* native print */
      case 'c': case 'v': case 'x': case 'a': return; /* editing passthrough */
    }
    if (k >= '1' && k <= '9') { post('shortcut', { key: 'ctrl-' + k }); e.preventDefault(); }
  }, true);
})();
"#;
