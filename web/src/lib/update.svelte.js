// The running version and whether a newer release can be installed (see src/update.rs).
import { get, post } from "./api.js";

export const update = $state({
  version: "",
  latest: null,
  can: false, // a newer release exists and this process can install it
  blocker: null, // why it cannot, when a newer release exists
  working: false,
  error: "",
});

export async function loadVersion() {
  try {
    const res = await get("/version");
    if (!res.ok) return;
    const data = await res.json();
    Object.assign(update, {
      version: data.version,
      latest: data.newer ? data.latest : null,
      can: data.can_update,
      blocker: data.blocker,
    });
  } catch {}
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

// Installs the newest release. The server restarts: wait until it went away and is back,
// then load the page again (the login is gone with the old process).
export async function install() {
  update.error = "";
  update.working = true;
  const res = await post("/update");
  if (!res.ok) {
    update.error = (await res.json().catch(() => ({}))).error || `HTTP ${res.status}`;
    update.working = false;
    return;
  }
  let down = false;
  for (let i = 0; i < 90; i++) {
    await sleep(1000);
    try {
      const alive = (await fetch("/api/auth")).ok;
      if (alive && down) return location.reload();
    } catch {
      down = true;
    }
    // Still the old process after a while: the update failed, the chat shows why.
    if (!down && i >= 30) break;
  }
  update.working = false;
}
