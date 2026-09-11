const { test } = require("node:test");
const assert = require("node:assert/strict");
const vm = require("node:vm");
const fs = require("node:fs");
const path = require("node:path");
test("bridge retries before DOM readiness and wraps the Rust state argument", async () => {
 const source=fs.readFileSync(path.join(__dirname,"../src-tauri/src/bridge.js"),"utf8");
 let ready=false, retry, tick, calls=[];
 const bar={querySelector: s => ({textContent:s===".title"?"Track":"Artist • Album"})};
 const events={};
 const video={paused:true,currentTime:10,duration:100};
 const document={addEventListener:(name,fn)=>{events[name]=fn},querySelector:s=>s==="ytmusic-player-bar"?(ready?bar:null):video};
 const window={addEventListener(){},__TAURI__:{core:{invoke:async (...args)=>calls.push(args)}}};
 vm.runInNewContext(source,{document,window,console,MutationObserver:class{observe(){}},
   setTimeout:fn=>{retry=fn},setInterval:fn=>{tick=fn}});
 assert.equal(typeof retry,"function");
 ready=true;retry();
 // Navigation scans must use the bridge's state envelope.
 window.addEventListener=(name,fn)=>fn();
 retry();await Promise.resolve();
 assert.equal(calls[0][0],"media_update");
 assert.equal(calls[0][1].state.title,"Track");
 assert.equal(calls[0][1].state.artist,"Artist");
 video.paused=false; events.play(); await Promise.resolve();
 assert.equal(calls.at(-1)[1].state.playing,true);
 video.paused=true; events.pause(); await Promise.resolve();
 assert.equal(calls.at(-1)[1].state.playing,false);
});
