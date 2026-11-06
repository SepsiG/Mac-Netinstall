//! `flash`: preflight, start the engine, print events, stop cleanly.
//!
//! # Ctrl-C
//!
//! With no device being prepared or flashed, Ctrl-C stops at once. While
//! one is, the first Ctrl-C only warns (on stderr; with `--json` also as
//! `{"event":"warning","code":"interrupt_pending","params":{"devices":..,
//! "confirm_secs":"5"}}` on stdout) that the device may be left unbootable;
//! a second Ctrl-C within [`CONFIRM_WINDOW`] aborts. SIGTERM always stops
//! at once.
//!
//! # `--count`
//!
//! When `--count` devices have finished, auto-repeat is turned off (no new
//! device is started; queued ones go back to waiting) and the run waits
//! for the devices still being prepared or flashed, then stops. The
//! Ctrl-C rules above apply while waiting.
//!
//! # Exit code
//!
//! - single device (no `--auto-repeat`): only the device being flashed
//!   counts: 0 done, 3 failed; others are reported but do not change it;
//! - `--auto-repeat`: 3 if any device failed (including those finished
//!   while draining after `--count`), else 0;
//! - 130 (`INTERRUPTED`) when stopped by a signal before the single device
//!   finished, or while any device was being prepared or flashed
//!   (including `cancelled` devices reported while stopping);
//! - 1 if the engine reports an error or its task is gone (`engine_gone`).
//!
//! Events missed by a slow terminal (`Lagged`) are made up for from the
//! engine's snapshot, so the target and the tallies stay right.

use crate::cli::{exit, FlashArgs, RecipeArgs};
use crate::commands::{engine_config, print_report, recipe_profile, Ctx, Outcome};
use crate::output::Printer;
use ni_engine::{preflight, DeviceState, EngineEvent, EngineHandle, Failure, Snapshot, WaitReason};
use ni_proto::Mac;
use serde_json::json;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::broadcast::error::RecvError;
use tracing::{info, warn};

/// A second Ctrl-C within this time aborts a running flash.
pub const CONFIRM_WINDOW: Duration = Duration::from_secs(5);

/// What the event loop should do next.
#[derive(Debug, PartialEq, Eq)]
enum Step {
    /// Flash this device (the single-device target was found).
    Flash(Mac),
    /// `--count` reached: turn auto-repeat off (no new devices) and wait
    /// for the devices still being prepared or flashed.
    Drain,
    /// Done: exit with this code.
    Finish(u8),
}

/// Device states and results as seen from the event stream (or a
/// snapshot after missed events).
#[derive(Debug)]
struct Tracker {
    auto_repeat: bool,
    count: Option<usize>,
    /// The device a single-device run flashes.
    target: Option<Mac>,
    /// Last known state name per device.
    states: HashMap<Mac, &'static str>,
    done: usize,
    failed: usize,
    /// Result of the target device: `Some(true)` done, `Some(false)` failed.
    target_result: Option<bool>,
    /// `--count` was reached; finishing once nothing is active.
    draining: bool,
}

impl Tracker {
    fn new(auto_repeat: bool, count: Option<usize>) -> Self {
        Tracker {
            auto_repeat,
            count,
            target: None,
            states: HashMap::new(),
            done: 0,
            failed: 0,
            target_result: None,
            draining: false,
        }
    }

    /// While draining: finish once no device is prepared or flashed.
    fn finish_drain(&self, steps: &mut Vec<Step>) {
        let finishing = steps.iter().any(|s| matches!(s, Step::Finish(_)));
        if self.draining && !finishing && self.active().is_empty() {
            steps.push(Step::Finish(self.result()));
        }
    }

    /// Devices being prepared or flashed.
    fn active(&self) -> Vec<Mac> {
        let mut v: Vec<Mac> = self
            .states
            .iter()
            .filter(|(_, s)| matches!(**s, "preparing" | "flashing"))
            .map(|(m, _)| *m)
            .collect();
        v.sort();
        v
    }

    /// The exit code if the run ended normally now.
    fn result(&self) -> u8 {
        if !self.auto_repeat {
            return match self.target_result {
                Some(true) => exit::OK,
                Some(false) => exit::DEVICE_FAILED,
                None => exit::INTERRUPTED,
            };
        }
        if self.failed > 0 {
            exit::DEVICE_FAILED
        } else {
            exit::OK
        }
    }

    /// A device is in state `name` (from an event or a snapshot).
    fn device(&mut self, mac: Mac, name: &'static str, steps: &mut Vec<Step>) {
        let prev = self.states.insert(mac, name);
        let finished = matches!(name, "done" | "failed") && prev != Some(name);
        if !finished {
            return;
        }
        let ok = name == "done";
        if ok {
            self.done += 1;
        } else {
            self.failed += 1;
        }
        if !self.auto_repeat && self.target == Some(mac) {
            self.target_result = Some(ok);
            steps.push(Step::Finish(self.result()));
        } else if !self.draining
            && self
                .count
                .is_some_and(|n| self.auto_repeat && self.done + self.failed >= n)
        {
            self.draining = true;
            steps.push(Step::Drain);
        }
    }

    fn pick_target(&mut self, mac: Mac, steps: &mut Vec<Step>) {
        if !self.auto_repeat && self.target.is_none() {
            self.target = Some(mac);
            steps.push(Step::Flash(mac));
        }
    }

    fn on_event(&mut self, e: &EngineEvent) -> Vec<Step> {
        let mut steps = Vec::new();
        match e {
            EngineEvent::Discovered { mac, .. } => self.pick_target(*mac, &mut steps),
            EngineEvent::Device { mac, state } => self.device(*mac, state.name(), &mut steps),
            _ => {}
        }
        self.finish_drain(&mut steps);
        steps
    }

    /// Catch up after missed events: finished devices are counted once,
    /// a device waiting for a profile can still become the target. (A
    /// device that finished, started over and finished again entirely
    /// within the gap is counted once.)
    fn resync(&mut self, snap: &Snapshot) -> Vec<Step> {
        let mut steps = Vec::new();
        for d in &snap.devices {
            self.device(d.mac, d.state.name(), &mut steps);
            let waiting = d.state
                == DeviceState::Waiting {
                    reason: WaitReason::NoProfile,
                };
            if waiting && d.info.is_some() {
                self.pick_target(d.mac, &mut steps);
            }
        }
        self.finish_drain(&mut steps);
        steps
    }
}

pub async fn flash(ctx: &Ctx, args: FlashArgs) -> Outcome {
    let engine = EngineHandle::spawn(&ctx.cache_dir, ctx.http.clone())?;
    let profile = match (&args.profile, &args.version) {
        (Some(name), _) => ctx.profiles.load(name)?,
        (None, Some(version)) => {
            let recipe = RecipeArgs {
                version: version.clone(),
                packages: args.packages.clone(),
                script: args.script.clone(),
            };
            recipe_profile(engine.assets(), "command-line", &recipe).await?
        }
        (None, None) => return Err(Failure::new("no_profile")),
    };
    let mut cfg = engine_config(&args.net);
    cfg.profile = Some(profile.clone());
    cfg.auto_repeat = args.auto_repeat;
    cfg.only_mac = args.only_mac;
    cfg.max_parallel = usize::from(args.max_parallel);
    cfg.cooldown = Duration::from_secs(args.cooldown);
    cfg.session.max_retransmits = args.retransmits;

    let report = preflight::run(&cfg, Some(engine.assets()), Some(&profile)).await;
    let code = print_report(ctx, &report);
    if code != exit::OK {
        return Ok(code);
    }

    let mut printer = Printer::new(ctx.json);
    let mut events = engine.subscribe();
    let started = engine.start(cfg).await?;
    info!(?started, profile = %profile.name, "flash started");
    let mut interrupt = signal(SignalKind::interrupt()).map_err(signal_failure)?;
    let mut terminate = signal(SignalKind::terminate()).map_err(signal_failure)?;
    let mut tracker = Tracker::new(args.auto_repeat, args.count);
    let mut armed: Option<Instant> = None;
    // `Some(active)` when stopped by a signal (devices active at the time).
    let mut signalled: Option<Vec<Mac>> = None;
    let code = 'run: loop {
        let event = tokio::select! {
            _ = interrupt.recv() => {
                let active = tracker.active();
                let confirmed = armed.is_some_and(|t| t.elapsed() < CONFIRM_WINDOW);
                if active.is_empty() || confirmed {
                    signalled = Some(active);
                    break 'run tracker.result();
                }
                armed = Some(Instant::now());
                interrupt_warning(ctx, &active);
                continue;
            }
            _ = terminate.recv() => {
                signalled = Some(tracker.active());
                break 'run tracker.result();
            }
            e = events.recv() => e,
        };
        let steps = match event {
            Ok(e) => {
                if let Some(line) = printer.line(&e) {
                    println!("{line}");
                }
                if let EngineEvent::Error(f) = &e {
                    // Already printed (as an `error` event with --json).
                    tracing::error!("{f}");
                    let _ = engine.stop().await;
                    if !ctx.json {
                        eprintln!("error: {}", crate::output::failure(f));
                    }
                    return Ok(exit::ERROR);
                }
                tracker.on_event(&e)
            }
            Err(RecvError::Lagged(n)) => {
                warn!(missed = n, "event stream lagged; resyncing from a snapshot");
                if !ctx.json {
                    eprintln!("warning: {n} events skipped; resyncing");
                }
                tracker.resync(&engine.snapshot().await?)
            }
            Err(RecvError::Closed) => return Err(Failure::new("engine_gone")),
        };
        for step in steps {
            match step {
                Step::Flash(mac) => engine.flash(mac, profile.clone()).await?,
                Step::Drain => {
                    info!(active = tracker.active().len(), "count reached; draining");
                    engine.set_auto_repeat(false).await?;
                }
                Step::Finish(code) => break 'run code,
            }
        }
    };
    let _ = engine.stop().await;
    let mut cancelled = 0;
    while let Ok(e) = events.try_recv() {
        if matches!(&e, EngineEvent::Device { state: DeviceState::Failed { code, .. }, .. } if code == "cancelled")
        {
            cancelled += 1;
        }
        if let Some(line) = printer.line(&e) {
            println!("{line}");
        }
    }
    let code = match signalled {
        Some(active) if !active.is_empty() || cancelled > 0 => exit::INTERRUPTED,
        // Should not happen (nothing is active when a run finishes), but a
        // device cancelled half-way is a failure.
        None if cancelled > 0 => exit::DEVICE_FAILED,
        _ => code,
    };
    info!(
        done = tracker.done,
        failed = tracker.failed,
        cancelled,
        code,
        "flash finished"
    );
    Ok(code)
}

/// First Ctrl-C while devices are being flashed.
fn interrupt_warning(ctx: &Ctx, active: &[Mac]) {
    let macs: Vec<String> = active.iter().map(Mac::to_string).collect();
    let secs = CONFIRM_WINDOW.as_secs();
    warn!(devices = ?macs, "Ctrl-C while flashing; waiting for confirmation");
    eprintln!(
        "warning: device {} is being flashed; press Ctrl-C again within {secs} s to abort, \
         it may be left unbootable",
        macs.join(", ")
    );
    if ctx.json {
        // Shaped like an engine `warning` event.
        let v = json!({"event": "warning", "code": "interrupt_pending",
                       "params": {"devices": macs.join(","),
                                  "confirm_secs": secs.to_string()}});
        println!("{v}");
    }
}

fn signal_failure(e: std::io::Error) -> Failure {
    Failure::new("signal_failed").with("detail", e)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ni_engine::{DeviceInfoView, DeviceView, KernelState};

    const A: Mac = Mac([0x48, 0xa9, 0x8a, 0, 0, 1]);
    const B: Mac = Mac([0x48, 0xa9, 0x8a, 0, 0, 2]);

    fn ev(mac: Mac, state: DeviceState) -> EngineEvent {
        EngineEvent::Device { mac, state }
    }

    fn discovered(mac: Mac) -> EngineEvent {
        EngineEvent::Discovered {
            mac,
            info: DeviceInfoView {
                model: "m".into(),
                arch: "arm64".into(),
                license_id: "x".into(),
                min_os: "7".into(),
            },
        }
    }

    fn failed() -> DeviceState {
        DeviceState::failed(Failure::new("flash_timeout"))
    }

    fn flashing() -> DeviceState {
        DeviceState::Flashing {
            version: "7.25.1".into(),
            phase: "uploading".into(),
            sent: 1,
            total: 2,
        }
    }

    fn view(mac: Mac, state: DeviceState) -> DeviceView {
        DeviceView {
            mac,
            ip: None,
            boot_arch: None,
            info: Some(DeviceInfoView {
                model: "m".into(),
                arch: "arm64".into(),
                license_id: "x".into(),
                min_os: "7".into(),
            }),
            profile: None,
            version: None,
            attempt: 1,
            first_seen_ms: 0,
            updated_ms: 0,
            state,
        }
    }

    fn snapshot(devices: Vec<DeviceView>) -> Snapshot {
        Snapshot {
            running: true,
            started: None,
            network: None,
            kernel_version: None,
            kernels: KernelState::Idle,
            auto_repeat: false,
            profile: None,
            only_mac: None,
            max_parallel: 4,
            cooldown_secs: 300,
            link_down: None,
            devices,
        }
    }

    #[test]
    fn single_mode_exit_code_follows_the_target_only() {
        let mut t = Tracker::new(false, None);
        assert_eq!(t.on_event(&discovered(A)), [Step::Flash(A)]);
        assert_eq!(t.on_event(&discovered(B)), []);
        // Another device failing (e.g. flashed by hand) does not count.
        assert_eq!(t.on_event(&ev(B, failed())), []);
        assert_eq!(t.on_event(&ev(A, flashing())), []);
        assert_eq!(t.active(), [A]);
        assert_eq!(t.on_event(&ev(A, DeviceState::Done)), [Step::Finish(0)]);
        let mut t = Tracker::new(false, None);
        t.on_event(&discovered(A));
        assert_eq!(t.result(), exit::INTERRUPTED, "not finished yet");
        assert_eq!(t.on_event(&ev(A, failed())), [Step::Finish(3)]);
    }

    #[test]
    fn auto_repeat_counts_finished_devices() {
        let mut t = Tracker::new(true, Some(2));
        assert_eq!(t.on_event(&discovered(A)), []);
        assert_eq!(t.on_event(&ev(A, failed())), []);
        assert_eq!(t.on_event(&ev(A, failed())), [], "same state twice");
        assert_eq!(
            t.on_event(&ev(B, DeviceState::Done)),
            [Step::Drain, Step::Finish(3)],
            "nothing else active: finish at once"
        );
        let t = Tracker::new(true, None);
        assert_eq!(t.result(), exit::OK);
    }

    #[test]
    fn reaching_the_count_waits_for_devices_still_flashing() {
        let mut t = Tracker::new(true, Some(1));
        assert_eq!(t.on_event(&ev(B, flashing())), []);
        assert_eq!(t.on_event(&ev(A, DeviceState::Done)), [Step::Drain]);
        assert_eq!(t.active(), [B]);
        // More devices finishing while draining do not drain again.
        assert_eq!(t.on_event(&ev(B, flashing())), []);
        assert_eq!(t.on_event(&ev(B, failed())), [Step::Finish(3)]);
        let mut t = Tracker::new(true, Some(1));
        t.on_event(&ev(B, flashing()));
        t.on_event(&ev(A, DeviceState::Done));
        assert_eq!(t.on_event(&ev(B, DeviceState::Done)), [Step::Finish(0)]);
    }

    #[test]
    fn a_snapshot_makes_up_for_missed_events() {
        // Single mode: the Discovered event was missed; the device waits.
        let mut t = Tracker::new(false, None);
        let waiting = DeviceState::Waiting {
            reason: WaitReason::NoProfile,
        };
        assert_eq!(
            t.resync(&snapshot(vec![view(A, waiting)])),
            [Step::Flash(A)]
        );
        // Its whole flash was missed too: the snapshot says done.
        assert_eq!(
            t.resync(&snapshot(vec![view(A, DeviceState::Done)])),
            [Step::Finish(0)]
        );
        // Auto-repeat: finished devices are counted once.
        let mut t = Tracker::new(true, Some(3));
        t.on_event(&ev(A, DeviceState::Done));
        let snap = snapshot(vec![
            view(A, DeviceState::Done),
            view(B, failed()),
            view(Mac([2; 6]), flashing()),
        ]);
        assert_eq!(t.resync(&snap), []);
        assert_eq!((t.done, t.failed), (1, 1));
        assert_eq!(t.active(), [Mac([2; 6])]);
        assert_eq!(t.resync(&snap), [], "idempotent");
        assert_eq!(t.result(), exit::DEVICE_FAILED);
    }
}
