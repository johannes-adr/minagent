// A minimal router: the page is chosen by the URL path, which the Rust server answers with
// the same index.html for every path without a file extension.
export const nav = $state({ path: location.pathname });

addEventListener("popstate", () => (nav.path = location.pathname));

export function go(path) {
  if (path === nav.path) return;
  history.pushState(null, "", path);
  nav.path = path;
}
