/* Mununu — theme toggle + EN/ES language switch + subtle reveal.
   No tracking, no dependencies. English is the source of truth in the HTML;
   Spanish is applied from a dictionary, and switching back to English restores
   the original markup captured from the page. */
(function () {
  var root = document.documentElement;
  var THEME_KEY = "mununu-theme";
  var LANG_KEY  = "mununu-lang";

  function getLS(k) { try { return localStorage.getItem(k); } catch (e) { return null; } }
  function setLS(k, v) { try { localStorage.setItem(k, v); } catch (e) {} }

  /* ---------- Theme ---------- */

  var LABELS = {
    en: { dark: "Dark theme", light: "Light theme" },
    es: { dark: "Tema oscuro", light: "Tema claro" }
  };
  function systemTheme() {
    return window.matchMedia && window.matchMedia("(prefers-color-scheme: dark)").matches
      ? "dark" : "light";
  }
  function currentLang() { return root.getAttribute("lang") === "es" ? "es" : "en"; }
  function applyTheme(theme) {
    root.setAttribute("data-theme", theme);
    var btn = document.querySelector(".theme-toggle");
    if (btn) {
      var l = LABELS[currentLang()] || LABELS.en;
      btn.textContent = theme === "dark" ? l.light : l.dark;
      btn.setAttribute("aria-pressed", theme === "dark" ? "true" : "false");
    }
  }

  /* ---------- Language ---------- */

  // Chrome shared by every page (nav, footer, evidence legend, common CTAs).
  // Page-specific copy is supplied per page as window.MununuEs and merged over this.
  var SHARED_ES = {
    "skip": "Saltar al contenido",
    "nav.defense": "Defensa",
    "nav.aerospace": "Aeroespacial",
    "nav.airtl": "RTL generado por IA",
    "nav.docs": "Documentación",
    "nav.contact": "Contacto",
    "nav.blog": "Blog",
    "cta.readdocs": "Leer la documentación",
    "tech.more": "Detalle técnico",
    "legend.title": "Cómo leer nuestra evidencia",
    "legend.bounded": '<span class="chip chip--bounded">BOUNDED</span><span>Verificación acotada de modelos: válida solo hasta la profundidad indicada.</span>',
    "legend.demo": '<span class="chip chip--demo">DEMO</span><span>Ejemplo construido. Ilustra la capacidad; no es un hallazgo en silicio real.</span>',
    "footer.honest": "— software temprano, veredictos honestos.",
    "footer.soundness": "Solidez del núcleo verificada por máquina en Lean 4 e Isabelle/HOL.",
    "footer.inception": "Miembro del programa NVIDIA Inception.",
    "footer.source": "Código fuente",
    "footer.copyright": "© 2026 Mununu. Todos los derechos reservados."
  };

  var originals = {};
  var captured = false;
  function captureOriginals() {
    if (captured) return;
    document.querySelectorAll("[data-i18n]").forEach(function (el) {
      var k = el.getAttribute("data-i18n");
      if (!(k in originals)) originals[k] = el.innerHTML;
    });
    captured = true;
  }
  function applyLang(lang) {
    captureOriginals();
    var dict = lang === "es" ? Object.assign({}, SHARED_ES, window.MununuEs || {}) : null;
    document.querySelectorAll("[data-i18n]").forEach(function (el) {
      var k = el.getAttribute("data-i18n");
      var val = (dict && dict[k] != null) ? dict[k] : originals[k];
      if (val != null) el.innerHTML = val;
    });
    root.setAttribute("lang", lang);
    document.querySelectorAll(".lang-btn").forEach(function (b) {
      b.setAttribute("aria-pressed", b.getAttribute("data-lang") === lang ? "true" : "false");
    });
    // Refresh the theme-toggle label in the newly selected language.
    applyTheme(root.getAttribute("data-theme") || systemTheme());
  }

  /* ---------- Init (deferred script: the DOM is parsed at this point) ---------- */

  applyTheme(getLS(THEME_KEY) || systemTheme());
  applyLang(getLS(LANG_KEY) === "es" ? "es" : "en");

  // Follow OS theme changes unless the user has chosen manually.
  if (window.matchMedia) {
    window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", function (e) {
      if (!getLS(THEME_KEY)) applyTheme(e.matches ? "dark" : "light");
    });
  }

  var themeBtn = document.querySelector(".theme-toggle");
  if (themeBtn) {
    themeBtn.addEventListener("click", function () {
      var next = root.getAttribute("data-theme") === "dark" ? "light" : "dark";
      setLS(THEME_KEY, next);
      applyTheme(next);
    });
  }

  document.querySelectorAll(".lang-btn").forEach(function (btn) {
    btn.addEventListener("click", function () {
      var next = btn.getAttribute("data-lang") === "es" ? "es" : "en";
      setLS(LANG_KEY, next);
      applyLang(next);
    });
  });

  // Subtle scroll reveal; CSS already no-ops under prefers-reduced-motion.
  if ("IntersectionObserver" in window) {
    var io = new IntersectionObserver(function (entries) {
      entries.forEach(function (en) {
        if (en.isIntersecting) { en.target.classList.add("in"); io.unobserve(en.target); }
      });
    }, { rootMargin: "0px 0px -8% 0px" });
    document.querySelectorAll(".reveal").forEach(function (el) { io.observe(el); });
  } else {
    document.querySelectorAll(".reveal").forEach(function (el) { el.classList.add("in"); });
  }
})();
