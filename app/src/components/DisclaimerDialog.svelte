<script lang="ts">
  // The disclaimer, accepted once (per text version) before the first
  // start: "Accept" only with the checkbox ticked; Cancel starts nothing.
  import { useBackend } from "../lib/context";
  import { t } from "../lib/i18n";
  import Modal from "./Modal.svelte";

  let {
    onaccept,
    oncancel,
  }: {
    onaccept: () => void;
    oncancel: () => void;
  } = $props();

  const backend = useBackend();
  let understood = $state(false);
</script>

<Modal title={$t("disclaimer.title")} onclose={oncancel} id="disclaimer" width={520} minWidth={360} minHeight={260}>
  <p>{$t("disclaimer.text")}</p>
  <p class="secondary">{$t("disclaimer.trademarks")}</p>
  <p>
    <button type="button" class="link" onclick={() => void backend.openLink("disclaimer").catch(() => {})}>
      {$t("disclaimer.full_text")}
    </button>
  </p>
  <label class="row">
    <input type="checkbox" bind:checked={understood} data-autofocus />
    {$t("disclaimer.understood")}
  </label>
  {#snippet actions()}
    <button onclick={oncancel}>{$t("disclaimer.cancel")}</button>
    <button class="primary" disabled={!understood} onclick={onaccept}>{$t("disclaimer.accept")}</button>
  {/snippet}
</Modal>

<style>
  p {
    margin: 0 0 8px;
    white-space: normal;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-top: 6px;
    font-weight: 600;
  }
  .link {
    background: none;
    border: none;
    padding: 0;
    color: var(--accent, #0a64d6);
    text-decoration: underline;
    cursor: pointer;
  }
</style>
