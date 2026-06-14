// Paper / midnight theme toggle. Persists the choice to localStorage; the no-FOUC <head> script in
// layout.html.tera re-applies it before first paint on every page load. No framework — a few lines
// of vanilla JS (UI mandate: server-rendered HTML + CSS, light JS only).
(function () {
  function current() {
    return document.documentElement.getAttribute('data-theme') || 'paper';
  }
  function apply(theme) {
    document.documentElement.setAttribute('data-theme', theme);
    try {
      localStorage.setItem('cortex-theme', theme);
    } catch (e) {}
    var toggle = document.getElementById('theme-toggle');
    if (toggle) {
      var next = theme === 'midnight' ? 'paper' : 'midnight';
      toggle.title = 'Switch to the ' + next + ' theme';
      toggle.setAttribute('aria-pressed', theme === 'midnight' ? 'true' : 'false');
    }
  }
  document.addEventListener('DOMContentLoaded', function () {
    apply(current());
    var toggle = document.getElementById('theme-toggle');
    if (toggle) {
      toggle.addEventListener('click', function (e) {
        e.preventDefault();
        apply(current() === 'midnight' ? 'paper' : 'midnight');
      });
    }
  });
})();
