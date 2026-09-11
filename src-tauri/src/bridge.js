// Playback watcher for music.youtube.com, injected via
// WebviewWindowBuilder::initialization_script and kept as a Rust string
// (include_str! avoids double-escaping inside a raw literal).
//
// YTM DOM drift is the #1 failure mode: every selector lives in SELECTORS
// below so a page redesign is a one-place patch.
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
  var playerBar = document.querySelector(SELECTORS.playerBar);
  if (!playerBar) {
    // Page not ready yet (or YTM restructured); retry shortly.
    setTimeout(startBridge, 1000);
    return;
  }

  var last = null;
  var scheduled = false;

  function current() {
    var bar = document.querySelector(SELECTORS.playerBar);
    var titleEl = bar && bar.querySelector(SELECTORS.title);
    var bylineEl = bar && bar.querySelector(SELECTORS.byline);
    var artEl = bar && bar.querySelector(SELECTORS.image);
    var video = document.querySelector('video');

    return {
      title: titleEl ? titleEl.textContent.trim() : '',
      byline: bylineEl ? bylineEl.textContent.trim() : '',
      artwork: artEl ? artEl.currentSrc || artEl.src : '',
      playing: video ? !video.paused : false,
      position: video && isFinite(video.currentTime) ? Math.floor(video.currentTime) : 0,
      duration: video && isFinite(video.duration) ? Math.floor(video.duration) : 0
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
  // Media events do not bubble; capture also covers dynamically replaced videos.
  ['play', 'pause', 'ended', 'durationchange', 'seeked'].forEach(function (event) {
    document.addEventListener(event, function () { push(current(), true); }, true);
  });
  // Bridge starts cleanly on SPA-ish navigations.
  window.addEventListener('yt-navigate-finish', scan);
  scan();
})();
