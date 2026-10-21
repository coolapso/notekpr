// Service worker for the opt-in "Keep this browser usable offline" setting.
// It keeps a copy of the app's own files (never API responses, notes or keys)
// so the app can start when the server can't be reached. It always asks the
// network first, so a new server version is picked up at once; the copy is
// only used when that fails. Registered and removed by the app (Settings).

const CACHE = "notekpr-app";
const SHELL = [
  "/",
  "/boot.js",
  "/style.css",
  "/favicon.svg",
  "/manifest.webmanifest",
  "/pkg/notekpr_web.js",
  "/pkg/notekpr_web_bg.wasm",
];

self.addEventListener("install", (event) => {
  event.waitUntil(
    caches.open(CACHE).then((c) => c.addAll(SHELL)).then(() => self.skipWaiting()),
  );
});

self.addEventListener("activate", (event) => {
  event.waitUntil(self.clients.claim());
});

self.addEventListener("fetch", (event) => {
  const req = event.request;
  const url = new URL(req.url);
  if (req.method !== "GET" || url.origin !== self.location.origin) return;
  const navigate = req.mode === "navigate";
  // Every page of the app is the same index.html.
  const key = navigate ? "/" : url.pathname;
  if (!navigate && !SHELL.includes(key)) return;
  event.respondWith(
    fetch(req)
      .then((res) => {
        // A proxy answering for a server that is down: use the copy.
        if (res.status >= 502 && res.status <= 504) throw new Error("server unavailable");
        if (res.ok) {
          const copy = res.clone();
          event.waitUntil(caches.open(CACHE).then((c) => c.put(key, copy)));
        }
        return res;
      })
      .catch(() =>
        caches.open(CACHE).then((c) => c.match(key)).then((r) => r || Response.error()),
      ),
  );
});
