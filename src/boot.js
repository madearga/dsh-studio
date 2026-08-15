const status = document.getElementById("status");
const row = document.getElementById("row");

const invoke = (cmd) =>
  window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.core.invoke(cmd);

function say(text, bad = false) {
  status.textContent = text;
  status.classList.toggle("bad", bad);
  row.hidden = !bad;
}

// Ask once in case the backend was already ready before this page loaded.
invoke("backend_url").then((url) => {
  if (url) location.replace(url);
});

window.__TAURI__.event.listen("backend", (e) => {
  const ev = e.payload || {};
  switch (ev.state) {
    case "booting":
      say("starting engine…");
      break;
    case "ready":
      say("connected");
      location.replace(ev.url);
      break;
    case "crashed":
      say("engine stopped — restarting…", true);
      note(ev.note);
      break;
    case "missing":
      say(ev.note
        ? "engine not found: " + ev.note
        : "no dsh backend found (set DSH_BIN or bundle one)", true);
      break;
  }
});

document.getElementById("again").onclick = () => {
  say("starting engine…");
  invoke("restart_backend");
};
document.getElementById("bye").onclick = () => invoke("quit");

function note(text) {
  const el = document.getElementById("note");
  if (!text) { el.textContent = ""; el.hidden = true; return; }
  el.hidden = false;
  el.textContent = "… " + text.split("\n").slice(-6).join("\n");
}
