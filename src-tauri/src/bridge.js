// Playback watcher for music.youtube.com, injected via
// WebviewWindowBuilder::initialization_script and kept as a Rust string
// (include_str! avoids double-escaping inside a raw literal).
//
// YTM DOM drift is the #1 failure mode: every selector lives in SELECTORS
// below so a page redesign is a one-place patch.
//
// Background playback: Chromium and Windows both deprioritise a window
// nobody is looking at. The browser flags in main.rs remove the Chromium
// half; this file removes the rest by watching the media element itself and
// resuming playback that was interrupted while the app was in the
// background. A pause the user asked for is never overridden.
(function startBridge() {
  'use strict';

  var SELECTORS = {
    playerBar: 'ytmusic-player-bar',
    title: '.title',
    byline: '.byline',
    image: '.image',
    playPause: '#play-pause-button',
    next: '.next-button',
    previous: '.previous-button'
  };

  var POSITION_TICK_MS = 1000; // 1 Hz position scrub while playing
  var KEEPALIVE_MS = 2000;     // background playback watchdog cadence
  var STALL_TICKS = 3;         // ~6 s without progress while "playing"
  var MAX_NUDGES = 3;          // give up after this many recovery attempts
  var INTENT_WINDOW_MS = 4000; // a pause this soon after a user action is theirs
  var SLEEP_GAP_MS = 15000;    // a gap this long means the machine slept
  var SLEEP_FORGET_MS = 120000; // ...and this long means do not start on our own

  var playerBar = document.querySelector(SELECTORS.playerBar);
  if (!playerBar) {
    // Page not ready yet (or YTM restructured); retry shortly.
    setTimeout(startBridge, 1000);
    return;
  }

  var last = null;
  var scheduled = false;

  // --- background keep-alive state ---
  var shouldPlay = false;   // user intent: playback should be running
  var intentAt = 0;         // last explicit user transport action
  var lastTickAt = Date.now();
  var lastTime = -1;
  var stallTicks = 0;
  var nudges = 0;

  function video() {
    return document.querySelector('video');
  }

  // Called by the injected transport helpers (media keys, mini-player,
  // seeks) so an intentional pause is never mistaken for the bug.
  function noteIntent() {
    intentAt = Date.now();
  }
  window.__ytmNoteIntent = noteIntent;

  function resume(reason) {
    var v = video();
    if (!v) return;
    stallTicks = 0;
    lastTime = -1;
    var p = v.play();
    if (p && p.catch) {
      p.catch(function (error) { console.debug('Keep-alive resume (' + reason + '):', error); });
    }
  }

  function nudge(v) {
    nudges++;
    if (nudges > MAX_NUDGES) return;
    if (nudges === 1) {
      // Cheapest recovery: ask the element to play again.
      var p = v.play();
      if (p && p.catch) p.catch(function () {});
      return;
    }
    // A hair-sized seek forces the demuxer to restart when the buffer ran dry.
    try { v.currentTime = v.currentTime + 0.01; } catch (error) { /* ignore */ }
  }

  function keepAlive() {
    var now = Date.now();
    var gap = now - lastTickAt;
    lastTickAt = now;
    if (gap > SLEEP_GAP_MS) {
      // The machine slept or the renderer was frozen: restart the counters
      // and only keep playing on our own if the gap was short.
      stallTicks = 0;
      lastTime = -1;
      nudges = 0;
      if (gap > SLEEP_FORGET_MS) shouldPlay = false;
      return;
    }
    var v = video();
    if (!v) return;
    if (v.ended) { shouldPlay = false; return; }
    if (!shouldPlay) { lastTime = -1; stallTicks = 0; return; }
    if (v.paused) {
      // Only recover a pause nobody asked for, and only while the app is in
      // the background: a focused window belongs to the user.
      if (now - intentAt < INTENT_WINDOW_MS) { shouldPlay = false; return; }
      if (document.hasFocus()) { shouldPlay = false; return; }
      resume('paused while backgrounded');
      return;
    }
    if (v.currentTime > lastTime + 0.05) {
      lastTime = v.currentTime;
      stallTicks = 0;
      nudges = 0;
      return;
    }
    stallTicks++;
    if (stallTicks >= STALL_TICKS) {
      stallTicks = 0;
      nudge(v);
    }
  }

  function current() {
    var bar = document.querySelector(SELECTORS.playerBar);
    var titleEl = bar && bar.querySelector(SELECTORS.title);
    var bylineEl = bar && bar.querySelector(SELECTORS.byline);
    var artEl = bar && bar.querySelector(SELECTORS.image);
    var v = video();

    return {
      title: titleEl ? titleEl.textContent.trim() : '',
      byline: bylineEl ? bylineEl.textContent.trim() : '',
      artwork: artEl ? artEl.currentSrc || artEl.src : '',
      playing: v ? !v.paused : false,
      position: v && isFinite(v.currentTime) ? Math.floor(v.currentTime) : 0,
      duration: v && isFinite(v.duration) ? Math.floor(v.duration) : 0
    };
  }

  // artist/album: YTM puts "artist • album" in the byline.
  function splitByline(byline) {
    var i = byline.indexOf('•');
    if (i === -1) return { artist: byline, album: '' };
    return { artist: byline.slice(0, i).trim(), album: byline.slice(i + 1).trim() };
  }

  function push(s, forcePosition) {
    var p = {
      title: s.title || null,
      artist: s.byline ? splitByline(s.byline).artist : null,
      album: s.byline ? splitByline(s.byline).album : null,
      cover_url: s.artwork || null,
      playing: s.playing,
      position: s.position,
      duration: s.duration > 0 ? s.duration : null
    };
    var changed =
      forcePosition ||
      !last ||
      last.title !== p.title || last.artist !== p.artist || last.album !== p.album ||
      last.cover_url !== p.cover_url || last.playing !== p.playing || last.duration !== p.duration;
    if (!changed) return;
    if (window.__TAURI__ && window.__TAURI__.core) {
      window.__TAURI__.core.invoke('media_update', { state: p }).then(function () { last = p; }).catch(function (error) { console.debug('Media bridge:', error); });
    }
  }

  function scan() {
    push(current(), false);
  }

  function positionTick() {
    if (last && last.playing) {
      push(current(), true);
    }
  }

  new MutationObserver(function () {
    if (scheduled) return;
    scheduled = true;
    setTimeout(function () { scheduled = false; scan(); }, 100);
  }).observe(playerBar, { subtree: true, childList: true, characterData: true });
  setInterval(positionTick, POSITION_TICK_MS);
  setInterval(keepAlive, KEEPALIVE_MS);
  // Media events do not bubble; capture also covers dynamically replaced videos.
  ['play', 'pause', 'ended', 'durationchange', 'seeked'].forEach(function (event) {
    document.addEventListener(event, function () { push(current(), true); }, true);
  });
  // Keep-alive intent tracking. Capture phase so YTM's own handlers cannot
  // swallow the signal.
  document.addEventListener('play', function () {
    shouldPlay = true;
    lastTime = -1;
    stallTicks = 0;
    nudges = 0;
  }, true);
  document.addEventListener('pause', function () {
    var v = video();
    if (v && v.ended) { shouldPlay = false; return; }
    if (!shouldPlay) return;
    if (Date.now() - intentAt < INTENT_WINDOW_MS) { shouldPlay = false; return; }
    if (document.hasFocus()) { shouldPlay = false; return; }
    resume('paused while backgrounded');
  }, true);
  document.addEventListener('ended', function () { shouldPlay = false; }, true);
  // Any pointer or key activity is the user driving the player.
  document.addEventListener('pointerdown', noteIntent, true);
  document.addEventListener('keydown', noteIntent, true);
  // Bridge starts cleanly on SPA-ish navigations.
  window.addEventListener('yt-navigate-finish', scan);
  scan();
})();
