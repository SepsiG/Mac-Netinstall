<script lang="ts">
  import { t } from "../lib/i18n";
  import { addressText, visibleInterfaces } from "../lib/interfaces";
  import type { InterfaceView } from "../lib/types";

  let {
    interfaces,
    selected = $bindable(),
    showAll = $bindable(),
    disabled = false,
    onrefresh,
  }: {
    interfaces: InterfaceView[];
    selected: string | null;
    showAll: boolean;
    disabled?: boolean;
    onrefresh: () => void;
  } = $props();

  const visible = $derived(visibleInterfaces(interfaces, showAll, selected));

  function label(i: InterfaceView): string {
    const name = i.display_name ? $t("iface.label", { name: i.name, display: i.display_name }) : i.name;
    const parts = [name, addressText(i) ?? $t("iface.no_ip")];
    if (i.kind !== "ethernet") parts.push($t(`kind.${i.kind}`));
    return parts.join(" · ");
  }
</script>

<div class="picker">
  <label for="iface">{$t("toolbar.interface")}</label>
  <select id="iface" bind:value={selected} {disabled}>
    {#if visible.length === 0}
      <option value={null}>{$t("toolbar.no_interface")}</option>
    {/if}
    {#each visible as i (i.name)}
      <option value={i.name}>{label(i)}</option>
    {/each}
  </select>
  <button class="small" onclick={onrefresh} {disabled} aria-label="↻" title="↻">↻</button>
  <label class="check">
    <input type="checkbox" bind:checked={showAll} {disabled} />
    {$t("toolbar.show_all")}
  </label>
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
  .check {
    display: flex;
    align-items: center;
    gap: 4px;
  }
</style>
