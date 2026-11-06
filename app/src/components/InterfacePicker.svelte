<script lang="ts">
  // Only interfaces a RouterBOARD can be netinstalled through (real wired
  // Ethernet, decided in Rust); a clear message when there is none.
  import { t } from "../lib/i18n";
  import { addressText, capableInterfaces } from "../lib/interfaces";
  import type { InterfaceView } from "../lib/types";

  let {
    interfaces,
    selected = $bindable(),
    disabled = false,
    onrefresh,
  }: {
    interfaces: InterfaceView[];
    selected: string | null;
    disabled?: boolean;
    onrefresh: () => void;
  } = $props();

  const visible = $derived(capableInterfaces(interfaces));

  function label(i: InterfaceView): string {
    const name = i.display_name ? $t("iface.label", { name: i.name, display: i.display_name }) : i.name;
    return [name, addressText(i) ?? $t("iface.no_ip")].join(" · ");
  }
</script>

<div class="picker">
  <label for="iface">{$t("toolbar.interface")}</label>
  {#if visible.length === 0}
    <span id="iface" class="empty" role="status">{$t("toolbar.no_interface")}</span>
  {:else}
    <select id="iface" bind:value={selected} {disabled}>
      {#each visible as i (i.name)}
        <option value={i.name}>{label(i)}</option>
      {/each}
    </select>
  {/if}
  <button
    class="small"
    onclick={onrefresh}
    {disabled}
    aria-label={$t("toolbar.refresh_interfaces")}
    title={$t("toolbar.refresh_interfaces")}>↻</button
  >
</div>

<style>
  .picker {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
  }
  label[for] {
    width: 110px;
    white-space: nowrap;
    text-align: right;
    color: var(--secondary);
  }
  select {
    min-width: 260px;
    max-width: 420px;
  }
  .empty {
    color: var(--warning);
    white-space: normal;
    max-width: 520px;
  }
</style>
