// The login: one password, created on the page the first time (see src/frontend/auth.rs).
// A successful login (or setup) returns a token that goes with every request.
const KEY = "token";

function stored() {
  // A token in the URL fragment (never sent to a server) is the one printed at startup.
  const found = location.hash.match(/token=([0-9a-f]+)/);
  try {
    if (found) {
      localStorage.setItem(KEY, found[1]);
      history.replaceState(null, "", location.pathname);
    }
    return localStorage.getItem(KEY) || "";
  } catch {
    return found ? found[1] : "";
  }
}

function remember(token) {
  auth.token = token;
  try {
    token ? localStorage.setItem(KEY, token) : localStorage.removeItem(KEY);
  } catch {}
}

// phase: loading | setup (no password yet) | login | ok
export const auth = $state({ phase: "loading", token: stored() });

// Asks the server what the stored token is worth.
export async function checkAuth() {
  try {
    const res = await fetch("/api/auth", { headers: { "x-token": auth.token } });
    const { configured, authenticated } = await res.json();
    auth.phase = authenticated ? "ok" : configured ? "login" : "setup";
  } catch {
    auth.phase = "login";
  }
}

// Sets the first password (`setup`) or logs in. Throws the server's message on failure.
export async function submitPassword(password, setup) {
  const res = await fetch(setup ? "/api/auth/setup" : "/api/auth/login", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ password }),
  });
  const body = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(body.error || `HTTP ${res.status}`);
  remember(body.token);
  auth.phase = "ok";
}

export async function logout() {
  await fetch("/api/auth/logout", { method: "POST", headers: { "x-token": auth.token } }).catch(() => {});
  remember("");
  // Starts the page over, so nothing of the session stays in memory.
  location.reload();
}

// A request the server refused: the token is gone (a restart forgets them).
export function expired() {
  if (auth.phase === "ok") {
    remember("");
    checkAuth();
  }
}
