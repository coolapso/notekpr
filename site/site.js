/* notekpr website: the moving parts. Hand-written, no dependencies, nothing
   loaded from elsewhere. All of it is optional: without it (or with reduced
   motion) every word and picture is there, just still. */
(function () {
  "use strict";
  var d = document;
  var reduce = window.matchMedia && matchMedia("(prefers-reduced-motion: reduce)").matches;
  var HEX = "0123456789abcdef";

  function hex(n) {
    var s = "";
    for (var i = 0; i < n; i++) s += HEX[(Math.random() * 16) | 0];
    return s;
  }

  // ------------------------------------------------------------ reveal on scroll

  d.querySelectorAll(".draw-in path").forEach(function (p) {
    if (p.getTotalLength) p.style.setProperty("--len", Math.ceil(p.getTotalLength()) + 1);
  });
  var targets = d.querySelectorAll(".reveal, .draw-in");
  if ("IntersectionObserver" in window && !reduce) {
    var io = new IntersectionObserver(function (entries) {
      entries.forEach(function (e) {
        if (e.isIntersecting) {
          e.target.classList.add("in");
          io.unobserve(e.target);
        }
      });
    }, { rootMargin: "0px 0px -8% 0px" });
    targets.forEach(function (el) { io.observe(el); });
  } else {
    targets.forEach(function (el) { el.classList.add("in"); });
  }

  // ------------------------------------------------------------ "noise"

  // Now and then the word turns into what the server would store of it.
  var noise = d.getElementById("noise");
  if (noise && !reduce) {
    var word = noise.querySelector(".w").textContent;
    setInterval(function () {
      var f = 0;
      (function step() {
        f++;
        if (f < 12) {
          noise.setAttribute("data-s", hex(word.length));
          setTimeout(step, 60);
        } else {
          noise.removeAttribute("data-s");
        }
      })();
    }, 4500);
  }

  // ------------------------------------------------------------ the app in the hero

  var app = d.getElementById("app");
  if (app) heroApp(app);

  function heroApp(app) {
    var note = app.querySelector("#note");
    var rows = app.querySelector("#rows");
    var count = app.querySelector("#count");
    var you = app.querySelector(".seg2 .you");
    var srv = app.querySelector(".seg2 .srv");
    var stored = 2481;
    var touched = false;

    function view(v) {
      app.setAttribute("data-view", v);
      you.setAttribute("aria-pressed", String(v === "you"));
      srv.setAttribute("aria-pressed", String(v === "server"));
    }
    you.addEventListener("click", function () { touched = true; view("you"); });
    srv.addEventListener("click", function () { touched = true; view("server"); });

    // One stored update: what the server keeps of an edit.
    function update(len) {
      var div = d.createElement("div");
      div.innerHTML = '<span class="v">01</span> <span class="e">00000002</span> <span class="n">' + hex(48) +
        "</span> " + hex(len) + ' <span class="t">' + hex(32) + "</span>";
      rows.insertBefore(div, rows.firstChild);
      while (rows.children.length > 14) rows.removeChild(rows.lastChild);
      stored++;
      count.textContent = stored.toLocaleString("en") + " rows · 0 readable";
    }

    if (reduce) return;

    // Lines typed by Alice and Bob: their final content, kept as templates.
    var lines = [].slice.call(note.querySelectorAll(".line"));
    var templates = lines.map(function (l) { return l.cloneNode(true); });
    var callout = note.querySelector(".callout");
    var carets = { alice: caret(""), bob: caret("bob") };

    function caret(who) {
      var c = d.createElement("span");
      c.className = "caret " + who;
      c.setAttribute("aria-hidden", "true");
      return c;
    }

    // Line i with only its first k characters, formatting and all.
    function show(i, k, who) {
      var dst = lines[i];
      var left = k;
      dst.textContent = "";
      (function copy(from, to) {
        for (var c = from.firstChild; c && left > 0; c = c.nextSibling) {
          if (c.nodeType === 3) {
            var t = c.nodeValue.slice(0, left);
            left -= t.length;
            to.appendChild(d.createTextNode(t));
          } else {
            var e = c.cloneNode(false);
            to.appendChild(e);
            copy(c, e);
          }
        }
      })(templates[i], dst);
      if (who) {
        var inner = dst.querySelector(".txt") || dst;
        inner.appendChild(carets[who]);
      }
    }

    function length(i) { return templates[i].textContent.length; }

    var timers = [];
    function later(ms, f) { timers.push(setTimeout(f, ms)); }

    // A typist: types lines one after the other, a little unevenly.
    function typist(who, queue, done) {
      var q = queue.slice();
      (function next() {
        var job = q.shift();
        if (!job) { carets[who].remove(); done(); return; }
        var i = job.line, k = 0, n = length(i);
        later(job.wait || 0, function tick() {
          k++;
          show(i, k, who);
          if (k % 6 === 0 || k === n) update(40 + ((Math.random() * 90) | 0));
          if (k < n) later(38 + Math.random() * 85 + (templates[i].textContent[k] === " " ? 40 : 0), tick);
          else later(job.pause || 350, function () { if (job.after) job.after(); next(); });
        });
      })();
    }

    function reset() {
      timers.forEach(clearTimeout);
      timers = [];
      lines.forEach(function (l, i) { show(i, 0); l.classList.remove("done"); });
      callout.style.opacity = "0";
      callout.style.transition = "opacity .5s ease";
    }

    function run() {
      reset();
      var left = 2;
      function both() {
        if (--left) return;
        callout.style.opacity = "1";
        update(120);
        later(2600, function () {
          if (!touched) view("server");
          later(touched ? 2000 : 5200, function () {
            if (!touched) view("you");
            later(1200, run);
          });
        });
      }
      typist("alice", [
        { line: 0, wait: 700, pause: 500, after: function () { lines[0].classList.add("done"); update(48); } },
        { line: 1, wait: 200 },
        { line: 3, wait: 300 }
      ], both);
      typist("bob", [{ line: 2, wait: 2600 }], both);
    }

    // Start when the app is on screen, once.
    if ("IntersectionObserver" in window) {
      var seen = new IntersectionObserver(function (es) {
        if (es[0].isIntersecting) { seen.disconnect(); run(); }
      }, { threshold: 0.3 });
      seen.observe(app);
    } else {
      run();
    }
  }
})();
