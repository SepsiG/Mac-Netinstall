<script lang="ts">
  // "Downloaded RouterOS files": what the cache holds, by version, with
  // each file's state; delete one file, a version, the damaged ones or
  // everything, always after a confirmation.
  import { onMount } from "svelte";
  import { toFailure } from "../lib/api";
  import { useBackend } from "../lib/context";
  import { bytes } from "../lib/format";
  import { locale, t } from "../lib/i18n";
  import { describe } from "../lib/messages";
  import { damaged, type CachedItem, type CacheView, type DeleteScope, type Failure } from "../lib/types";
  import ConfirmDialog from "./ConfirmDialog.svelte";

  const backend = useBackend();
  let view = $state<CacheView | null>(null);
  let error = $state<Failure | null>(null);
  let freed = $state<number | null>(null);
  let asking = $state<{ scope: DeleteScope; what: string; size: number } | null>(null);
  let working = $state(false);

  const inv = $derived(view?.inventory ?? null);
  const empty = $derived(!inv || (inv.versions.length === 0 && inv.partial.length === 0 && inv.local.length === 0));
  const allItems = $derived(inv ? [...inv.versions.flatMap((v) => v.items), ...inv.partial, ...inv.local] : []);
  const damagedItems = $derived(allItems.filter(damaged));

  onMount(() => void load());

  async function load() {
    try {
      view = await backend.cacheInventory();
    } catch (e) {
      error = toFailure(e);
    }
  }

  function label(i: CachedItem): string {
    return i.kind === "kernels" ? $t("settings.cache_kernels", { count: i.files }) : i.name;
  }

  function ask(scope: DeleteScope, what: string, size: number) {
    error = null;
    freed = null;
    asking = { scope, what, size };
  }

  async function confirmed() {
    const a = asking;
    asking = null;
    if (!a) return;
    working = true;
    try {
      freed = await backend.deleteCached(a.scope);
    } catch (e) {
      error = toFailure(e);
    }
    await load();
    working = false;
  }
</script>

{#snippet rows(items: CachedItem[])}
  {#each items as i (i.id)}
    <tr>
      <td class="name">{label(i)}</td>
      <td class="secondary">{i.arch ?? ""}</td>
      <td class="num">{bytes(i.size, $locale)}</td>
      <td>
        <span class="badge" class:badge-warning={damaged(i)} class:badge-ok={i.integrity === "verified"}>
          {$t(`integrity.${i.integrity}`)}
        </span>
      </td>
      <td class="act">
        <button
          type="button"
          class="small"
          disabled={working}
          aria-label={$t("settings.delete_item", { name: label(i) })}
          title={$t("settings.delete_item", { name: label(i) })}
          onclick={() => ask({ scope: "item", id: i.id }, label(i), i.size)}>×</button
        >
      </td>
    </tr>
  {/each}
{/snippet}

<section class="cache" aria-labelledby="cache-title">
  <h3 id="cache-title">{$t("settings.cache")}</h3>
  <p class="help">{$t("settings.cache_help")}</p>
  {#if view}
    <p class="help">{$t("settings.cache_location", { size: bytes(view.inventory.total, $locale), location: view.location })}</p>
  {/if}
  {#if inv && !empty}
    <table>
      <tbody>
        {#each inv.versions as v (v.version)}
          <tr class="group">
            <th colspan="2">{$t("settings.cache_version", { version: v.version })}</th>
            <td class="num">{bytes(v.size, $locale)}</td>
            <td></td>
            <td class="act">
              <button
                type="button"
                class="small"
                disabled={working}
                onclick={() =>
                  ask({ scope: "version", version: v.version }, $t("settings.cache_version", { version: v.version }), v.size)}
                >{$t("settings.delete_version")}</button
              >
            </td>
          </tr>
          {@render rows(v.items)}
        {/each}
        {#if inv.partial.length > 0}
          <tr class="group"><th colspan="5">{$t("settings.cache_partial")}</th></tr>
          {@render rows(inv.partial)}
        {/if}
        {#if inv.local.length > 0}
          <tr class="group"><th colspan="5">{$t("settings.cache_local")}</th></tr>
          {@render rows(inv.local)}
        {/if}
      </tbody>
    </table>
  {:else if view}
    <p class="secondary">{$t("settings.cache_empty")}</p>
  {/if}
  <div class="row">
    {#if damagedItems.length > 0}
      <button
        type="button"
        class="small"
        disabled={working}
        onclick={() =>
          ask({ scope: "damaged" }, $t("confirm.delete_cached_damaged"), damagedItems.reduce((n, i) => n + i.size, 0))}
        >{$t("settings.delete_damaged")}</button
      >
    {/if}
    <button
      type="button"
      class="small"
      disabled={working || empty || (inv?.versions.length === 0 && inv?.partial.length === 0)}
      onclick={() => ask({ scope: "all" }, $t("confirm.delete_cached_all"), inv?.total ?? 0)}
      >{$t("settings.delete_all")}</button
    >
    {#if freed !== null}<span class="ok-text">{$t("settings.cache_freed", { size: bytes(freed, $locale) })}</span>{/if}
  </div>
  {#if error}<div class="error" role="alert">{describe($locale, error)}</div>{/if}
</section>

{#if asking}
  <ConfirmDialog
    title={$t("confirm.delete_cached_title", { what: asking.what })}
    body={$t("confirm.delete_cached_body", { size: bytes(asking.size, $locale) })}
    confirmLabel={$t("confirm.delete_cached_ok")}
    cancelLabel={$t("confirm.cancel")}
    destructive
    onconfirm={confirmed}
    oncancel={() => (asking = null)}
  />
{/if}

<style>
  .cache {
    grid-column: 1 / -1;
  }
  h3 {
    margin: 8px 0 0;
    font-size: 12px;
    font-weight: 600;
    color: var(--secondary);
    border-bottom: 1px solid var(--separator);
    padding-bottom: 3px;
  }
  .help {
    margin: 4px 0;
    font-size: 11px;
    color: var(--secondary);
  }
  table {
    width: 100%;
    border-collapse: collapse;
    font-size: 12px;
    margin: 4px 0;
  }
  td,
  th {
    padding: 2px 6px;
    text-align: left;
  }
  .group th {
    font-weight: 600;
    padding-top: 6px;
  }
  .name {
    font-family: ui-monospace, monospace;
    word-break: break-all;
  }
  .num {
    text-align: right;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .act {
    text-align: right;
  }
  .badge {
    font-size: 11px;
    color: var(--secondary);
    white-space: nowrap;
  }
  .badge-ok {
    color: var(--ok);
  }
  .badge-warning {
    color: var(--warning);
    font-weight: 600;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .error {
    color: var(--danger);
    font-size: 12px;
    margin-top: 4px;
  }
</style>
