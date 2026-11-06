<script lang="ts">
  import { onMount, untrack } from "svelte";
  import { toFailure } from "../lib/api";
  import { useBackend } from "../lib/context";
  import { bytes } from "../lib/format";
  import { LANGS, locale, t } from "../lib/i18n";
  import { describe, hint } from "../lib/messages";
  import { fromSettingsForm, settingsErrors, toSettingsForm, type FieldError } from "../lib/settingsForm";
  import type { CacheInfo, Failure, Settings } from "../lib/types";
  import Modal from "./Modal.svelte";

  let {
    settings,
    onclose,
  }: {
    settings: Settings;
    /** The stored settings, or `null` if cancelled. */
    onclose: (saved: Settings | null) => void;
  } = $props();

  const backend = useBackend();
  const base = untrack(() => $state.snapshot(settings));
  let form = $state(toSettingsForm(base));
  let tried = $state(false);
  let error = $state<Failure | null>(null);
  let cache = $state<CacheInfo | null>(null);
  let cleared = $state(false);

  const errors = $derived(settingsErrors(form));
  // Open when a value in it was changed from the default.
  const advancedOpen = base.pool_start !== null || base.download_timeout_secs !== 30 * 60;

  onMount(() => {
    void loadCache();
  });

  async function loadCache() {
    try {
      cache = await backend.cacheInfo();
    } catch (e) {
      error = toFailure(e);
    }
  }

  async function clearCache() {
    error = null;
    try {
      await backend.clearCache();
      cleared = true;
      await loadCache();
    } catch (e) {
      error = toFailure(e);
    }
  }

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

<Modal title={$t("settings.title")} onclose={() => onclose(null)} width={540}>
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

    <details class="advanced" open={advancedOpen}>
      <summary>{$t("settings.advanced")}</summary>
      <div class="grid">
        <label for="s-pool">{$t("settings.pool")}</label>
        <div>
          <div class="row">
            <input id="s-pool" type="text" bind:value={form.pool_start} placeholder="192.168.88.10" aria-label={`${$t("settings.pool")} 1`} />
            –
            <input type="text" bind:value={form.pool_end} placeholder="192.168.88.254" aria-label={`${$t("settings.pool")} 2`} />
          </div>
          {@render fieldError(errors.pool_start ?? errors.pool_end)}
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

    <h3>{$t("settings.cache")}</h3>
    <span></span>
    <div>
      {#if cache}
        <div class="help">{$t("settings.cache_info", { size: bytes(cache.bytes, $locale), location: cache.location })}</div>
      {/if}
      <div class="row">
        <button type="button" class="small" onclick={clearCache}>{$t("settings.clear_cache")}</button>
        {#if cleared}<span class="ok-text">{$t("settings.cache_cleared")}</span>{/if}
      </div>
    </div>
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
  .grid > label {
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
