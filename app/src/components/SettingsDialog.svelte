<script lang="ts">
  import { untrack } from "svelte";
  import { toFailure } from "../lib/api";
  import { useBackend } from "../lib/context";
  import { LANGS, locale, t } from "../lib/i18n";
  import { describe, hint } from "../lib/messages";
  import { fromSettingsForm, settingsErrors, toSettingsForm, type FieldError } from "../lib/settingsForm";
  import type { Failure, InterfaceView, Settings } from "../lib/types";
  import CachePanel from "./CachePanel.svelte";
  import Modal from "./Modal.svelte";

  let {
    settings,
    iface = null,
    onclose,
    onpermissions,
  }: {
    settings: Settings;
    /** The selected adapter: its configured subnet and automatic pool. */
    iface?: InterfaceView | null;
    /** The stored settings, or `null` if cancelled. */
    onclose: (saved: Settings | null) => void;
    /** Open the macOS permissions sheet (on top of this one). */
    onpermissions?: () => void;
  } = $props();

  const backend = useBackend();
  const base = untrack(() => $state.snapshot(settings));
  let form = $state(toSettingsForm(base));
  let tried = $state(false);
  let error = $state<Failure | null>(null);

  const subnet = $derived.by(() => {
    const c = iface?.configured_ipv4;
    return c?.method === "manual" && c.address && c.prefix !== null ? { address: c.address, prefix: c.prefix } : null;
  });
  const errors = $derived(settingsErrors(form, subnet));
  // Open when a value in it was changed from the default.
  const advancedOpen = base.pool_start !== null || base.download_timeout_secs !== 30 * 60;

  async function save() {
    tried = true;
    if (Object.keys(errors).length > 0) return;
    try {
      onclose(await backend.setSettings(fromSettingsForm(form, base)));
    } catch (e) {
      error = toFailure(e);
    }
  }
</script>

{#snippet fieldError(e: FieldError | undefined)}
  {#if tried && e}<div class="field-error" role="alert">{$t(e.key, e.values)}</div>{/if}
{/snippet}

<Modal title={$t("settings.title")} onclose={() => onclose(null)} id="settings" width={600} minWidth={460} minHeight={320}>
  <form class="grid" onsubmit={(e) => { e.preventDefault(); save(); }}>
    <label for="s-lang">{$t("settings.language")}</label>
    <select id="s-lang" bind:value={form.language}>
      <option value="">{$t("settings.language_system")}</option>
      {#each LANGS as l (l)}<option value={l}>{$t(`lang.${l}`)}</option>{/each}
    </select>

    <h3>{$t("settings.flashing")}</h3>
    <label for="s-par">{$t("settings.max_parallel")}</label>
    <div>
      <input id="s-par" type="text" inputmode="numeric" size="4" bind:value={form.max_parallel} />
      {@render fieldError(errors.max_parallel)}
    </div>
    <label for="s-cool">{$t("settings.cooldown")}</label>
    <div>
      <input id="s-cool" type="text" inputmode="numeric" size="6" bind:value={form.cooldown_secs} />
      {@render fieldError(errors.cooldown_secs)}
    </div>

    {#if onpermissions}
      <span class="label-cell">{$t("settings.permissions")}</span>
      <div>
        <button type="button" class="small" onclick={onpermissions}>{$t("settings.check_permissions")}</button>
        <div class="help">{$t("settings.permissions_help")}</div>
      </div>
    {/if}

    <details class="advanced" open={advancedOpen}>
      <summary>{$t("settings.advanced")}</summary>
      <div class="grid">
        <span class="label">{$t("settings.pool")}</span>
        <div>
          {#if form.poolCustom}
            <div class="row">
              <input type="text" bind:value={form.pool_start} placeholder={iface?.auto_pool?.start ?? "192.168.88.10"} aria-label={`${$t("settings.pool")} 1`} />
              –
              <input type="text" bind:value={form.pool_end} placeholder={iface?.auto_pool?.end ?? "192.168.88.254"} aria-label={`${$t("settings.pool")} 2`} />
            </div>
            {@render fieldError(errors.pool_start ?? errors.pool_end)}
            <button type="button" class="small" onclick={() => ((form.poolCustom = false), (form.pool_start = ""), (form.pool_end = ""))}
              >{$t("settings.pool_auto_reset")}</button
            >
          {:else}
            <div>
              {iface?.auto_pool
                ? $t("settings.pool_auto", { start: iface.auto_pool.start, end: iface.auto_pool.end })
                : $t("settings.pool_auto_pending")}
            </div>
            <button type="button" class="small" onclick={() => (form.poolCustom = true)}>{$t("settings.pool_custom")}</button>
          {/if}
          <div class="help">{$t("settings.pool_help")}</div>
        </div>
        <label for="s-dl">{$t("settings.download_timeout")}</label>
        <div>
          <input id="s-dl" type="text" inputmode="numeric" size="6" bind:value={form.download_timeout_min} />
          {@render fieldError(errors.download_timeout_min)}
          <div class="help">{$t("settings.download_timeout_help")}</div>
        </div>
      </div>
    </details>

    <CachePanel />
  </form>
  {#if error}
    <div class="save-error" role="alert">
      {describe($locale, error)}
      {#if hint($locale, error.code)}<div class="secondary">{hint($locale, error.code)}</div>{/if}
    </div>
  {/if}
  {#snippet actions()}
    <button onclick={() => onclose(null)}>{$t("settings.cancel")}</button>
    <button class="primary" onclick={save}>{$t("settings.save")}</button>
  {/snippet}
</Modal>

<style>
  .grid {
    display: grid;
    grid-template-columns: 170px 1fr;
    gap: 8px 10px;
    align-items: start;
  }
  .grid > label,
  .grid > .label-cell {
    text-align: right;
    color: var(--secondary);
    padding-top: 3px;
  }
  .advanced {
    grid-column: 1 / -1;
  }
  .advanced summary {
    cursor: default;
    font-size: 12px;
    font-weight: 600;
    color: var(--secondary);
    margin: 8px 0 6px;
  }
  .advanced .label {
    text-align: right;
    color: var(--secondary);
    padding-top: 3px;
  }
  .advanced .grid {
    margin-top: 6px;
  }
  h3 {
    grid-column: 1 / -1;
    margin: 8px 0 0;
    font-size: 12px;
    font-weight: 600;
    color: var(--secondary);
    border-bottom: 1px solid var(--separator);
    padding-bottom: 3px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .help {
    margin-top: 3px;
    font-size: 11px;
    color: var(--secondary);
  }
  .field-error,
  .save-error {
    color: var(--danger);
    font-size: 12px;
  }
  .save-error {
    margin-top: 10px;
  }
  input[type="text"]:not([size]) {
    width: 150px;
  }
</style>
