/* SpritzWine landing page — no dependencies, no external requests, works offline. */
(function () {
  "use strict";

  var reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)");

  /* ---------- theme toggle: auto -> light -> dark ---------- */

  var THEMES = ["auto", "light", "dark"];
  var toggle = document.getElementById("theme-toggle");

  function applyTheme(mode) {
    if (mode === "light" || mode === "dark") {
      document.documentElement.setAttribute("data-theme", mode);
    } else {
      document.documentElement.removeAttribute("data-theme");
    }
    if (toggle) {
      var label = { auto: "Auto", light: "Light", dark: "Dark" }[mode];
      toggle.textContent = "Theme: " + label;
      toggle.setAttribute(
        "aria-label",
        "Colour theme: " + label + ". Activate to switch."
      );
    }
  }

  var stored = null;
  try { stored = localStorage.getItem("spritzwine-theme"); } catch (e) {}
  var theme = THEMES.indexOf(stored) >= 0 ? stored : "auto";
  applyTheme(theme);

  if (toggle) {
    toggle.addEventListener("click", function () {
      theme = THEMES[(THEMES.indexOf(theme) + 1) % THEMES.length];
      try { localStorage.setItem("spritzwine-theme", theme); } catch (e) {}
      applyTheme(theme);
    });
  }

  /* ---------- smooth scroll for anchor links ---------- */

  document.addEventListener("click", function (ev) {
    var link = ev.target.closest("a[href^='#']");
    if (!link) return;
    var id = link.getAttribute("href").slice(1);
    var target = document.getElementById(id);
    if (!target) return;
    ev.preventDefault();
    target.scrollIntoView({ behavior: reducedMotion.matches ? "auto" : "smooth" });
    history.pushState(null, "", "#" + id);
    target.setAttribute("tabindex", "-1");
    target.focus({ preventScroll: true });
  });

  /* ---------- header scroll shadow ---------- */

  var header = document.querySelector(".site-header");
  if (header) {
    var scrolled = false;
    var onScroll = function () {
      var now = window.scrollY > 12;
      if (now !== scrolled) {
        scrolled = now;
        header.style.boxShadow = scrolled ? "0 4px 24px rgba(0,0,0,0.25)" : "none";
      }
    };
    window.addEventListener("scroll", onScroll, { passive: true });
    onScroll();
  }

  /* ---------- hero bubbles: the carbonation in the spritzer ----------
     Pure atmosphere, cream on wood. Honours prefers-reduced-motion by
     drawing a single still scatter, and idles when off-screen or hidden. */

  var canvas = document.getElementById("hero-bubbles");
  if (canvas) {
    var ctx = canvas.getContext("2d");
    var bubbles = [];
    var BUBBLE_COUNT = 28;
    var raf = null;

    function resize() {
      var rect = canvas.parentElement.getBoundingClientRect();
      canvas.width = rect.width * (window.devicePixelRatio || 1);
      canvas.height = rect.height * (window.devicePixelRatio || 1);
      canvas.style.width = rect.width + "px";
      canvas.style.height = rect.height + "px";
    }

    function seed() {
      bubbles = [];
      for (var i = 0; i < BUBBLE_COUNT; i++) {
        bubbles.push({
          x: Math.random() * canvas.width,
          y: Math.random() * canvas.height,
          r: 1.5 + Math.random() * 3,
          speed: 0.15 + Math.random() * 0.4,
          drift: (Math.random() - 0.5) * 0.3,
          alpha: 0.08 + Math.random() * 0.14
        });
      }
    }

    function draw() {
      ctx.clearRect(0, 0, canvas.width, canvas.height);
      for (var i = 0; i < bubbles.length; i++) {
        var b = bubbles[i];
        ctx.beginPath();
        ctx.arc(b.x, b.y, b.r * (window.devicePixelRatio || 1), 0, Math.PI * 2);
        ctx.fillStyle = "rgba(244, 237, 228, " + b.alpha + ")";
        ctx.fill();
      }
    }

    function tick() {
      for (var i = 0; i < bubbles.length; i++) {
        var b = bubbles[i];
        b.y -= b.speed * (window.devicePixelRatio || 1);
        b.x += b.drift * (window.devicePixelRatio || 1);
        // respawn at the bottom when a bubble leaves the top
        if (b.y < -b.r * 2) {
          b.y = canvas.height + b.r * 2;
          b.x = Math.random() * canvas.width;
        }
      }
      draw();
      raf = requestAnimationFrame(tick);
    }

    resize();
    seed();

    if (reducedMotion.matches) {
      draw(); // static scatter — no animation
    } else {
      tick();
    }

    // visibility and motion-preference hooks
    var isVisible = true;
    var heroObs = new IntersectionObserver(function (entries) {
      isVisible = entries[0].isIntersecting;
      if (isVisible) {
        if (!raf && !reducedMotion.matches) tick();
      } else {
        if (raf) { cancelAnimationFrame(raf); raf = null; }
      }
    }, { threshold: 0 });
    heroObs.observe(canvas.parentElement);

    reducedMotion.addEventListener("change", function () {
      if (reducedMotion.matches) {
        if (raf) { cancelAnimationFrame(raf); raf = null; }
        draw();
      } else {
        if (isVisible && !raf) tick();
      }
    });

    document.addEventListener("visibilitychange", function () {
      if (document.hidden) {
        if (raf) { cancelAnimationFrame(raf); raf = null; }
      } else {
        if (isVisible && !reducedMotion.matches && !raf) tick();
      }
    });

    window.addEventListener("resize", function () {
      if (raf) { cancelAnimationFrame(raf); raf = null; }
      resize(); seed();
      if (reducedMotion.matches) {
        draw();
      } else if (isVisible) {
        tick();
      }
    });
  }

  /* ---------- copy button ---------- */

  var copyBtn = document.getElementById("copy-cmd");
  if (copyBtn) {
    copyBtn.addEventListener("click", function () {
      var text = "git clone --recursive https://codeberg.org/SpritzWine/SpritzWine.git";
      function done(ok) {
        copyBtn.textContent = ok ? "Copied" : "Select it manually";
        setTimeout(function () { copyBtn.textContent = "Copy"; }, 1800);
      }
      if (navigator.clipboard && navigator.clipboard.writeText) {
        navigator.clipboard.writeText(text).then(
          function () { done(true); },
          function () { done(false); }
        );
      } else {
        done(false);
      }
    });
  }

  /* ---------- release notes: releasenotes.md, rendered client-side ----------
     Same-origin, relative path — the no-external-requests rule holds. Under
     file:// (or on any fetch failure) the section keeps its fallback link to
     the Codeberg releases page. The renderer is Beisl's: escape first, then a
     small, closed markdown subset — no raw HTML from the file ever reaches
     innerHTML unescaped. */

  function mdInline(s) {
    return s
      .replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;")
      .replace(/`([^`]+)`/g, "<code>$1</code>")
      .replace(/\*\*([^*]+)\*\*/g, "<b>$1</b>")
      .replace(/\[([^\]]+)\]\(([^)\s]+)\)/g, '<a href="$2" rel="external">$1</a>')
      .replace(/\*([^*]+)\*/g, "<em>$1</em>");
  }

  function mdRender(md) {
    /* The subset a release-notes file actually uses: #/##/### headings, ---,
       - lists (with indented continuation lines), paragraphs, and the inline
       set above. Anything fancier belongs in the repo file, not here. */
    var out = [], inList = false, para = [];
    function flushPara() {
      if (para.length) { out.push("<p>" + mdInline(para.join(" ")) + "</p>"); para = []; }
    }
    function closeList() { if (inList) { out.push("</ul>"); inList = false; } }
    md.split(/\r?\n/).forEach(function (line) {
      var h = line.match(/^(#{1,3})\s+(.*)$/);
      if (h) {
        flushPara(); closeList();
        var lvl = h[1].length + 2; /* # -> h3: the section already owns the h2 */
        out.push("<h" + lvl + ">" + mdInline(h[2]) + "</h" + lvl + ">");
        return;
      }
      if (/^---+\s*$/.test(line)) { flushPara(); closeList(); out.push("<hr>"); return; }
      if (/^[-*]\s+/.test(line)) {
        flushPara();
        if (!inList) { out.push("<ul>"); inList = true; }
        out.push("<li>" + mdInline(line.replace(/^[-*]\s+/, "")) + "</li>");
        return;
      }
      if (inList && /^\s+\S/.test(line)) {
        out[out.length - 1] =
          out[out.length - 1].replace(/<\/li>$/, " " + mdInline(line.trim()) + "</li>");
        return;
      }
      if (/^\s*$/.test(line)) { flushPara(); closeList(); return; }
      para.push(line.trim());
    });
    flushPara(); closeList();
    return out.join("\n");
  }

  var notesHost = document.getElementById("release-notes");
  if (notesHost && window.fetch && location.protocol !== "file:") {
    fetch("releasenotes.md")
      .then(function (r) {
        if (!r.ok) throw new Error("HTTP " + r.status);
        return r.text();
      })
      .then(function (md) {
        notesHost.innerHTML = mdRender(md);
        /* Two releases worth of height, then scroll: each release opens with an
           h3, so the 3rd h3's top is exactly the cut line. With <= 2 releases
           the box stays uncapped (there is nothing to scroll to). */
        var heads = notesHost.querySelectorAll("h3");
        if (heads.length > 2) {
          notesHost.style.maxHeight = heads[2].offsetTop + "px";
          notesHost.classList.add("scrolls");
        }
      })
      .catch(function () { /* the fallback link stays */ });
  }
})();
