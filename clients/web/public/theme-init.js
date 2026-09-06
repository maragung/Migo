/*
 * The pre-paint theme restore, as a same-origin file the root layout loads at the top of <body>.
 *
 * This is `themeInitScript` from src/lib/theme.ts standing on its own. It used to be inlined into
 * the layout's HTML, but the client's Content-Security-Policy (brief §57, §108, §164) admits no
 * inline script, and the job this script does — restore the stored choice before the first frame
 * paints, so a light-theme visitor never sees one dark frame — is exactly what a hash or nonce
 * exception would make fragile: an inline copy would have to be pinned per build. A same-origin
 * <script src> keeps both properties at once: it still runs synchronously during parse, and
 * `script-src 'self'` covers it.
 *
 * The key, the query, and the default are spelled out here on purpose — the same reasons
 * theme.ts gives: it must execute before the bundle loads, so it cannot import the module.
 * test/theme.test.ts runs this file and the module's own string through the same harness, so
 * the two cannot drift apart.
 */
(function () {
  function resolve(choice) {
    if (choice === 'light' || choice === 'dark') {
      return choice;
    }
    try {
      if (window.matchMedia && window.matchMedia('(prefers-color-scheme: light)').matches) {
        return 'light';
      }
    } catch (error) {}
    return 'dark';
  }
  try {
    var choice = window.localStorage.getItem('migo:theme');
    if (choice !== 'light' && choice !== 'dark' && choice !== 'system') {
      choice = 'dark';
    }
    document.documentElement.setAttribute('data-theme', resolve(choice));
  } catch (error) {
    document.documentElement.setAttribute('data-theme', 'dark');
  }
})();
