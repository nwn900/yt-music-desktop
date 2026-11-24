const { test } = require("node:test");
const assert = require("node:assert/strict");
const vm = require("node:vm");
const fs = require("node:fs");
const path = require("node:path");

const SOURCE = fs.readFileSync(
  path.join(__dirname, "../src-tauri/src/bridge.js"),
  "utf8"
);

/// Minimal DOM stand-in for the injected bridge. `now` is a fake clock so the
/// keep-alive windows (intent, stall, sleep) are deterministic.
function harness(options = {}) {
  const state = {
    ready: options.ready !== false,
    focused: options.focused === true,
    now: 1_000_000,
  };
  const listeners = new Map();
  const intervals = [];
  const timeouts = [];
  const calls = [];
  let playCalls = 0;

  const video = {
    paused: options.paused !== false,
    currentTime: 10,
    duration: 100,
    ended: false,
    play() {
      playCalls += 1;
      const wasPaused = this.paused;
      this.paused = false;
      if (wasPaused) fire("play");
      return Promise.resolve();
    },
    pause() {
      this.paused = true;
      fire("pause");
    },
  };

  const bar = {
    querySelector: (s) => ({
      textContent: s === ".title" ? "Track" : "Artist • Album",
    }),
  };

  const document = {
    hasFocus: () => state.focused,
    addEventListener: (name, fn) => {
      if (!listeners.has(name)) listeners.set(name, []);
      listeners.get(name).push(fn);
    },
    querySelector: (s) =>
      s === "ytmusic-player-bar" ? (state.ready ? bar : null) : video,
  };

  const window = {
    addEventListener() {},
    __TAURI__: { core: { invoke: async (...args) => calls.push(args) } },
  };

  function fire(name) {
    for (const fn of listeners.get(name) || []) fn({ type: name });
  }

  const sandbox = {
    document,
    window,
    console,
    MutationObserver: class {
      observe() {}
    },
    setTimeout: (fn) => {
      timeouts.push(fn);
      return timeouts.length;
    },
    setInterval: (fn, ms) => {
      intervals.push({ fn, ms });
      return intervals.length;
    },
    Date: { now: () => state.now },
  };

  vm.runInNewContext(SOURCE, sandbox);

  const keepAlive = () => {
    const timer = intervals.find((i) => i.ms === 2000);
    assert.ok(timer, "bridge must register the 2 s keep-alive interval");
    timer.fn();
  };
  const tick = (ms = 2000) => {
    state.now += ms;
    keepAlive();
  };

  return {
    state,
    listeners,
    intervals,
    timeouts,
    calls,
    video,
    fire,
    document,
    window,
    keepAlive,
    tick,
    playCalls: () => playCalls,
  };
}

test("bridge retries before DOM readiness and wraps the Rust state argument", async () => {
  const h = harness({ ready: false });
  assert.equal(h.timeouts.length, 1, "bridge must retry once the page is not ready");

  h.state.ready = true;
  h.timeouts[0]();
  h.window.addEventListener = (name, fn) => fn();
  h.timeouts[0]();
  await Promise.resolve();

  assert.equal(h.calls[0][0], "media_update");
  assert.equal(h.calls[0][1].state.title, "Track");
  assert.equal(h.calls[0][1].state.artist, "Artist");

  h.video.paused = false;
  h.fire("play");
  await Promise.resolve();
  assert.equal(h.calls.at(-1)[1].state.playing, true);

  // A focused window owns its pause; the keep-alive must not undo it.
  h.state.focused = true;
  h.video.paused = true;
  h.fire("pause");
  await Promise.resolve();
  assert.equal(h.calls.at(-1)[1].state.playing, false);
});

test("resumes a pause that happened while the app was in the background", () => {
  const h = harness({ focused: false });
  h.video.paused = false;
  h.fire("play");

  h.video.pause();

  assert.equal(h.video.paused, false, "background pause must be undone");
});

test("never overrides a pause the user asked for", () => {
  const h = harness({ focused: false });
  h.video.paused = false;
  h.fire("play");

  // Media key, mini-player button or a seek: the transport helpers mark it.
  h.window.__ytmNoteIntent();
  h.video.pause();

  assert.equal(h.video.paused, true, "an intentional pause must stick");
});

test("leaves a focused window alone", () => {
  const h = harness({ focused: true });
  h.video.paused = false;
  h.fire("play");

  h.video.pause();

  assert.equal(h.video.paused, true, "a visible window belongs to the user");
});

test("keep-alive recovers a pause that slipped past the event handler", () => {
  const h = harness({ focused: false });
  h.video.paused = false;
  h.fire("play");

  h.video.paused = true; // silent pause, no pause event
  h.tick();

  assert.equal(h.video.paused, false, "watchdog must resume the silent pause");
});

test("keep-alive does nothing once the user paused", () => {
  const h = harness({ focused: false });
  h.video.paused = false;
  h.fire("play");
  h.window.__ytmNoteIntent();
  h.video.pause();

  h.tick();
  h.tick();

  assert.equal(h.video.paused, true);
});

test("nudges a player that stops advancing while it claims to play", () => {
  const h = harness({ focused: false });
  h.video.paused = false;
  h.fire("play");
  const before = h.playCalls();

  h.tick(); // establishes the position baseline
  h.tick(); // stall 1
  h.tick(); // stall 2
  h.tick(); // stall 3 -> first nudge

  assert.ok(h.playCalls() > before, "a stalled player must be nudged");

  const position = h.video.currentTime;
  h.tick();
  h.tick();
  h.tick(); // second nudge: a hair-sized seek restarts the demuxer
  assert.notEqual(h.video.currentTime, position, "second nudge must seek");
});

test("does not start playing on its own after a long sleep", () => {
  const h = harness({ focused: false });
  h.video.paused = false;
  h.fire("play");

  h.state.now += 10 * 60 * 1000; // machine slept for ten minutes
  h.keepAlive();
  h.video.paused = true;
  h.tick();

  assert.equal(h.video.paused, true, "a long gap must clear the play intent");
});
