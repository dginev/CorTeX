// Localize every server-rendered UTC timestamp to the viewer's own time zone, with the zone code
// shown (e.g. "Jun 15, 2026, 1:52 AM EDT"). The server only knows UTC; the viewer's zone is a
// browser fact, so timestamps are emitted as <time datetime="<ISO-8601 UTC>">fallback</time> and
// rewritten here. Native Intl does the zone conversion AND the abbreviation — no library, no config.
// With JS off, the server's ISO-UTC fallback text remains (unambiguous, just not localized).
(function () {
  function localize(el) {
    var iso = el.getAttribute('datetime');
    if (!iso) return;
    var d = new Date(iso);
    if (isNaN(d.getTime())) return; // leave unparseable values as-is
    try {
      el.textContent = new Intl.DateTimeFormat(undefined, {
        dateStyle: 'medium',
        timeStyle: 'short',
        timeZoneName: 'short'
      }).format(d);
      // Keep the precise UTC value reachable on hover.
      if (!el.title) el.title = iso;
    } catch (e) { /* Intl unavailable — keep the fallback text */ }
  }
  function run() {
    var nodes = document.querySelectorAll('time[datetime]');
    for (var i = 0; i < nodes.length; i++) localize(nodes[i]);
  }
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', run);
  } else {
    run();
  }
})();
