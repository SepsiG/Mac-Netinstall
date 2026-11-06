<script lang="ts">
  // The main window: toolbar (interface, profile, start/stop), status
  // line, device table and log; the editors and confirmations as sheets.
  import { onMount, untrack } from "svelte";
  import ConfirmDialog from "./components/ConfirmDialog.svelte";
  import DeviceTable from "./components/DeviceTable.svelte";
  import InterfacePicker from "./components/InterfacePicker.svelte";
  import LogPanel from "./components/LogPanel.svelte";
  import ProfileEditor from "./components/ProfileEditor.svelte";
  import SettingsDialog from "./components/SettingsDialog.svelte";
  import StatusBar from "./components/StatusBar.svelte";
  import { toFailure, type Backend, type Unlisten } from "./lib/api";
  import { setBackend } from "./lib/context";
  import { locale, pickLanguage, t } from "./lib/i18n";
  import { defaultInterface } from "./lib/interfaces";
  import { describe, hint } from "./lib/messages";
  import { EngineState } from "./lib/state.svelte";
  import type { Failure, InterfaceView, PreflightReport, Profile, Settings, StartRequest } from "./lib/types";

  let { backend }: { backend: Backend } = $props();
  setBackend(untrack(() => backend));

  const engine = new EngineState();
  let settings = $state<Settings | null>(null);
  let preferred: string[] = [];
  let interfaces = $state<InterfaceView[]>([]);
  let iface = $state<string | null>(null);
  let profiles = $state<Profile[]>([]);
  let profile = $state<string | null>(null);
  let autoRepeat = $state(false);
  /** The last full preflight (interface, ports, assets). */
  let fullReport = $state<PreflightReport | null>(null);
  /** A quick local re-check since then (interface, link, address). */
  let localReport = $state<PreflightReport | null>(null);
  /** What the status line shows: the full report with its interface part
   *  replaced by the newer local check. */
  const report = $derived(mergeReports(fullReport, localReport));
  let checking = $state(false);
  let busy = $state(false);
  let error = $state<Failure | null>(null);
  let editing = $state<{ profile: Profile | null } | null>(null);
  let settingsOpen = $state(false);
  let quitAsk = $state<number | null>(null);
  /** Devices being prepared or flashed when Stop was pressed. */
  let stopAsk = $state<string[] | null>(null);
  /** Start was pressed for the current choice (findings turn red). */
  let attempted = $state(false);
  let logOpen = $state(true);
  let ready = $state(false);
  /** While stopped the adapter list and a quick local check follow
   *  changes in macOS (a cable, the adapter's TCP/IP settings). */
  const REFRESH_MS = 5_000;

  /** Codes the local check (`check_interface`) decides. */
  const LOCAL_CODES = new Set([
    "unknown_interface",
    "interface_down",
    "link_down",
    "interface_not_ethernet",
    "unsuitable_interface",
    "no_manual_ipv4",
    "ipv4_dhcp",
    "no_ipv4",
    "server_ip_not_on_interface",
    "no_mac",
    "invalid_netmask",
    "server_ip_not_host",
    "pool_inverted",
    "pool_outside_subnet",
    "pool_includes_reserved",
    "invalid_network",
    "default_route",
  ]);

  function mergeReports(full: PreflightReport | null, local: PreflightReport | null): PreflightReport | null {
    if (!local) return full;
    if (!full) return local;
    return {
      network: local.network,
      findings: [...local.findings, ...full.findings.filter((f) => !LOCAL_CODES.has(f.code))],
    };
  }

  /** Explicit refresh (↻): the interface list and the full preflight. */
  let fullTick = $state(0);
  async function refresh() {
    await loadInterfaces();
    fullTick++;
  }

  /** The timer's quick, local re-check: no ports, no downloads. */
  async function quickCheck() {
    await loadInterfaces();
    const req = request();
    if (!req || engine.running) return;
    const mine = checkSeq;
    try {
      const r = await backend.checkInterface(req);
      if (mine === checkSeq) localReport = r;
    } catch {
      // the next full check reports it
    }
  }

  async function attempt<T>(action: () => Promise<T>): Promise<T | undefined> {
    try {
      error = null;
      return await action();
    } catch (e) {
      error = toFailure(e);
      return undefined;
    }
  }

  /** Run a command; `false` (and the error shown) if it failed. Commands
   *  without a result resolve to nothing, so the result itself cannot say. */
  async function succeeded(action: () => Promise<unknown>): Promise<boolean> {
    try {
      error = null;
      await action();
      return true;
    } catch (e) {
      error = toFailure(e);
      return false;
    }
  }

  function applyLanguage() {
    locale.set(pickLanguage(settings?.language ?? null, preferred));
  }

  async function loadInterfaces() {
    const list = await attempt(() => backend.listInterfaces());
    if (!list) return;
    interfaces = list;
    iface = defaultInterface(list, iface);
  }

  async function loadProfiles() {
    const view = await attempt(() => backend.listProfiles());
    if (!view) return;
    profiles = view.profiles;
    if (profile && !profiles.some((p) => p.name === profile)) profile = null;
  }

  async function resync() {
    const snap = await attempt(() => backend.snapshot());
    if (!snap) return;
    engine.applySnapshot(snap);
    if (snap.running) {
      profile = snap.profile;
      autoRepeat = snap.auto_repeat;
    }
  }

  onMount(() => {
    const unlisten: Unlisten[] = [];
    (async () => {
      unlisten.push(await backend.onEvent((e) => engine.apply(e)));
      unlisten.push(await backend.onLagged(() => void resync()));
      unlisten.push(await backend.onCloseRequested((n) => (quitAsk = n)));
      settings = (await attempt(() => backend.getSettings())) ?? null;
      preferred = (await attempt(() => backend.preferredLanguages())) ?? [];
      applyLanguage();
      iface = settings?.last_interface ?? null;
      profile = settings?.last_profile ?? null;
      autoRepeat = settings?.auto_repeat ?? false;
      await Promise.all([loadInterfaces(), loadProfiles(), resync()]);
      ready = true;
    })();
    const timer = setInterval(() => {
      if (ready && !engine.running) {
        void quickCheck();
      }
    }, REFRESH_MS);
    return () => {
      clearInterval(timer);
      unlisten.forEach((u) => u());
    };
  });

  function request(): StartRequest | null {
    return iface ? { interface: iface, profile, auto_repeat: autoRepeat && profile !== null } : null;
  }

  // Preflight whenever the choice changes while stopped.
  let checkSeq = 0;
  let checkedKey = "";
  $effect(() => {
    const req = request();
    void fullTick;
    if (!ready || engine.running || !req) {
      fullReport = null;
      localReport = null;
      return;
    }
    const mine = ++checkSeq;
    // A new choice starts over; a periodic re-check keeps the tone.
    const key = JSON.stringify(req);
    if (key !== checkedKey) attempted = false;
    checkedKey = key;
    checking = true;
    backend.preflight(req).then(
      (r) => {
        if (mine === checkSeq) {
          fullReport = r;
          localReport = null;
        }
      },
      (e) => {
        if (mine === checkSeq) error = toFailure(e);
      },
    ).finally(() => {
      if (mine === checkSeq) checking = false;
    });
  });


  /** `MAC · model` of each device being prepared or flashed. */
  function busyDevices(): string[] {
    return engine.rows
      .filter((d) => d.state.state === "preparing" || d.state.state === "flashing")
      .map((d) => (d.info ? `${d.mac} · ${d.info.model}` : d.mac));
  }

  async function startStop() {
    if (engine.running) {
      const affected = busyDevices();
      if (affected.length > 0) {
        stopAsk = affected; // confirmed in the dialog
        return;
      }
      await stop();
      return;
    }
    attempted = true;
    const req = request();
    if (!req) return;
    busy = true;
    // The full check again right before starting (ports, downloads).
    const mine = ++checkSeq;
    const fresh = await attempt(() => backend.preflight(req));
    if (fresh && mine === checkSeq) {
      fullReport = fresh;
      localReport = null;
    }
    if (!fresh || fresh.findings.some((f) => f.severity === "error")) {
      busy = false;
      return; // the findings now show as errors
    }
    const info = await attempt(() => backend.start(req));
    if (info && settings) engine.cooldownSecs = settings.cooldown_secs;
    busy = false;
  }

  async function stop() {
    stopAsk = null;
    busy = true;
    await attempt(() => backend.stop());
    busy = false;
  }

  /** Change the selected profile; while running the engine must accept it
   *  first, else the previous one stays selected. */
  async function chooseProfile(name: string | null): Promise<boolean> {
    const before = { profile, autoRepeat };
    profile = name;
    if (!name) autoRepeat = false;
    if (!engine.running) return true;
    if (!(await succeeded(() => backend.setActiveProfile(name)))) {
      profile = before.profile;
      autoRepeat = before.autoRepeat;
      return false;
    }
    engine.profile = name;
    if (!name) engine.autoRepeat = false;
    return true;
  }

  /** Fetch unavailable boot kernels again (same profile, same spec). */
  function retryKernels() {
    void attempt(() => backend.setActiveProfile(profile));
  }

  async function toggleAutoRepeat(on: boolean) {
    autoRepeat = on;
    if (engine.running) {
      if (!(await succeeded(() => backend.setAutoRepeat(on)))) autoRepeat = !on;
    }
  }

  async function profileClosed(result: { saved?: string; deleted?: string }) {
    const edited = editing?.profile?.name ?? null;
    const activeEdited = edited !== null && edited === profile;
    editing = null;
    await loadProfiles();
    if (result.saved) {
      // While running, saving changes the active profile only if it is the
      // one edited (maybe renamed): then it is applied again. Stopped, the
      // saved profile is selected.
      if (!engine.running || activeEdited) await chooseProfile(result.saved);
    }
    if (result.deleted && profile === result.deleted) await chooseProfile(null);
  }

  /** Open Settings on what is stored now (a start since loading may have
   *  changed the remembered choices). */
  async function openSettings() {
    const fresh = await attempt(() => backend.getSettings());
    if (fresh) settings = fresh;
    if (settings) settingsOpen = true;
  }

  function settingsClosed(saved: Settings | null) {
    settingsOpen = false;
    if (saved) {
      settings = saved;
      applyLanguage();
    }
  }

  const current = $derived(profiles.find((p) => p.name === profile) ?? null);
</script>

<main>
  <header class="toolbar">
    <div class="row">
      <InterfacePicker {interfaces} bind:selected={iface} disabled={engine.running} onrefresh={refresh} />
      <span class="spacer"></span>
      <button
        class="primary start"
        class:stop={engine.running}
        disabled={busy || (!engine.running && !iface)}
        onclick={startStop}
      >
        {engine.running ? $t("toolbar.stop") : $t("toolbar.start")}
      </button>
    </div>
    <div class="row">
      <label for="profile" class="field-label">{$t("toolbar.profile")}</label>
      <select id="profile" value={profile ?? ""} onchange={(e) => chooseProfile(e.currentTarget.value || null)}>
        <option value="">{$t("toolbar.no_profile")}</option>
        {#each profiles as p (p.name)}
          <option value={p.name}>{p.name} · {p.version}</option>
        {/each}
      </select>
      <button class="small" disabled={!current} onclick={() => (editing = { profile: current })}>{$t("toolbar.edit")}</button>
      <button class="small" onclick={() => (editing = { profile: null })}>{$t("toolbar.new")}</button>
      <label class="check" title={$t("toolbar.auto_repeat_help")}>
        <input
          type="checkbox"
          checked={autoRepeat}
          disabled={!profile}
          onchange={(e) => toggleAutoRepeat(e.currentTarget.checked)}
        />
        {$t("toolbar.auto_repeat")}
      </label>
      <span class="spacer"></span>
      <button class="small" onclick={openSettings} disabled={!settings}>{$t("toolbar.settings")}</button>
    </div>
  </header>

  <StatusBar
    running={engine.running}
    started={engine.started}
    kernels={engine.kernels}
    linkDown={engine.linkDown}
    onlyMac={engine.onlyMac}
    {report}
    {checking}
    {attempted}
    onretry={retryKernels}
  />

  {#if error}
    <div class="banner" role="alert">
      <div>
        <div>{describe($locale, error)}</div>
        {#if hint($locale, error.code)}<div class="secondary">{hint($locale, error.code)}</div>{/if}
      </div>
      <button class="small" onclick={() => (error = null)} aria-label={$t("banner.dismiss")} title={$t("banner.dismiss")}>×</button>
    </div>
  {/if}

  <DeviceTable
    rows={engine.rows}
    running={engine.running}
    {profile}
    onlyMac={engine.onlyMac}
    cooldownSecs={engine.cooldownSecs}
    stale={engine.stale}
    onerror={(f) => (error = f)}
  />

  <LogPanel entries={engine.log} bind:open={logOpen} onclear={() => engine.clearLog()} onerror={(f) => (error = f)} />
</main>

{#if editing}
  <ProfileEditor profile={editing.profile} onclose={profileClosed} />
{/if}

{#if settingsOpen && settings}
  <SettingsDialog {settings} onclose={settingsClosed} />
{/if}

{#if stopAsk !== null}
  <ConfirmDialog
    title={$t("confirm.stop_title")}
    body={$t("confirm.stop_body", { count: stopAsk.length })}
    items={stopAsk}
    confirmLabel={$t("confirm.stop_ok")}
    cancelLabel={$t("confirm.keep")}
    destructive
    onconfirm={stop}
    oncancel={() => (stopAsk = null)}
  />
{/if}

{#if quitAsk !== null}
  <ConfirmDialog
    title={$t("confirm.quit_title")}
    body={$t("confirm.quit_body", { count: quitAsk })}
    confirmLabel={$t("confirm.quit_ok")}
    cancelLabel={$t("confirm.cancel")}
    destructive
    onconfirm={() => backend.quit()}
    oncancel={() => (quitAsk = null)}
  />
{/if}

<style>
  main {
    display: flex;
    flex-direction: column;
    height: 100%;
  }
  .toolbar {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 8px 12px;
    border-bottom: 1px solid var(--separator);
    background: var(--bg);
  }
  .row {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .spacer {
    flex: 1;
  }
  .field-label {
    width: 110px;
    white-space: nowrap;
    text-align: right;
    color: var(--secondary);
  }
  #profile {
    min-width: 260px;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 4px;
    margin-left: 8px;
  }
  .start {
    min-width: 84px;
  }
  .start.stop {
    background: var(--danger);
    border-color: var(--danger);
  }
  .banner {
    display: flex;
    justify-content: space-between;
    gap: 10px;
    padding: 6px 12px;
    color: var(--danger);
    background: color-mix(in srgb, var(--danger) 10%, var(--panel));
    border-bottom: 1px solid var(--separator);
  }
</style>
