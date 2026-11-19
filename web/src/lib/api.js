// The token comes in the URL fragment (never sent to a server); keep it for reloads.
export const token = (() => {
  const found = location.hash.match(/token=([0-9a-f]+)/);
  if (found) {
    sessionStorage.setItem("token", found[1]);
    history.replaceState(null, "", location.pathname);
  }
  return sessionStorage.getItem("token") || "";
})();

const headers = { "x-token": token };

export const get = (path) => fetch("/api" + path, { headers });

export const post = (path, body) =>
  fetch("/api" + path, {
    method: "POST",
    headers: { ...headers, "content-type": "application/json" },
    body: JSON.stringify(body || {}),
  });

export const eventsUrl = () => `/api/events?token=${encodeURIComponent(token)}`;

export const put = (path, body) =>
  fetch("/api" + path, {
    method: "PUT",
    headers: { ...headers, "content-type": "application/json" },
    body: JSON.stringify(body || {}),
  });
