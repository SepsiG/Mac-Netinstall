<script lang="ts">
  // Create or edit a profile: version (channel or exact), extra packages
  // from the published list, configuration (default or a picked script).
  import { untrack } from "svelte";
  import { toFailure } from "../lib/api";
  import { useBackend } from "../lib/context";
  import { locale, t } from "../lib/i18n";
  import { describe, hint } from "../lib/messages";
  import { ARCHES, formErrors, fromForm, toForm } from "../lib/profile";
  import { CHANNELS, type Failure, type Profile } from "../lib/types";
  import ConfirmDialog from "./ConfirmDialog.svelte";
  import Modal from "./Modal.svelte";

  let {
    profile,
    onclose,
  }: {
    /** `null` for a new profile. */
    profile: Profile | null;
    /** `saved`: the stored name; `deleted`: the removed one. */
    onclose: (result: { saved?: string; deleted?: string }) => void;
  } = $props();

  const backend = useBackend();
  // The form starts from the profile given when the editor opens.
  const initial = untrack(() => $state.snapshot(profile));
  let form = $state(toForm(initial));
  let arch = $state<string>("arm64");
  let available = $state<string[] | null>(null);
  let listError = $state<Failure | null>(null);
  let saveError = $state<Failure | null>(null);
  let tried = $state(false);
  let askDelete = $state(false);

  const errors = $derived(formErrors(form));
  const spec = $derived(form.useChannel ? form.channel : form.exact.trim());
  // Selected packages stay listed even if this version/arch lacks them.
  const choices = $derived([...new Set([...(available ?? []), ...form.packages])].sort());

  let request = 0;
  $effect(() => {
    const version = spec;
    const a = arch;
    if (!form.useChannel && errors.version) return;
    const mine = ++request;
    available = null;
    listError = null;
    backend.availablePackages(version, a).then(
      (r) => {
        if (mine === request) available = r.packages;
      },
      (e) => {
        if (mine === request) {
          listError = toFailure(e);
          available = [];
        }
      },
    );
  });

  function toggle(name: string, on: boolean) {
    form.packages = on ? [...form.packages, name] : form.packages.filter((p) => p !== name);
  }

  async function pick() {
    try {
      const s = await backend.pickScript();
      if (s) form.script = s.name;
    } catch (e) {
      saveError = toFailure(e);
    }
  }

  async function save() {
    tried = true;
    if (Object.keys(errors).length > 0) return;
    const p = fromForm(form);
    try {
      await backend.saveProfile(p, initial?.name ?? null);
      onclose({ saved: p.name });
    } catch (e) {
      saveError = toFailure(e);
    }
  }

  async function remove() {
    askDelete = false;
    if (!initial) return;
    try {
      await backend.deleteProfile(initial.name);
      onclose({ deleted: initial.name });
    } catch (e) {
      saveError = toFailure(e);
    }
  }
</script>

<Modal title={initial ? $t("profile.edit_title") : $t("profile.new_title")} onclose={() => onclose({})} width={520}>
  <form class="grid" onsubmit={(e) => { e.preventDefault(); save(); }}>
    <label for="p-name">{$t("profile.name")}</label>
    <div>
      <input id="p-name" type="text" bind:value={form.name} autocomplete="off" aria-invalid={tried && !!errors.name} />
      {#if tried && errors.name}<div class="field-error" role="alert">{$t(errors.name)}</div>{/if}
    </div>

    <span class="label">{$t("profile.version")}</span>
    <div class="stack">
      <label class="row">
        <input type="radio" name="vmode" checked={form.useChannel} onchange={() => (form.useChannel = true)} />
        {$t("profile.channel")}
        <select bind:value={form.channel} disabled={!form.useChannel} aria-label={$t("profile.channel")}>
          {#each CHANNELS as c (c)}<option value={c}>{$t(`channel.${c}`)}</option>{/each}
        </select>
      </label>
      <label class="row">
        <input type="radio" name="vmode" checked={!form.useChannel} onchange={() => (form.useChannel = false)} />
        {$t("profile.exact")}
        <input
          type="text"
          bind:value={form.exact}
          disabled={form.useChannel}
          placeholder={$t("profile.exact_placeholder")}
          aria-label={$t("profile.exact")}
          size="12"
        />
      </label>
      {#if tried && errors.version}<div class="field-error" role="alert">{$t(errors.version)}</div>{/if}
    </div>

    <span class="label">{$t("profile.packages")}</span>
    <div class="stack">
      <label class="row secondary">
        {$t("profile.packages_for")}
        <select bind:value={arch} aria-label={$t("profile.packages_for")}>
          {#each ARCHES as a (a)}<option value={a}>{a}</option>{/each}
        </select>
      </label>
      <div class="packages">
        {#if available === null}
          <span class="secondary">{$t("profile.packages_loading")}</span>
        {:else if choices.length === 0}
          <span class="secondary">{$t("profile.packages_none")}</span>
        {/if}
        {#each choices as name (name)}
          <label class="row">
            <input type="checkbox" checked={form.packages.includes(name)} onchange={(e) => toggle(name, e.currentTarget.checked)} />
            {name}
          </label>
        {/each}
      </div>
      {#if listError}<div class="field-error">{describe($locale, listError)}</div>{/if}
    </div>

    <span class="label">{$t("profile.config")}</span>
    <div class="stack">
      <label class="row">
        <input type="radio" name="cmode" checked={!form.useScript} onchange={() => (form.useScript = false)} />
        {$t("profile.config_default")}
      </label>
      <label class="row">
        <input type="radio" name="cmode" checked={form.useScript} onchange={() => (form.useScript = true)} />
        {$t("profile.config_script")}
      </label>
      {#if form.useScript}
        <div class="row indent">
          <button type="button" class="small" onclick={pick}>{$t("profile.choose")}</button>
          <span class={form.script ? "mono" : "secondary"}>{form.script ?? $t("profile.no_script")}</span>
        </div>
        {#if tried && errors.script}<div class="field-error" role="alert">{$t(errors.script)}</div>{/if}
      {/if}
    </div>
  </form>
  {#if saveError}
    <div class="save-error" role="alert">
      {describe($locale, saveError)}
      {#if hint($locale, saveError.code)}<div class="secondary">{hint($locale, saveError.code)}</div>{/if}
    </div>
  {/if}
  {#snippet actions()}
    {#if initial}
      <button class="danger" onclick={() => (askDelete = true)}>{$t("profile.delete")}</button>
      <span class="spacer"></span>
    {/if}
    <button onclick={() => onclose({})}>{$t("profile.cancel")}</button>
    <button class="primary" onclick={save}>{$t("profile.save")}</button>
  {/snippet}
</Modal>

{#if askDelete && initial}
  <ConfirmDialog
    title={$t("confirm.delete_title", { name: initial.name })}
    body=""
    confirmLabel={$t("confirm.delete_ok")}
    cancelLabel={$t("confirm.cancel")}
    destructive
    onconfirm={remove}
    oncancel={() => (askDelete = false)}
  />
{/if}

<style>
  .grid {
    display: grid;
    grid-template-columns: 120px 1fr;
    gap: 10px 10px;
    align-items: start;
  }
  .grid > label,
  .label {
    text-align: right;
    color: var(--secondary);
    padding-top: 3px;
  }
  .stack {
    display: flex;
    flex-direction: column;
    gap: 5px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .indent {
    padding-left: 22px;
  }
  .packages {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 2px 12px;
    max-height: 160px;
    overflow: auto;
    padding: 6px 8px;
    border: 1px solid var(--separator);
    border-radius: 6px;
    background: var(--panel-alt);
  }
  .field-error,
  .save-error {
    color: var(--danger);
    font-size: 12px;
  }
  .save-error {
    margin-top: 10px;
  }
  input[type="text"] {
    width: 100%;
  }
  .row input[type="text"] {
    width: auto;
  }
  .spacer {
    flex: 1;
  }
</style>
