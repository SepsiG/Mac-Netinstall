<script lang="ts">
  // Create or edit a profile: version (channel or exact), extra packages
  // from the published list, configuration (keep, default, empty or a
  // picked script), what to do afterwards, and the advanced options of
  // the official tool (mode script, remove branding, license key).
  import { untrack } from "svelte";
  import { toFailure } from "../lib/api";
  import { useBackend } from "../lib/context";
  import { locale, t } from "../lib/i18n";
  import { describe, hint } from "../lib/messages";
  import { ARCHES, CONFIG_CHOICES, formErrors, fromForm, toForm } from "../lib/profile";
  import { AFTER_INSTALL, CHANNELS, type Failure, type Profile } from "../lib/types";
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
  let askDiscard = $state(false);
  const pristine = JSON.stringify(toForm(initial));
  const dirty = $derived(JSON.stringify($state.snapshot(form)) !== pristine);

  /** Wait this long after typing an exact version before asking for its
   *  packages (no request per keystroke). */
  const TYPING_PAUSE_MS = 300;

  const errors = $derived(formErrors(form));
  const spec = $derived(form.useChannel ? form.channel : form.exact.trim());
  // Selected packages stay listed even if this version/arch lacks them.
  const choices = $derived([...new Set([...(available ?? []), ...form.packages])].sort());

  let request = 0;
  let firstList = true;
  $effect(() => {
    const version = spec;
    const a = arch;
    const typed = !form.useChannel;
    if (typed && errors.version) return;
    const mine = ++request;
    available = null;
    listError = null;
    if (!typed || firstList) {
      firstList = false;
      list(mine, version, a);
      return;
    }
    const timer = setTimeout(() => list(mine, version, a), TYPING_PAUSE_MS);
    return () => clearTimeout(timer);
  });

  function list(mine: number, version: string, a: string) {
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
  }

  /** Escape: close at once, or ask first if there are unsaved changes. */
  function requestClose() {
    if (dirty) askDiscard = true;
    else onclose({});
  }

  function toggle(name: string, on: boolean) {
    form.packages = on ? [...form.packages, name] : form.packages.filter((p) => p !== name);
  }

  /** The configuration script picked last was empty (refused: "empty
   *  configuration" is the way to install without one). */
  let emptyScript = $state(false);

  async function pick(field: "script" | "modeScript") {
    try {
      const s = await backend.pickScript();
      if (!s) return;
      if (field === "script" && s.size === 0) {
        emptyScript = true;
        form.script = null;
        return;
      }
      if (field === "script") emptyScript = false;
      form[field] = s.name;
    } catch (e) {
      saveError = toFailure(e);
    }
  }

  async function pickKey() {
    try {
      const k = await backend.pickLicenseKey();
      if (k) form.licenseKey = k.key;
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

<Modal title={initial ? $t("profile.edit_title") : $t("profile.new_title")} onclose={requestClose} id="profile" width={620} minWidth={480} minHeight={360}>
  <form class="grid" onsubmit={(e) => { e.preventDefault(); save(); }}>
    <label for="p-name">{$t("profile.name")}</label>
    <div>
      <input
        id="p-name"
        type="text"
        bind:value={form.name}
        autocomplete="off"
        aria-invalid={tried && !!errors.name}
        data-autofocus
      />
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
        <select bind:value={arch} aria-label={$t("profile.packages_for")} aria-describedby="packages-for-hint">
          {#each ARCHES as a (a)}<option value={a}>{a}</option>{/each}
        </select>
      </label>
      <span id="packages-for-hint" class="secondary hint">{$t("profile.packages_for_hint")}</span>
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
      {#each CONFIG_CHOICES as c (c)}
        <label class="row">
          <input type="radio" name="cmode" checked={form.config === c} onchange={() => (form.config = c)} />
          {$t(`profile.config_${c}`)}
        </label>
        <div class="help indent">{$t(`profile.config_${c}_help`)}</div>
        {#if c === "script" && form.config === "script"}
          <div class="row indent">
            <button type="button" class="small" onclick={() => pick("script")}>{$t("profile.choose")}</button>
            <span class={form.script ? "mono" : "secondary"}>{form.script ?? $t("profile.no_script")}</span>
          </div>
          {#if emptyScript}
            <div class="field-error" role="alert">{$t("profile.err_script_empty")}</div>
          {:else if tried && errors.script}
            <div class="field-error" role="alert">{$t(errors.script)}</div>
          {/if}
        {/if}
      {/each}
    </div>

    <label for="p-after">{$t("profile.after_install")}</label>
    <div>
      <select id="p-after" bind:value={form.afterInstall}>
        {#each AFTER_INSTALL as a (a)}<option value={a}>{$t(`profile.after_${a}`)}</option>{/each}
      </select>
    </div>

    <span class="label">{$t("profile.advanced")}</span>
    <div class="stack">
      <label class="row">
        <input type="checkbox" bind:checked={form.useModeScript} disabled={form.config === "keep" && !form.useModeScript} />
        {$t("profile.mode_script")}
      </label>
      <div class="help indent">{$t("profile.mode_script_help")}</div>
      {#if form.useModeScript}
        <div class="row indent">
          <button type="button" class="small" onclick={() => pick("modeScript")}>{$t("profile.choose")}</button>
          <span class={form.modeScript ? "mono" : "secondary"}>{form.modeScript ?? $t("profile.no_script")}</span>
        </div>
      {/if}
      {#if errors.modeScript && (tried || errors.modeScript === "profile.err_mode_keep")}
        <div class="field-error" role="alert">{$t(errors.modeScript)}</div>
      {/if}
      <label class="row">
        <input type="checkbox" bind:checked={form.removeBranding} />
        {$t("profile.remove_branding")}
      </label>
      <div class="help indent">{$t("profile.remove_branding_help")}</div>
      <div class="row">
        {$t("profile.license_key")}
        <button type="button" class="small" onclick={pickKey}>{$t("profile.license_key_choose")}</button>
        {#if form.licenseKey}
          <span>{$t("profile.license_key_set", { count: form.licenseKey.length })}</span>
          <button type="button" class="small" onclick={() => (form.licenseKey = null)}>{$t("profile.license_key_remove")}</button>
        {:else}
          <span class="secondary">{$t("profile.license_key_none")}</span>
        {/if}
      </div>
      <div class="help">{$t("profile.license_key_help")}</div>
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

{#if askDiscard}
  <ConfirmDialog
    title={$t("profile.discard_title")}
    body={$t("profile.discard_body")}
    confirmLabel={$t("profile.discard_ok")}
    cancelLabel={$t("profile.keep_editing")}
    destructive
    onconfirm={() => onclose({})}
    oncancel={() => (askDiscard = false)}
  />
{/if}

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
  .help {
    margin-top: -3px;
    font-size: 11px;
    color: var(--secondary);
    white-space: normal;
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
