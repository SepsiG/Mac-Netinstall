<script lang="ts">
  // The two macOS permissions the app needs (Local Network; incoming
  // connections through the firewall): what they are for, a button that
  // makes macOS ask now (not in the middle of an install), what macOS
  // answered, and System Settings for what is not allowed.
  import { onMount } from "svelte";
  import { toFailure } from "../lib/api";
  import { useBackend } from "../lib/context";
  import { locale, t } from "../lib/i18n";
  import { describe } from "../lib/messages";
  import type { Failure, PermissionsView, SettingsPane } from "../lib/types";
  import Modal from "./Modal.svelte";

  let {
    onclose,
    onrequested,
    refused = false,
  }: {
    onclose: () => void;
    /** macOS was asked (the prompts were triggered). */
    onrequested?: () => void;
    /** Opened because sending was refused: say so and check at once. */
    refused?: boolean;
  } = $props();

  const backend = useBackend();
  let result = $state<PermissionsView | null>(null);
  let checking = $state(false);
  let error = $state<Failure | null>(null);

  async function request() {
    checking = true;
    error = null;
    try {
      result = await backend.requestPermissions();
      onrequested?.();
    } catch (e) {
      error = toFailure(e);
    } finally {
      checking = false;
    }
  }

  async function open(pane: SettingsPane) {
    try {
      await backend.openSystemSettings(pane);
    } catch (e) {
      error = toFailure(e);
    }
  }

  onMount(() => {
    if (refused) void request();
  });

  const localText = $derived(
    result === null ? $t("permissions.not_checked") : $t(`permissions.${result.local_network}`),
  );
  const firewallText = $derived(
    result === null ? $t("permissions.not_checked") : $t(`permissions.firewall_${result.firewall}`),
  );
  const localBad = $derived(result !== null && result.local_network !== "granted");
  const firewallBad = $derived(result !== null && (result.firewall === "blocked" || result.firewall === "unknown"));
</script>

<Modal title={$t("permissions.title")} onclose={onclose} id="permissions" width={520} minWidth={380} minHeight={240}>
  {#if refused}
    <p class="refused" role="alert">{$t("permissions.refused")}</p>
  {/if}
  <p class="intro">{$t("permissions.intro")}</p>
  <ul class="items" aria-live="polite">
    <li>
      <div class="name">{$t("permissions.local_network")}</div>
      <div class="help">{$t("permissions.local_network_help")}</div>
      <div class="state" class:bad={localBad} class:good={result?.local_network === "granted"}>
        {checking ? $t("permissions.checking") : localText}
      </div>
      {#if localBad}
        <button class="small" onclick={() => open("local_network")}>{$t("permissions.open_local_network")}</button>
      {/if}
    </li>
    <li>
      <div class="name">{$t("permissions.firewall")}</div>
      <div class="help">{$t("permissions.firewall_help")}</div>
      <div class="state" class:bad={firewallBad} class:good={result !== null && !firewallBad}>
        {checking ? $t("permissions.checking") : firewallText}
      </div>
      {#if firewallBad}
        <button class="small" onclick={() => open("firewall")}>{$t("permissions.open_firewall")}</button>
      {/if}
    </li>
  </ul>
  {#if localBad || firewallBad}
    <p class="help">{$t("permissions.after_change")}</p>
  {/if}
  {#if error}
    <p class="error" role="alert">{describe($locale, error)}</p>
  {/if}
  {#snippet actions()}
    {#if result === null}
      <button onclick={onclose}>{$t("permissions.later")}</button>
      <button class="primary" onclick={request} disabled={checking} data-autofocus>{$t("permissions.request")}</button>
    {:else}
      <button onclick={request} disabled={checking}>{$t("permissions.check_again")}</button>
      <button class="primary" onclick={onclose} data-autofocus>{$t("permissions.done")}</button>
    {/if}
  {/snippet}
</Modal>

<style>
  p {
    margin: 0 0 8px;
  }
  .intro,
  .help {
    color: var(--secondary);
  }
  .help {
    font-size: 12px;
  }
  .refused,
  .error {
    color: var(--danger);
    font-weight: 600;
  }
  .items {
    list-style: none;
    margin: 0 0 8px;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .items li {
    border: 1px solid var(--separator);
    border-radius: 6px;
    padding: 8px 10px;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 3px;
  }
  .name {
    font-weight: 600;
  }
  .state.good {
    color: var(--ok);
  }
  .state.bad {
    color: var(--danger);
  }
</style>
