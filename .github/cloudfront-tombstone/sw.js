/*
 * The tombstone service worker for https://rustdar.mcswain.dev/.
 *
 * Squallar now lives at https://squallar.app/. A redirecting index.html on the
 * old origin, on its own, reaches almost nobody who ever used the site:
 * `squallar-web/sw.js` answers a top-level navigation out of the cache before
 * any network traffic happens. A returning visitor is handed the frozen shell,
 * it works, it works offline, and nothing in it can ever mention that the site
 * moved.
 *
 * The registration is the one thing on that origin that can still be revoked.
 * The worker script itself is never served through the worker: the browser
 * re-fetches it out of band on navigation, compares bytes, and installs a
 * replacement. So the way to reach every one of those clients is to publish
 * different bytes at the same path -- this file, at /sw.js, where the app's
 * worker lived.
 *
 * install   skipWaiting(), so this does not sit in `waiting` behind a
 *           controller that a frozen page never releases.
 * activate  delete every cache, claim the clients, send each one to the new
 *           origin, and only then unregister.
 * fetch     redirect any navigation, covering the window between activating
 *           and a client actually leaving.
 *
 * Same mechanism as `.github/pages-tombstone/`, which retired the GitHub Pages
 * origin the same way -- see its comments for the full argument. One
 * difference: this origin is CloudFront, which COULD also send a real 301 from
 * the edge. Deliberately not blanket-enabled: a redirect on /sw.js would make
 * the out-of-band worker update fail, and the installed clients -- the whole
 * audience of this file -- would keep their frozen shell forever.
 *
 * This deploy is permanent. It only does anything for someone who comes back,
 * and there is no way to observe that the last one has.
 */

const DESTINATION = "https://squallar.app/";

self.addEventListener("install", () => {
  self.skipWaiting();
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    (async () => {
      // Every cache, not just the `squallar-` generations. The point is that
      // nothing on this origin is servable from disk any more, so a later bug
      // here cannot resurrect a shell.
      for (const name of await caches.keys()) {
        await caches.delete(name).catch(() => {});
      }

      // Order is load bearing. `Client.navigate()` rejects unless this worker
      // controls the client, so `claim()` comes first; `unregister()` takes the
      // registration out of the scope map, so it comes last, or the clients it
      // was meant to move are stranded on the frozen page.
      await self.clients.claim().catch(() => {});

      for (const client of await self.clients.matchAll({ type: "window" })) {
        // Resolves to null for a cross-origin destination -- the navigation
        // still happens. If a browser refuses it outright, the fetch handler
        // below catches the same client on its next navigation.
        client.navigate(DESTINATION).catch(() => {});
      }

      await self.registration.unregister().catch(() => {});
    })(),
  );
});

self.addEventListener("fetch", (event) => {
  // Only navigations. Subresources are left to the network, where they 404 --
  // there is nothing on this origin to fetch any more, and answering them would
  // only keep a frozen page alive longer.
  if (event.request.mode !== "navigate") return;
  event.respondWith(Response.redirect(DESTINATION, 302));
});
