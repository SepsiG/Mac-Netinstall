//! `flash`: preflight, start the engine, print events, stop cleanly.

use crate::cli::{exit, FlashArgs, RecipeArgs};
use crate::commands::{engine_config, print_report, recipe_profile, Ctx, Outcome};
use crate::output::Printer;
use ni_engine::{preflight, DeviceState, EngineEvent, EngineHandle, Failure};
use ni_proto::Mac;
use std::time::Duration;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::broadcast::error::RecvError;
use tracing::info;

/// How a run ended.
#[derive(Debug, Default)]
struct Tally {
    done: usize,
    failed: usize,
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
    let mut target: Option<Mac> = None;
    let mut tally = Tally::default();
    let code = loop {
        let event = tokio::select! {
            _ = interrupt.recv() => break interrupted(&args, &tally),
            _ = terminate.recv() => break interrupted(&args, &tally),
            e = events.recv() => e,
        };
        let event = match event {
            Ok(e) => e,
            Err(RecvError::Lagged(n)) => {
                eprintln!("warning: {n} events skipped");
                continue;
            }
            Err(RecvError::Closed) => return Err(Failure::new("engine_gone")),
        };
        if let Some(line) = printer.line(&event) {
            println!("{line}");
        }
        match &event {
            EngineEvent::Discovered { mac, .. } if !args.auto_repeat && target.is_none() => {
                target = Some(*mac);
                engine.flash(*mac, profile.clone()).await?;
            }
            EngineEvent::Device { mac, state } => {
                let finished = match state {
                    DeviceState::Done => {
                        tally.done += 1;
                        true
                    }
                    DeviceState::Failed { .. } => {
                        tally.failed += 1;
                        true
                    }
                    _ => false,
                };
                if !finished {
                    continue;
                }
                if !args.auto_repeat && target == Some(*mac) {
                    break result(&tally);
                }
                if args.count.is_some_and(|n| tally.done + tally.failed >= n) {
                    break result(&tally);
                }
            }
            EngineEvent::Error(f) => {
                let _ = engine.stop().await;
                return Err(f.clone());
            }
            _ => {}
        }
    };
    let _ = engine.stop().await;
    while let Ok(e) = events.try_recv() {
        if let Some(line) = printer.line(&e) {
            println!("{line}");
        }
    }
    info!(
        done = tally.done,
        failed = tally.failed,
        code,
        "flash finished"
    );
    Ok(code)
}

fn signal_failure(e: std::io::Error) -> Failure {
    Failure::new("signal_failed").with("detail", e)
}

fn result(tally: &Tally) -> u8 {
    if tally.failed > 0 {
        exit::DEVICE_FAILED
    } else {
        exit::OK
    }
}

/// Ctrl-C: the normal way to end auto-repeat; before the one device of a
/// single run is finished, an interruption.
fn interrupted(args: &FlashArgs, tally: &Tally) -> u8 {
    if args.auto_repeat {
        result(tally)
    } else {
        exit::INTERRUPTED
    }
}
