<script>
  import { onMount } from "svelte";
  import { chat, connect } from "./lib/chat.svelte.js";
  import { nav } from "./lib/route.svelte.js";
  import Header from "./components/Header.svelte";
  import SessionDrawer from "./components/SessionDrawer.svelte";
  import Transcript from "./components/Transcript.svelte";
  import SidePanel from "./components/SidePanel.svelte";
  import Composer from "./components/Composer.svelte";
  import SettingsPage from "./pages/SettingsPage.svelte";
  import FilesPage from "./pages/FilesPage.svelte";
  import HooksPage from "./pages/HooksPage.svelte";
  import ApprovalsPage from "./pages/ApprovalsPage.svelte";

  // The event stream stays connected while the settings page is shown.
  onMount(connect);
</script>

{#if nav.path.startsWith("/settings")}
  <SettingsPage />
{:else if nav.path.startsWith("/files")}
  <FilesPage />
{:else if nav.path.startsWith("/hooks")}
  <HooksPage />
{:else if nav.path.startsWith("/approvals")}
  <ApprovalsPage />
{:else}
  <SessionDrawer />
  <Header />
  <div class="row">
    <Transcript pane={chat.main} />
    {#if chat.subs.length && !chat.sideHidden}
      <SidePanel />
    {/if}
  </div>
  <Composer />
{/if}

<style>
  .row { flex: 1; min-height: 0; display: flex; }
  @media (max-width: 800px) { .row { flex-direction: column; } }
</style>
