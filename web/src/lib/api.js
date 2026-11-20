import { auth, expired } from "./auth.svelte.js";

const request = async (path, method, body) => {
  const res = await fetch("/api" + path, {
    method,
    headers: {
      "x-token": auth.token,
      ...(body === undefined ? {} : { "content-type": "application/json" }),
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (res.status === 401) expired();
  return res;
};

export const get = (path) => request(path, "GET");
export const post = (path, body) => request(path, "POST", body || {});
export const put = (path, body) => request(path, "PUT", body || {});

export const eventsUrl = () => `/api/events?token=${encodeURIComponent(auth.token)}`;
