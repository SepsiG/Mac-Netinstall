<script lang="ts">
  // One line of status under the toolbar: preflight findings with advice
  // while stopped; link, kernels and selection while running. Blocking
  // findings read as things to do until Start is tried (`attempted`), and
  // only then as errors.
  import { percent } from "../lib/format";
  import { locale, t } from "../lib/i18n";
  import { describe, hint } from "../lib/messages";
  import type { KernelState, PreflightReport, StartInfo } from "../lib/types";

  let {
    running,
    started,
    kernels,
    linkDown,
    onlyMac,
    report,
    checking,
    attempted = false,
    onretry,
  }: {
    running: boolean;
    started: StartInfo | null;
    kernels: KernelState;
    linkDown: string | null;
    onlyMac: string | null;
    report: PreflightReport | null;
    checking: boolean;
    /** Start was pressed (and blocked or failed) for this choice. */
    attempted?: boolean;
    /** Fetch unavailable kernels again. */
    onretry?: () => void;
  } = $props();

  function tone(severity: "error" | "warning"): string {
    if (severity === "warning") return "warning";
    return attempted ? "error" : "todo";
  }

  function kernelText(k: KernelState): string {
    switch (k.state) {
      case "idle":
        return $t("kernel.idle");
      case "downloading":
        return k.total
          ? $t("kernel.downloading_progress", { spec: k.spec, percent: percent(k.done, k.total) })
          : $t("kernel.downloading", { spec: k.spec });
      case "ready":
        return k.offline ? $t("kernel.ready_offline", { version: k.version }) : $t("kernel.ready", { version: k.version });
      case "unavailable":
        return `${$t("kernel.unavailable")}: ${describe($locale, { code: k.code, params: k.params })}`;
    }
  }

  const findings = $derived(running ? [] : (report?.findings ?? []));
  const kernelHint = $derived(kernels.state === "unavailable" ? hint($locale, "kernels_unavailable") : null);
</script>

<div class="status" role="status">
  <div class="main">
    {#if running && started}
      <span class="dot" class:down={linkDown !== null}></span>
      <span>{$t("status.running", { interface: started.interface, ip: started.server_ip })}</span>
      {#if linkDown}
        <span class="warning-text">· {$t("status.link_down", { reason: $t(`link.${linkDown}`) })}</span>
      {/if}
      {#if onlyMac}
        <span class="secondary">· {$t("status.only_mac", { mac: onlyMac })}</span>
      {/if}
    {:else if checking}
      <span class="secondary">{$t("status.checking")}</span>
    {:else if findings.length === 0 && report}
      <span class="ok-text">✓ {$t("status.ready")}</span>
    {/if}
    <span class="spacer"></span>
    <span class="kernels" class:error-text={kernels.state === "unavailable"}>
      <span class="secondary">{$t("status.kernels")}:</span>
      {kernelText(kernels)}
    </span>
    {#if running && kernels.state === "unavailable" && onretry}
      <button class="small" onclick={onretry}>{$t("kernel.retry")}</button>
    {/if}
  </div>
  {#each findings as f (f.code + JSON.stringify(f.params ?? {}))}
    {@const advice = hint($locale, f.code)}
    {@const kind = tone(f.severity)}
    <div class="finding {kind}">
      <span class="icon">{kind === "error" ? "⛔" : kind === "todo" ? "→" : "⚠️"}</span>
      <div>
        <div>
          {#if kind === "todo"}<strong class="todo-label">{$t("status.todo")}</strong>{/if}
          {describe($locale, f)}
        </div>
        {#if advice}<div class="hint">{advice}</div>{/if}
      </div>
    </div>
  {/each}
  {#if kernelHint}
    <div class="finding warning"><span class="icon">ℹ️</span><div class="hint">{kernelHint}</div></div>
  {/if}
</div>

<style>
  .status {
    padding: 5px 12px;
    border-bottom: 1px solid var(--separator);
    background: var(--panel-alt);
  }
  .main {
    display: flex;
    align-items: center;
    gap: 6px;
    min-height: 20px;
  }
  .spacer {
    flex: 1;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--ok);
  }
  .dot.down {
    background: var(--warning);
  }
  .finding {
    display: flex;
    gap: 8px;
    padding: 4px 0 2px;
  }
  .finding.error {
    color: var(--danger);
  }
  .finding.todo {
    color: var(--label);
  }
  .finding.todo .icon {
    color: var(--accent);
  }
  .todo-label {
    font-weight: 600;
    margin-right: 4px;
  }
  .hint {
    color: var(--secondary);
    font-size: 12px;
    -webkit-user-select: text;
    user-select: text;
  }
  .icon {
    width: 16px;
    text-align: center;
  }
</style>
