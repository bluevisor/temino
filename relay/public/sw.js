// Temino Remote's service worker: only for notifications. A push says
// which machine and tab need you, nothing from the screen.
self.addEventListener("push", (event) => {
  let message = { title: "Temino", body: "An agent needs you" };
  try { message = { ...message, ...event.data.json() }; } catch {}
  event.waitUntil(self.registration.showNotification(message.title, {
    body: message.body, tag: message.tag, renotify: true, icon: "/icon-192.png", badge: "/icon-192.png",
  }));
});

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  event.waitUntil((async () => {
    const open = await self.clients.matchAll({ type: "window", includeUncontrolled: true });
    for (const client of open) if ("focus" in client) return client.focus();
    return self.clients.openWindow("/app");
  })());
});
