// ---------------------------------------------------------------------------------------------
// tini story page. One fixed notch at the top center guides the whole page: it changes state per
// chapter, springs between shapes like the app (geometry.rs), and the agent card can be answered.
// GSAP + ScrollTrigger + SplitText drive the scroll story and Lenis smooths the wheel. Without
// those libraries the page still reads top to bottom (an IntersectionObserver drives the guide).
// prefers-reduced-motion: no loader, no pins, no scrub, everything in its final state.
// ---------------------------------------------------------------------------------------------
(() => {
  const html = document.documentElement;
  const reduce = matchMedia("(prefers-reduced-motion: reduce)").matches;
  const G = window.gsap && window.ScrollTrigger ? window.gsap : null;
  if (G) { html.classList.add("gsap"); G.registerPlugin(ScrollTrigger); if (window.SplitText) G.registerPlugin(SplitText); }
  const NS = "http://www.w3.org/2000/svg";
  const rand = (a, b) => a + Math.random() * (b - a);
  const clamp = (v, a, b) => Math.max(a, Math.min(b, v));
  const $ = (s, r = document) => r.querySelector(s);
  const $$ = (s, r = document) => [...r.querySelectorAll(s)];


  // ------------------------------------------------------------------------- countdown
  // One constant: Oct 30 2026, 19:00 WIB (12:00 UTC). ?launched previews the "out now" state.
  const LAUNCH = new URLSearchParams(location.search).has("launched") ? Date.now() - 1000 : Date.UTC(2026, 9, 30, 12, 0, 0);
  let cdTimer = 0;
  function tickCountdown() {
    const ms = LAUNCH - Date.now();
    const done = ms <= 0;
    $$("[data-countdown]").forEach((e) => { e.hidden = done; });
    $$("[data-cd-done]").forEach((e) => { e.hidden = !done; });
    $$("[data-cd-short]").forEach((e) => { e.textContent = done ? "Out now" : ""; });
    if (done) { clearInterval(cdTimer); $$("[data-cd-kicker]").forEach((e) => { e.textContent = "tini is here"; }); return; }
    const d = Math.floor(ms / 864e5), h = Math.floor(ms / 36e5) % 24, m = Math.floor(ms / 6e4) % 60;
    const set = (k, v) => $$(`[data-cd="${k}"]`).forEach((e) => { e.textContent = v; });
    set("d", d); set("h", String(h).padStart(2, "0")); set("m", String(m).padStart(2, "0"));
    $$("[data-cd-inline]").forEach((e) => { e.textContent = `${d}d ${h}h ${m}m to go`; });
    $$("[data-cd-short]").forEach((e) => { e.textContent = `${d}d ${h}h ${m}m`; });
  }

  // ------------------------------------------------------------------------- frame loop
  // One rAF loop for the springs and eyes; it sleeps when nothing visible needs a frame.
  const tickers = new Set();
  let raf = 0, last = 0;
  function loop(t) {
    const dt = last ? Math.min(0.05, (t - last) / 1000) : 1 / 60;
    last = t;
    let busy = false;
    for (const o of tickers) if (o.active && o.tick(t, dt)) busy = true;
    raf = busy && !document.hidden ? requestAnimationFrame(loop) : 0;
    if (!raf) last = 0;
  }
  const wake = () => { if (!raf && !reduce && !document.hidden) raf = requestAnimationFrame(loop); };
  document.addEventListener("visibilitychange", wake);
  const seen = new IntersectionObserver((entries) => {
    for (const e of entries) for (const o of e.target.__live || []) { o.active = e.isIntersecting; o.onVisible && o.onVisible(e.isIntersecting); }
    wake();
  }, { rootMargin: "60px" });
  function watch(el, o) { (el.__live = el.__live || []).push(o); o.active = false; seen.observe(el); }

  let px = -1e5, py = -1e5;
  addEventListener("pointermove", (e) => { px = e.clientX; py = e.clientY; wake(); }, { passive: true });

  // ------------------------------------------------------------------------- Nochi
  // Eye shapes from the app (pet.rs): [eye width, eye height, half distance between eyes].
  const SHAPES = { idle: [5, 9, 5.5], curious: [5, 9, 5.5], focused: [5, 5, 5.5], alert: [8, 8, 6.5], sleepy: [9, 2.2, 6.5], zen: [6, 3.6, 5.5] };
  const svgEl = (tag, attrs) => { const n = document.createElementNS(NS, tag); for (const k in attrs) n.setAttribute(k, attrs[k]); return n; };

  class Nochi {
    constructor(host, { hoverEl, clickEl, always } = {}) {
      this.host = host;
      this.k = +host.dataset.k || 1;
      this.mood = host.dataset.mood || "idle";
      this.follow = host.dataset.follow !== "off";
      this.nod = host.dataset.nod === "on";
      const svg = svgEl("svg", { viewBox: "-24 -14 48 28", width: 48 * this.k, height: 28 * this.k, "aria-hidden": "true" });
      this.g = svgEl("g", {});
      this.eyes = [-1, 1].map(() => svgEl("rect", { fill: "#F5F5F7" }));
      this.arcs = [-6, 6].map((cx) => svgEl("path", { d: `M ${cx - 3.6} 1.8 A 3.6 3.6 0 0 1 ${cx + 3.6} 1.8`, fill: "none", stroke: "#F5F5F7", "stroke-width": 2.2, "stroke-linecap": "round" }));
      this.g.append(...this.eyes, ...this.arcs);
      svg.append(this.g);
      host.replaceChildren(svg);
      const now = performance.now();
      this.look = [0, 0]; this.target = [0, 0];
      this.blinkAt = now + rand(600, 3000); this.blinkT = -1;
      this.sacAt = now + rand(800, 2000); this.hopT = -1; this.happy = false;
      this.gazeUntil = 0;
      this.rect = null; this.rectAt = 0;
      const h = hoverEl || host, c = clickEl || host;
      h.addEventListener("pointerenter", () => { this.happy = true; this.render(performance.now()); wake(); });
      h.addEventListener("pointerleave", () => { this.happy = false; this.render(performance.now()); wake(); });
      c.addEventListener("click", () => this.hop());
      this.render(now);
      tickers.add(this);
      if (always) this.active = true; else watch(host, this);
    }
    setMood(m) { this.mood = m; this.render(performance.now()); wake(); }
    hop() { this.hopT = performance.now(); wake(); }
    blink() { this.blinkAt = performance.now(); wake(); }
    gaze(x, y, ms) { this.target = [x, y]; this.gazeUntil = performance.now() + ms; wake(); }
    tick(t) {
      if (this.blinkT < 0 && t > this.blinkAt) {
        this.blinkT = t;
        const slow = this.mood === "zen" || this.mood === "sleepy";
        this.blinkAt = t + (Math.random() < 0.15 ? 420 : rand(slow ? 4500 : 2400, slow ? 8000 : 6000));
      }
      if (this.blinkT >= 0 && t - this.blinkT > 380) this.blinkT = -1;
      if (!this.rect || t - this.rectAt > 400) { this.rect = this.host.getBoundingClientRect(); this.rectAt = t; }
      const cx = this.rect.left + this.rect.width / 2, cy = this.rect.top + this.rect.height / 2;
      const dx = px - cx, dy = py - cy;
      if (t < this.gazeUntil) { /* scripted look, keep target */ }
      else if (this.mood === "focused") this.target = [Math.sin(t / 380) * 2.2, 0.6];
      else if (this.mood === "zen" || this.mood === "sleepy") this.target = [0, 1.4];
      else if (this.follow && Math.hypot(dx, dy) < 700 * Math.max(1, this.k / 2)) this.target = [clamp(dx / (90 * Math.max(1, this.k / 3)), -2.4, 2.4), clamp(dy / (110 * Math.max(1, this.k / 3)), -1.4, 1.6)];
      else if (t > this.sacAt) { this.target = [rand(-1.8, 1.8), rand(-0.8, 0.9)]; this.sacAt = t + rand(1200, 3600); }
      this.look[0] += (this.target[0] - this.look[0]) * 0.18;
      this.look[1] += (this.target[1] - this.look[1]) * 0.18;
      this.render(t);
      return true;
    }
    render(t) {
      const happy = this.mood === "happy" || (this.happy && (this.mood === "idle" || this.mood === "curious"));
      this.eyes.forEach((r) => r.setAttribute("visibility", happy ? "hidden" : "visible"));
      this.arcs.forEach((a) => a.setAttribute("visibility", happy ? "visible" : "hidden"));
      let [w, h, d] = SHAPES[this.mood] || SHAPES.idle;
      if (this.mood === "alert") { const g = 1 + 0.1 * (Math.sin(t / 240) + 1); w *= g; h *= g; }
      let b = 0;
      if (this.blinkT >= 0) {
        const e = t - this.blinkT;
        b = e < 150 ? e / 150 : e < 240 ? 1 : 1 - (e - 240) / 140;
        b = b * b * (3 - 2 * b);
      }
      const hh = Math.max(2, h - (h - 2) * b);
      this.eyes.forEach((r, i) => {
        const side = i ? 1 : -1;
        r.setAttribute("x", side * d - w / 2); r.setAttribute("y", -hh / 2);
        r.setAttribute("width", w); r.setAttribute("height", hh);
        r.setAttribute("rx", Math.min(w, hh) / 2);
      });
      let hop = 0;
      if (this.hopT >= 0) { const p = (t - this.hopT) / 460; if (p > 1) this.hopT = -1; else hop = Math.sin(p * Math.PI) * (4 - 1.5 * p); }
      const bob = reduce ? 0 : this.nod ? Math.sin((t / 430) * Math.PI * 2) * 0.9 : Math.sin((t / 1700) * Math.PI * 2) * 0.45;
      this.g.setAttribute("transform", `translate(${this.look[0].toFixed(2)} ${(this.look[1] + bob - hop).toFixed(2)})`);
    }
  }
  const makeNochis = (root, opts) => $$(".nochi", root).map((n) => new Nochi(n, opts));

  // ------------------------------------------------------------------------- the notch island
  // Shapes per state from geometry.rs (logical px); each host scales them.
  const STATES = {
    peek: { w: 60, h: 20, r: 10 }, pill: { w: 120, h: 28, r: 14 }, full: { w: 300, h: 52, r: 22 },
    live: { w: 210, h: 28, r: 14 }, card: { w: 380, h: 104, r: 26 }, glance: { w: 280, h: 32, r: 16 },
    focus: { w: 300, h: 52, r: 22 }, music: { w: 300, h: 52, r: 22 },
    ask: { w: 300, h: 52, r: 22 }, countdown: { w: 340, h: 56, r: 24 },
  };
  const ICON = {
    x: '<svg viewBox="0 0 24 24"><path d="M6 6l12 12M18 6L6 18"/></svg>',
    check: '<svg viewBox="0 0 24 24"><path d="M5 12.5l4.5 4.5L19 7.5"/></svg>',
    prev: '<svg viewBox="0 0 24 24"><path d="M5 5h3v14H5zM20 5v14L9 12z"/></svg>',
    pause: '<svg viewBox="0 0 24 24"><path d="M6 5h4v14H6zM14 5h4v14h-4z"/></svg>',
    next: '<svg viewBox="0 0 24 24"><path d="M16 5h3v14h-3zM4 5v14l11-7z"/></svg>',
  };
  // The live line follows the terminal in the Agent chapter (index = terminal step).
  const ACTIVITY = ["Thinking…", "Reading settings.rs", "Editing theme.rs", "Running cargo test", "Waiting for you", "Pushing to origin", "Back to Claude's dialog"];
  const LAYERS = {
    peek: `<span class="nochi" data-k=".78" style="left:30px;top:10px"></span>`,
    pill: `<span class="nochi" style="left:27px;top:14px"></span><span class="ring-sm" style="left:84px;top:8px"></span><span class="t-strong" style="right:14px;top:5px;color:rgba(245,245,247,.75)" data-left-sm>2</span>`,
    full: `<span class="nochi" data-k="1.2" style="left:30px;top:27px"></span><span class="t-kicker" style="left:56px;top:9px">Next · 9:00 AM</span><span class="t-title" style="left:56px;top:25px">Weekly report</span><span class="t-kicker" style="right:18px;top:18px" data-left>2 left</span>`,
    live: `<span class="orb" style="left:11px;top:6px;width:16px;height:16px"></span><span class="chrome" style="left:8px;top:3px"></span><span class="nochi" data-k=".62" data-mood="focused" data-follow="off" style="left:19px;top:14px"></span><span class="ticker" style="left:38px;top:5px;width:128px">${ACTIVITY.map((a) => `<span>${a}</span>`).join("")}</span><span class="t-dim" style="right:14px;top:8px">tini</span>`,
    card: `<span class="tile" style="left:14px;top:10px;width:30px;height:30px"></span><span class="nochi" data-k=".85" data-mood="alert" data-follow="off" style="left:29px;top:25px"></span><span class="bang" style="left:39px;top:0">!</span><span class="t-title" style="left:54px;top:8px;font-size:13.5px">Claude wants to run</span><span class="t-kicker" style="left:54px;top:26px">tini · Bash</span><button type="button" class="cbtn deny" style="left:294px;top:9px" aria-label="Deny">${ICON.x}</button><button type="button" class="cbtn allow" style="left:334px;top:9px" aria-label="Allow">${ICON.check}</button><span class="code" style="left:14px;top:50px;width:352px;height:30px;box-sizing:border-box"><i>$</i>git push origin main</span><span class="countdown" style="left:26px;top:99px;width:328px"><b></b></span>`,
    glance: `<span class="nochi" data-k=".85" data-mood="happy" data-follow="off" style="left:26px;top:16px"></span><span style="left:44px;top:8px"><span class="t-strong g-title" style="font-size:13px">Allowed</span>&nbsp;&nbsp;<span class="t-kicker g-detail">git push</span></span>`,
    focus: `<span class="nochi" data-k="1.2" data-mood="zen" style="left:30px;top:27px"></span><span class="t-kicker" style="left:56px;top:9px">Focus 2 of 4 · Review PR</span><span class="t-title f-time" style="left:56px;top:24px;font:600 16px var(--display)">25:00</span><span class="ring-lg" style="right:18px;top:15px"></span>`,
    music: `<span class="nochi" data-k="1.2" data-nod="on" style="left:30px;top:27px"></span><span class="t-kicker" style="left:56px;top:9px">Nochi &amp; Friends</span><span class="marquee" style="left:56px;top:24px"><span>Soft Launch · lo-fi edit&nbsp;&nbsp;·&nbsp;&nbsp;Soft Launch · lo-fi edit</span></span><span class="eq" style="right:104px;top:20px"><i></i><i></i><i></i><i></i></span><span class="media" style="right:16px;top:21px">${ICON.prev}${ICON.pause}${ICON.next}</span>`,
    ask: `<span class="nochi" data-k="1.2" data-mood="curious" style="left:30px;top:27px"></span><span class="t-kicker" style="left:56px;top:9px">Questions?</span><span class="t-title" style="left:56px;top:25px">Ask Nochi ↓</span>`,
    countdown: `<span class="nochi" data-k="1.25" style="left:32px;top:29px"></span><span class="t-kicker" style="left:60px;top:10px" data-cd-kicker>Launching Oct 30 · 19:00 WIB</span><span class="t-title" style="left:60px;top:27px;font:600 17px var(--display);font-variant-numeric:tabular-nums" data-cd-short></span><span class="live-dot" style="right:22px;top:24px"></span>`,
  };

  class Island {
    constructor(host, names, start, scaleFor, { always } = {}) {
      this.host = host;
      this.scaleFor = scaleFor;
      this.root = document.createElement("div");
      this.root.className = "island";
      this.shape = document.createElement("div");
      this.shape.className = "shape";
      this.root.append(this.shape);
      this.layers = {};
      for (const n of names) {
        const l = document.createElement("div");
        l.className = "layer";
        l.style.width = STATES[n].w + "px";
        l.style.height = STATES[n].h + "px";
        l.innerHTML = LAYERS[n];
        this.root.append(l);
        this.layers[n] = l;
      }
      host.prepend(this.root);
      this.nochis = makeNochis(this.root, { hoverEl: this.root, clickEl: this.root, always });
      this.dims = ["w", "h", "r"].map(() => ({ x: 0, v: 0, t: 0 }));
      this.state = null;
      this.set(start, true);
      new ResizeObserver(() => this.layout()).observe(host);
      this.layout();
      tickers.add(this);
      if (always) this.active = true; else watch(host, this);
    }
    layout() { this.s = this.scaleFor(this.host.clientWidth); this.root.style.transform = `scale(${this.s})`; }
    set(name, instant) {
      if (name === this.state && !instant) return;
      const prev = this.state ? STATES[this.state] : null;
      this.state = name;
      for (const n in this.layers) this.layers[n].classList.toggle("on", n === name);
      const st = STATES[name];
      const grow = !prev || st.w * st.h >= prev.w * prev.h;
      this.cfg = grow ? { resp: 0.42, zeta: 0.74 } : { resp: 0.32, zeta: 1 };
      [st.w, st.h, st.r].forEach((v, i) => { this.dims[i].t = v; if (instant || reduce) { this.dims[i].x = v; this.dims[i].v = 0; } });
      this.apply();
      wake();
    }
    tick(t, dt) {
      const w = (2 * Math.PI) / this.cfg.resp, k = w * w, c = 2 * this.cfg.zeta * w;
      let moving = false;
      for (const d of this.dims) {
        const a = -k * (d.x - d.t) - c * d.v;
        d.v += a * dt; d.x += d.v * dt;
        if (Math.abs(d.x - d.t) > 0.05 || Math.abs(d.v) > 0.05) moving = true; else { d.x = d.t; d.v = 0; }
      }
      this.apply();
      return moving;
    }
    apply() {
      const [w, h, r] = this.dims.map((d) => Math.max(0, d.x));
      this.shape.style.width = w + "px";
      this.shape.style.height = h + "px";
      this.shape.style.borderRadius = `0 0 ${r}px ${r}px`;
    }
    onVisible(v) { this.host.classList.toggle("paused", !v); }
  }

  // The activity line: shows one entry, rolling up or down to it.
  function ticker(layer) {
    const spans = $$(".ticker span", layer);
    let i = 0, id = 0;
    const place = () => spans.forEach((s, k) => { s.style.transform = `translateY(${-i * 18}px)`; s.style.opacity = k === i ? 1 : 0; });
    place();
    return {
      show(n) { if (n !== i) { i = n; place(); } },
      start(list) { this.stop(); if (reduce) return; let k = 0; id = setInterval(() => { k = (k + 1) % list.length; i = list[k]; place(); }, 1300); },
      stop() { clearInterval(id); id = 0; },
    };
  }
  function countdownBar(layer, ms) {
    const b = $(".countdown b", layer);
    b.style.transition = "none";
    b.style.transform = "scaleX(1)";
    void b.offsetWidth;
    if (!reduce) { b.style.transition = `transform ${ms}ms linear`; b.style.transform = "scaleX(0)"; }
  }
  function glance(island, title, detail, mood) {
    const l = island.layers.glance;
    $(".g-title", l).textContent = title;
    $(".g-detail", l).textContent = detail;
    const n = island.nochis.find((x) => l.contains(x.host));
    n.setMood(mood);
    island.set("glance");
    if (mood === "happy") n.hop();
  }

  // ------------------------------------------------------------------------- the guide
  const guideEl = $("#guide");
  const guideScale = () => clamp(innerWidth / 1100, 0.86, 1.3);
  const guide = new Island(guideEl, Object.keys(STATES), "pill", guideScale, { always: true });
  const live = ticker(guide.layers.live);
  tickCountdown();
  cdTimer = setInterval(tickCountdown, 1000);

  // Chapter state: each section names the notch state it wants. Agent and Sizes refine it.
  let chapter = "pill", size = "full";
  let agentStep = 0, answered = null, cardTimer = 0;
  const answerLine = $("[data-answer]");
  const answerText = {
    wait: "  waiting for your OK in tini…",
    allow: "  ✓ allowed in tini",
    deny: "  ✕ declined in tini",
    none: "  no answer, back to Claude's dialog",
  };
  const setAnswerLine = (k) => { if (answerLine) answerLine.innerHTML = `<span class="${k === "allow" ? "ok" : k === "deny" ? "no" : "warn"}">${answerText[k]}</span>`; };

  function renderGuide() {
    clearTimeout(cardTimer);
    if (chapter !== "live") { live.stop(); guide.set(chapter === "sizes" ? size : chapter); return; }
    if (agentStep < 4) { guide.set("live"); live.show(agentStep); return; }
    if (answered) { guide.set("live"); live.show(answered === "allow" ? 5 : answered === "none" ? 6 : 0); return; }
    guide.set("card");
    countdownBar(guide.layers.card, 12000);
    if (!reduce) cardTimer = setTimeout(() => answer("none"), 12000);
  }
  function setChapter(c) { if (c && c !== chapter) { chapter = c; renderGuide(); } }
  function setAgentStep(n) {
    $$(".term .ln[data-step]").forEach((l) => l.classList.toggle("on", +l.dataset.step <= n));
    if (n < 4 && answered) { answered = null; setAnswerLine("wait"); }
    if (n === agentStep) return;
    agentStep = n;
    if (chapter === "live") renderGuide();
  }
  function answer(k) {
    clearTimeout(cardTimer);
    answered = k;
    setAnswerLine(k);
    if (k === "none") glance(guide, "No answer", "back to Claude's dialog", "idle");
    else glance(guide, k === "allow" ? "Allowed" : "Declined", "git push", k === "allow" ? "happy" : "idle");
    cardTimer = setTimeout(() => { if (chapter === "live") renderGuide(); }, 2200);
  }
  $(".allow", guide.layers.card).addEventListener("click", (e) => { e.stopPropagation(); answer("allow"); });
  $(".deny", guide.layers.card).addEventListener("click", (e) => { e.stopPropagation(); answer("deny"); });
  setAnswerLine("wait");

  // Sizes chapter: the switch drives the guide itself and the stage next to it.
  const sizesStage = $('[data-island="sizes"]');
  const sizesIsland = sizesStage && new Island(sizesStage, ["peek", "pill", "full"], "full", (w) => clamp((w - 40) / 320, 0.8, 1.6));
  const sizesLabel = $("[data-sizes-label]");
  const SIZE_TEXT = {
    peek: "<b>Shy</b> · 60 × 20 · just Nochi peeking",
    pill: "<b>Compact</b> · 120 × 28 · Nochi, focus ring, tasks left",
    full: "<b>Full</b> · 300 × 52 · your next task, always in view",
  };
  function setSize(s) {
    size = s;
    $$("[data-size]").forEach((x) => x.setAttribute("aria-pressed", String(x.dataset.size === s)));
    if (sizesIsland) sizesIsland.set(s);
    if (sizesLabel) sizesLabel.innerHTML = SIZE_TEXT[s];
    if (chapter === "sizes") renderGuide();
  }
  $$("[data-size]").forEach((b) => b.addEventListener("click", () => setSize(b.dataset.size)));
  setSize("full");

  // Today: the guide's task count follows the demo.
  function setTasksAdded(on) {
    $$("[data-left]", guideEl).forEach((e) => { e.textContent = on ? "4 left" : "2 left"; });
    $$("[data-left-sm]", guideEl).forEach((e) => { e.textContent = on ? "4" : "2"; });
  }

  // Focus: the guide's timer and the big ring show the same minutes.
  const fTime = $(".f-time", guide.layers.focus), bigTime = $("[data-time]"), ring = $("[data-ring]");
  function setFocus(p) {
    const secs = Math.round(25 * 60 * (1 - p * 0.72));
    const txt = `${Math.floor(secs / 60)}:${String(secs % 60).padStart(2, "0")}`;
    fTime.textContent = txt;
    if (bigTime) bigTime.textContent = txt;
    if (ring) ring.style.strokeDashoffset = (p * 0.72).toFixed(4);
    guide.layers.focus.querySelector(".ring-lg").style.setProperty("--p", `${Math.round((1 - p * 0.72) * 100)}%`);
  }

  // ------------------------------------------------------------------------- small islands
  // The monitor's desktop is drawn at 1500 x 844 design px and scaled to its screen; its notch is a
  // live island at 1:1 in that space, so Nochi blinks and follows the cursor there too.
  const desk = $("[data-desk]");
  let deskIsland = null, deskScale = 1;
  if (desk) {
    const screen = desk.parentElement;
    const fit = () => { deskScale = screen.clientWidth / 1500; desk.style.transform = `scale(${deskScale})`; };
    new ResizeObserver(fit).observe(screen);
    fit();
    deskIsland = new Island(desk, ["pill", "full"], "full", () => 1);
  }
  $$('[data-island="music"]').forEach((el) => new Island(el, ["music"], "music", (w) => clamp((w - 24) / 320, 0.7, 1.2)));

  $$("[data-deck]").forEach((deck) => {
    const toast = deck.parentElement.querySelector("[data-deck-toast]");
    let hide = 0;
    $$(".key", deck).forEach((k) => k.addEventListener("click", () => {
      k.classList.add("press");
      setTimeout(() => k.classList.remove("press"), 160);
      toast.innerHTML = k.dataset.n ? `<b>Ctrl+Alt+${k.dataset.n}</b> · ${k.dataset.label}` : `<b>${k.dataset.label}</b>`;
      toast.classList.add("on");
      clearTimeout(hide);
      hide = setTimeout(() => toast.classList.remove("on"), 1800);
      guide.nochis.forEach((n) => n.hop());
    }));
  });

  // ------------------------------------------------------------------------- loader
  const loader = $("#loader");
  let skipIntro = reduce || !G || html.classList.contains("skip-intro");
  const loaderNochiHost = $(".nochi", loader);
  const K = clamp(Math.floor(innerWidth / 64), 4, 7);
  loaderNochiHost.dataset.k = K;
  const loaderPet = skipIntro ? null : new Nochi(loaderNochiHost, { hoverEl: loader, clickEl: document.createElement("i"), always: true });

  // standalone Nochis (gallery, FAQ avatars)
  $$(".nochi").forEach((n) => {
    if (n.firstChild) return;
    const stage = n.closest(".pet-stage, .bubble");
    new Nochi(n, { hoverEl: stage || n, clickEl: stage || n });
  });

  let started = false, lenis = null;
  function startPage() {
    if (started) return;
    started = true;
    html.classList.add("ready");
    try { sessionStorage.setItem("tini-intro", "1"); } catch (e) {}
    if (lenis) lenis.start();
    if (G) heroIn();
  }

  // ------------------------------------------------------------------------- no GSAP: plain page
  if (!G) {
    const io = new IntersectionObserver((es) => { for (const e of es) if (e.isIntersecting) { setChapter(e.target.dataset.guide); if (e.target.id === "agent") setAgentStep(4); } }, { rootMargin: "-45% 0px -45% 0px" });
    $$("[data-guide]").forEach((s) => io.observe(s));
    setAgentStep(reduce ? 4 : 0);
    setTasksAdded(true);
    setFocus(0.4);
    startPage();
    return;
  }

  // ------------------------------------------------------------------------- smooth scroll
  if (!reduce && window.Lenis) {
    lenis = new Lenis({ autoRaf: false, lerp: 0.11 });
    lenis.on("scroll", ScrollTrigger.update);
    G.ticker.add((t) => lenis.raf(t * 1000));
    G.ticker.lagSmoothing(0);
    if (!skipIntro) lenis.stop();
  }

  // ------------------------------------------------------------------------- headings
  const splits = [];
  function splitHeadings() {
    if (!window.SplitText || reduce) return;
    $$(".split").forEach((el) => {
      const hero = !!el.closest(".hero");
      splits.push(SplitText.create(el, {
        type: "lines", mask: "lines", linesClass: "split-line", autoSplit: true,
        onSplit(self) {
          if (hero) { G.set(self.lines, { yPercent: started ? 0 : 110 }); el.__lines = self.lines; return; }
          return G.from(self.lines, { yPercent: 110, duration: 1.1, ease: "expo.out", stagger: 0.09, scrollTrigger: { trigger: el, start: "top 86%", once: true } });
        },
      }));
    });
  }
  function heroIn() {
    const h = $(".hero h1");
    if (h && h.__lines) G.to(h.__lines, { yPercent: 0, duration: 1.2, ease: "expo.out", stagger: 0.1 });
    G.from(".hero .kicker, .hero .lead, .hero .ctas, .hero .launch-line, .hero .watch-hint, .scroll-hint", { y: 24, autoAlpha: 0, duration: 1, ease: "expo.out", stagger: 0.07, delay: 0.25, clearProps: "transform,opacity,visibility" });
  }

  // ------------------------------------------------------------------------- chapters
  const mm = G.matchMedia();
  const DESK = "(min-width: 900px) and (prefers-reduced-motion: no-preference)";
  const MOB = "(max-width: 899px) and (prefers-reduced-motion: no-preference)";

  // Timelines shared by both layouts; desktop scrubs them while pinned, phones play them once.
  function todayTl() {
    const out = $("[data-type]"), text = "review PR, then team meeting at 2pm tomorrow";
    const o = { n: 0 };
    const tl = G.timeline({ onUpdate() { setTasksAdded(tl.progress() > 0.7); } });
    tl.to(o, { n: text.length, duration: 1.4, ease: "none", onUpdate() { out.textContent = text.slice(0, Math.round(o.n)); } })
      .from("[data-drop]", { yPercent: -60, autoAlpha: 0, scale: 0.96, duration: 0.6, ease: "back.out(1.6)", stagger: 0.18 }, "+=0.15")
      .to(o, { n: 0, duration: 0.3, ease: "none", onUpdate() { out.textContent = text.slice(0, Math.round(o.n)); } }, "<");
    return tl;
  }
  function focusTl() {
    const o = { p: 0 };
    return G.timeline()
      .to(o, { p: 1, duration: 2, ease: "none", onUpdate() { setFocus(o.p); } })
      .to("#focus .copy", { autoAlpha: 0.3, duration: 0.8 }, 0.9)
      .to(".focus-ring", { "--glow": 1, scale: 1.04, duration: 1.2 }, 0.6);
  }
  function lightTl() {
    const el = $("[data-count]"), o = { v: 0 };
    return G.timeline().to(o, { v: 15, duration: 1, ease: "power3.out", onUpdate() { el.textContent = Math.round(o.v); } });
  }

  mm.add(DESK, () => {
    // Hero: the camera dives into the desktop while the copy lifts away.
    G.timeline({ scrollTrigger: { trigger: ".hero", start: "top top", end: "+=70%", pin: true, scrub: 0.6 } })
      .to(".world", { scale: 1.5, yPercent: 6, ease: "none" }, 0)
      .to(".hero-copy", { yPercent: -18, autoAlpha: 0, ease: "power1.in" }, 0)
      .to(".scroll-hint", { autoAlpha: 0, duration: 0.2 }, 0)
      .to(".world .taskbar", { yPercent: 100, ease: "none" }, 0);
    // Product reveal: the monitor rises and settles flat, then the camera dives into its notch until
    // that notch is exactly where and as big as the guide; the desktop fades and the guide takes over.
    const mon = $(".monitor"), scr = $(".monitor .screen");
    G.set(mon, { transformOrigin: () => `50% ${scr.offsetTop}px` });
    G.timeline({ scrollTrigger: { trigger: "#device", start: "top top", end: "+=220%", pin: true, scrub: 0.6, invalidateOnRefresh: true,
      onUpdate(self) { if (deskIsland) deskIsland.set(self.progress < 0.42 ? "full" : "pill"); } } })
      .to(guideEl, { autoAlpha: 0, duration: 0.05 }, 0)
      .fromTo(mon, { yPercent: 52, rotateX: 14, scale: 0.86 }, { yPercent: 0, rotateX: 0, scale: 1, duration: 0.36, ease: "power2.out" }, 0)
      .to(".device-copy", { yPercent: -50, autoAlpha: 0, duration: 0.26, ease: "power1.in" }, 0.06)
      .to(".monitor .neck, .monitor .foot", { autoAlpha: 0, duration: 0.12 }, 0.44)
      .to(mon, { y: () => -(mon.offsetTop + scr.offsetTop), scale: () => guide.s / deskScale, duration: 0.42, ease: "power2.inOut" }, 0.44)
      .to(".desk .win, .desk .taskbar, .desk .petal", { autoAlpha: 0, duration: 0.08 }, 0.8)
      .to(mon, { autoAlpha: 0, duration: 0.08 }, 0.9)
      .to(guideEl, { autoAlpha: 1, duration: 0.04 }, 0.9);

    ScrollTrigger.create({ trigger: "#today", start: "top top", end: "+=110%", pin: true, scrub: 0.5, animation: todayTl() });
    ScrollTrigger.create({ trigger: "#focus", start: "top top", end: "+=120%", pin: true, scrub: 0.5, animation: focusTl() });
    ScrollTrigger.create({ trigger: "#agent", start: "top top", end: "+=160%", pin: true,
      onUpdate(self) { const p = self.progress; setAgentStep(p < 0.1 ? 0 : p < 0.3 ? 1 : p < 0.5 ? 2 : p < 0.68 ? 3 : 4); },
      onLeaveBack() { setAgentStep(0); } });

    const track = $(".track"), wrap = $(".hwrap");
    const dist = () => Math.max(0, track.scrollWidth - wrap.clientWidth);
    G.to(track, { x: () => -dist(), ease: "none", scrollTrigger: { trigger: "#more", start: "top top", end: () => "+=" + dist(), pin: true, scrub: 0.6, invalidateOnRefresh: true } });
    G.from(".gcard", { y: 60, autoAlpha: 0, duration: 0.9, ease: "expo.out", stagger: 0.08, scrollTrigger: { trigger: "#more", start: "top 70%", once: true } });

    ScrollTrigger.create({ trigger: "#light", start: "top top", end: "+=80%", pin: true, scrub: 0.5, animation: lightTl() });
    privacyWords({ trigger: "#privacy", start: "top top", end: "+=100%", pin: true, scrub: 0.5 });

    // Magnetic buttons.
    if (matchMedia("(pointer: fine)").matches) $$(".magnetic").forEach((b) => {
      const xTo = G.quickTo(b, "x", { duration: 0.5, ease: "power3" }), yTo = G.quickTo(b, "y", { duration: 0.5, ease: "power3" });
      const move = (e) => { const r = b.getBoundingClientRect(); xTo((e.clientX - r.left - r.width / 2) * 0.3); yTo((e.clientY - r.top - r.height / 2) * 0.4); };
      const leave = () => { xTo(0); yTo(0); };
      b.addEventListener("pointermove", move); b.addEventListener("pointerleave", leave);
      return () => { b.removeEventListener("pointermove", move); b.removeEventListener("pointerleave", leave); };
    });
  });

  mm.add(MOB, () => {
    G.to(".world", { scale: 1.3, ease: "none", scrollTrigger: { trigger: ".hero", start: "top top", end: "bottom top", scrub: true } });
    G.fromTo(".monitor", { yPercent: 16, rotateX: 14, scale: 0.92 }, { yPercent: 0, rotateX: 0, scale: 1, ease: "none", scrollTrigger: { trigger: ".monitor", start: "top bottom", end: "center 60%", scrub: true } });
    ScrollTrigger.create({ trigger: "#today .demo", start: "top 75%", animation: todayTl().duration(2.6), toggleActions: "play none none none" });
    ScrollTrigger.create({ trigger: "#focus .demo", start: "top 70%", animation: focusTl().duration(3.2), toggleActions: "play none none none" });
    ScrollTrigger.create({ trigger: "#agent .term", start: "top 70%", once: true, onEnter() { [1, 2, 3, 4].forEach((n) => setTimeout(() => setAgentStep(n), n * 700)); } });
    ScrollTrigger.create({ trigger: "#light .big", start: "top 80%", animation: lightTl().duration(1.4), toggleActions: "play none none none" });
    privacyWords({ trigger: ".statement", start: "top 80%", end: "bottom 45%", scrub: true });
  });

  mm.add("(prefers-reduced-motion: reduce)", () => {
    setAgentStep(4);
    setTasksAdded(true);
    setFocus(0.4);
    $("[data-count]").textContent = "15";
  });

  function privacyWords(st) {
    const el = $(".statement");
    if (!window.SplitText) return;
    const sp = SplitText.create(el, { type: "words", wordsClass: "w" });
    G.to(sp.words, { opacity: 1, stagger: 0.1, ease: "none", scrollTrigger: st });
    return () => sp.revert();
  }

  // Shared one-shot reveals and the FAQ chat.
  if (!reduce) {
    $$("section:not(.hero) .kicker, section:not(.hero) .lead, .panel, .term, .focus-ring, .agents, .try, .seg, .stage-sizes, .facts > div, .cd, .finale .ctas, .big-sub").forEach((el) => {
      G.from(el, { y: 28, autoAlpha: 0, duration: 1, ease: "expo.out", scrollTrigger: { trigger: el, start: "top 88%", once: true } });
    });
    G.set(".bubble", { y: 24, autoAlpha: 0 });
    ScrollTrigger.batch(".bubble", { start: "top 88%", once: true, onEnter: (els) => G.to(els, { y: 0, autoAlpha: 1, duration: 0.7, ease: "expo.out", stagger: 0.18 }) });
  }

  // Guide triggers come last so their positions include the pin spacing above them.
  $$("[data-guide]").forEach((sec) => {
    const trig = sec.parentElement.classList.contains("pin-spacer") ? sec.parentElement : sec;
    ScrollTrigger.create({ trigger: trig, start: "top 50%", end: "bottom 50%", onToggle(self) { if (self.isActive) setChapter(sec.dataset.guide); } });
  });
  ScrollTrigger.sort();

  // ------------------------------------------------------------------------- intro
  const fontsReady = document.fonts && document.fonts.ready ? document.fonts.ready : Promise.resolve();
  if (skipIntro) {
    splitHeadings();
    startPage();
    fontsReady.then(() => ScrollTrigger.refresh());
    return;
  }

  // Nochi alone on black: looks around, blinks, hops, then the black collapses into the notch.
  const pet = $(".pet", loader);
  const W = innerWidth, H = innerHeight;
  G.set(pet, { x: W / 2 - 24 * K, y: H / 2 - 14 * K, scale: 0.6, autoAlpha: 0 });
  const intro = G.timeline();
  intro.to(pet, { scale: 1, autoAlpha: 1, duration: 0.6, ease: "back.out(2)" })
    .to(".skip", { autoAlpha: 1, duration: 0.6 }, 0.8)
    .call(() => loaderPet.gaze(-2.4, 0.2, 450), null, 0.5)
    .call(() => loaderPet.gaze(2.4, -0.5, 450), null, 0.95)
    .call(() => loaderPet.blink(), null, 1.45)
    .to(pet, { scaleY: 0.82, scaleX: 1.1, duration: 0.12, ease: "power2.in" }, 1.75)
    .call(() => loaderPet.hop(), null, 1.87)
    .to(pet, { scaleY: 1, scaleX: 1, duration: 0.5, ease: "elastic.out(1.1, 0.45)" }, 1.87)
    .call(exitLoader, null, 2.6);

  let exiting = false;
  function exitLoader() {
    if (exiting) return;
    exiting = true;
    intro.kill();
    ["wheel", "touchmove", "pointerdown", "keydown"].forEach((ev) => removeEventListener(ev, skip));
    const s = guide.s, w = innerWidth, h = innerHeight;
    const side = w / 2 - 60 * s;
    G.timeline({ onComplete: startPage })
      .to(".skip", { autoAlpha: 0, duration: 0.2 }, 0)
      .fromTo(loader, { clipPath: `inset(0px 0px 0px 0px round 0px 0px 0px 0px)` },
        { clipPath: `inset(0px ${side}px ${h - 28 * s}px ${side}px round 0px 0px ${14 * s}px ${14 * s}px)`, duration: 0.95, ease: "expo.inOut" }, 0)
      .to(pet, { x: w / 2 - 33 * s - 24 * K, y: 14 * s - 14 * K, scaleX: s / K, scaleY: s / K, duration: 0.95, ease: "expo.inOut" }, 0)
      .add(() => { splitHeadings(); }, 0.2);
  }
  function skip(e) { if (e.type === "keydown" && e.key === "Tab") return; if (intro.time() > 0.3) exitLoader(); }
  ["wheel", "touchmove", "pointerdown", "keydown"].forEach((ev) => addEventListener(ev, skip, { passive: true }));
  loader.style.animation = "none";
  fontsReady.then(() => ScrollTrigger.refresh());
})();
