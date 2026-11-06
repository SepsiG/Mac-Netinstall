<script lang="ts">
  // Only interfaces a RouterBOARD can be netinstalled through (real wired
  // Ethernet, decided in Rust); a clear message when there is none.
  import { t } from "../lib/i18n";
  import { addressText, capableInterfaces, hasLink, manualText } from "../lib/interfaces";
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
  const current = $derived(visible.find((i) => i.name === selected) ?? null);

  /** The selected adapter's IPv4 setup in macOS and its link. */
  function ipv4State(i: InterfaceView): string {
    const manual = manualText(i);
    const setup = manual
      ? $t("iface.manual", { address: manual })
      : i.configured_ipv4?.method === "dhcp"
        ? $t("iface.dhcp")
        : $t("iface.unconfigured");
    return `${setup} · ${hasLink(i) ? $t("iface.link_up") : $t("iface.link_down")}`;
  }

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
  {#if current}
    <span class="ipv4-state" class:warn={!manualText(current)} data-testid="ipv4-state">{ipv4State(current)}</span>
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
  .ipv4-state {
    color: var(--secondary);
    font-size: 12px;
    white-space: nowrap;
  }
  .ipv4-state.warn {
    color: var(--warning);
  }
  .empty {
    color: var(--warning);
    white-space: normal;
    max-width: 520px;
  }
</style>
