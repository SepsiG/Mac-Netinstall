<script lang="ts">
  // The devices the engine knows, with their per-device actions. Cancel
  // always asks first (a device stopped while flashing may not boot).
  import { onMount } from "svelte";
  import { toFailure } from "../lib/api";
  import { useBackend } from "../lib/context";
  import { ago, percent } from "../lib/format";
  import { locale, t } from "../lib/i18n";
  import { stateText } from "../lib/messages";
  import type { DeviceRow } from "../lib/state.svelte";
  import type { Failure } from "../lib/types";
  import ConfirmDialog from "./ConfirmDialog.svelte";

  let {
    rows,
    running,
    profile,
    onlyMac,
    cooldownSecs = 0,
    stale = false,
    onerror,
  }: {
    rows: DeviceRow[];
    running: boolean;
    /** The selected profile, used by Flash. */
    profile: string | null;
    onlyMac: string | null;
    /** The engine's post-flash cooldown: a done device cannot be
     *  forgotten during it (the engine refuses with `device_in_cooldown`). */
    cooldownSecs?: number;
    /** The rows are the last known state of a stopped engine. */
    stale?: boolean;
    onerror: (f: Failure) => void;
  } = $props();

  const backend = useBackend();
  let confirming = $state<DeviceRow | null>(null);
  let now = $state(Date.now());

  onMount(() => {
    const timer = setInterval(() => (now = Date.now()), 15_000);
    return () => clearInterval(timer);
  });

  async function run(action: () => Promise<void>) {
    try {
      await action();
    } catch (e) {
      onerror(toFailure(e));
    }
  }

  function progress(d: DeviceRow): number | null {
    const s = d.state;
    if (s.state === "kernel_serving" || s.state === "flashing") return percent(s.sent, s.total);
    return null;
  }

  function cancellable(d: DeviceRow): boolean {
    const s = d.state.state;
    return s === "preparing" || s === "flashing" || (s === "waiting" && d.state.reason === "concurrency");
  }

  function inCooldown(d: DeviceRow): boolean {
    return d.state.state === "done" && now - d.updated_ms < cooldownSecs * 1000;
  }

  function status(d: DeviceRow): string {
    if (d.state.state === "done") {
      return $t("status.done_ago", { ago: ago(d.updated_ms, now, $locale) });
    }
    return stateText($locale, d.state);
  }

  async function confirmCancel() {
    const d = confirming;
    confirming = null;
    if (d) await run(() => backend.cancel(d.mac));
  }
</script>

<div class="table-wrap" class:stale>
  {#if stale && rows.length > 0}
    <div class="stale-note secondary">{$t("devices.stale")}</div>
  {/if}
  <table>
    <thead>
      <tr>
        <th>{$t("devices.mac")}</th>
        <th>{$t("devices.model")}</th>
        <th>{$t("devices.arch")}</th>
        <th class="status-col">{$t("devices.status")}</th>
        <th class="progress-col">{$t("devices.progress")}</th>
        <th>{$t("devices.version")}</th>
        <th class="actions-col">{$t("devices.actions")}</th>
      </tr>
    </thead>
    <tbody>
      {#each rows as d (d.mac)}
        {@const p = progress(d)}
        <tr data-mac={d.mac} class:failed={d.state.state === "failed"} class:done={d.state.state === "done"}>
          <td class="mono">{d.mac}</td>
          <td>{d.info?.model ?? ""}</td>
          <td>{d.info?.arch ?? d.boot_arch ?? ""}</td>
          <td class="status-col" title={d.attempt > 1 ? $t("devices.attempt", { attempt: d.attempt }) : undefined}>
            <span class="state state-{d.state.state}">{status(d)}</span>
          </td>
          <td class="progress-col">
            {#if p !== null}
              <progress max="100" value={p} aria-label={$t("devices.progress")}></progress>
              <span class="pct">{p}%</span>
            {:else if d.state.state === "preparing"}
              <progress aria-label={$t("devices.progress")}></progress>
            {/if}
          </td>
          <td>
            {d.version ?? ""}
            {#if d.config}<div class="config secondary">{$t(`devices.config_${d.config}`)}</div>{/if}
          </td>
          <td class="actions-col">
            {#if d.state.state === "waiting" && d.state.reason === "no_profile"}
              <button class="small" disabled={!running || !profile} onclick={() => run(() => backend.flash(d.mac, profile ?? ""))}>
                {$t("action.flash")}
              </button>
            {/if}
            {#if d.state.state === "failed"}
              <button class="small" disabled={!running} onclick={() => run(() => backend.retry(d.mac))}>
                {$t("action.retry")}
              </button>
            {/if}
            {#if cancellable(d)}
              <button class="small danger" disabled={!running} onclick={() => (confirming = d)}>
                {$t("action.cancel")}
              </button>
            {:else}
              {@const cooling = inCooldown(d)}
              <!-- A disabled button shows no tooltip: the wrapper does. -->
              <span title={cooling ? $t("devices.forget_cooldown") : undefined}>
                <button class="small" disabled={!running || cooling} onclick={() => run(() => backend.forget(d.mac))}>
                  {$t("action.forget")}
                </button>
              </span>
            {/if}
            {#if running}
              {#if onlyMac === d.mac}
                <button class="small" onclick={() => run(() => backend.setOnlyMac(null))}>{$t("action.all")}</button>
              {:else if onlyMac === null}
                <button class="small" onclick={() => run(() => backend.setOnlyMac(d.mac))}>{$t("action.only")}</button>
              {/if}
            {/if}
          </td>
        </tr>
      {:else}
        <tr class="empty">
          <td colspan="7">{$t("devices.empty")}</td>
        </tr>
      {/each}
    </tbody>
  </table>
</div>

{#if confirming}
  <ConfirmDialog
    title={$t("confirm.cancel_title", { mac: confirming.mac })}
    body={confirming.state.state === "flashing" ? $t("confirm.cancel_flashing") : $t("confirm.cancel_other")}
    confirmLabel={$t("confirm.cancel_ok")}
    cancelLabel={$t("confirm.keep")}
    destructive
    onconfirm={confirmCancel}
    oncancel={() => (confirming = null)}
  />
{/if}

<style>
  .table-wrap {
    flex: 1;
    overflow: auto;
    background: var(--panel);
  }
  table {
    width: 100%;
    border-collapse: collapse;
  }
  th {
    position: sticky;
    top: 0;
    z-index: 1;
    padding: 4px 8px;
    text-align: left;
    font-weight: 500;
    font-size: 12px;
    color: var(--secondary);
    background: var(--panel-alt);
    border-bottom: 1px solid var(--separator);
  }
  td {
    padding: 4px 8px;
    border-bottom: 1px solid color-mix(in srgb, var(--separator) 50%, transparent);
    white-space: nowrap;
  }
  tbody tr:nth-child(even):not(.empty) {
    background: color-mix(in srgb, var(--panel-alt) 60%, transparent);
  }
  tbody tr:hover:not(.empty) {
    background: var(--row-hover);
  }
  .status-col {
    white-space: normal;
    min-width: 160px;
  }
  .progress-col {
    width: 150px;
  }
  .progress-col progress {
    width: 100px;
    vertical-align: middle;
  }
  .pct {
    display: inline-block;
    width: 36px;
    text-align: right;
    font-variant-numeric: tabular-nums;
    color: var(--secondary);
  }
  .actions-col {
    text-align: right;
  }
  .actions-col > * + * {
    margin-left: 4px;
  }
  .stale table {
    opacity: 0.6;
  }
  .stale-note {
    padding: 4px 12px;
    font-size: 12px;
  }
  .config {
    font-size: 11px;
  }
  .state-failed {
    color: var(--danger);
  }
  .state-done {
    color: var(--ok);
  }
  .empty td {
    padding: 28px 16px;
    text-align: center;
    color: var(--secondary);
    white-space: normal;
  }
</style>
