//! The `ni-cli` binary: arguments, exit codes, and a real `flash` run
//! against a simulated RouterBOARD over loopback.

use ni_assets::testutil::{mirror, tempdir, TempDir, TestServer};
use ni_proto::Mac;
use ni_sim::lan::{tftp_get, SimLan};
use ni_sim::{DeviceSim, SimConfig};
use serde_json::Value;
use std::net::SocketAddr;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader, Lines};
use tokio::process::{ChildStdout, Command};

const WAIT: Duration = Duration::from_secs(20);
const DEVICE: Mac = Mac([0x48, 0xa9, 0x8a, 0, 0, 0x0c]);

/// Temp dirs for cache, profiles and logs: tests never touch `~/Library`.
struct Env {
    dir: TempDir,
}

impl Env {
    fn new(tag: &str) -> Env {
        Env { dir: tempdir(tag) }
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_ni-cli"));
        let d = |sub: &str| self.dir.join(sub).display().to_string();
        c.args(["--log-dir", &d("logs"), "--cache-dir", &d("cache")])
            .args(["--profiles-dir", &d("profiles")])
            .args(args)
            .env_remove("NI_LOG")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        c
    }

    async fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = tokio::time::timeout(WAIT, self.cmd(args).output())
            .await
            .expect("ni-cli finished")
            .unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// Everything written to the log directory.
    fn logs(&self) -> String {
        let mut text = String::new();
        for entry in std::fs::read_dir(self.dir.join("logs")).unwrap().flatten() {
            text += &std::fs::read_to_string(entry.path()).unwrap();
        }
        text
    }
}

/// The next JSON line on `lines` matching `pred`.
async fn json_line(
    lines: &mut Lines<BufReader<ChildStdout>>,
    pred: impl Fn(&Value) -> bool,
) -> Value {
    let end = tokio::time::Instant::now() + WAIT;
    loop {
        let line = tokio::time::timeout_at(end, lines.next_line())
            .await
            .expect("expected line in time")
            .unwrap()
            .expect("stdout still open");
        let v: Value = serde_json::from_str(&line).unwrap_or_else(|e| panic!("{line}: {e}"));
        if pred(&v) {
            return v;
        }
    }
}

fn addr(v: &Value, key: &str) -> SocketAddr {
    v[key].as_str().unwrap().parse().unwrap()
}

#[tokio::test]
async fn help_and_usage_errors() {
    let env = Env::new("cli-help");
    let (code, out, _) = env.run(&["--help"]).await;
    assert_eq!(code, 0);
    assert!(out.contains("Exit codes: 0 success"), "{out}");
    let (code, _, err) = env.run(&["flash", "--iface", "lo0"]).await;
    assert_eq!(code, 2);
    assert!(err.contains("--profile"), "{err}");
    let (code, _, _) = env
        .run(&["flash", "--iface", "lo0", "--version", "6.0"])
        .await;
    assert_eq!(code, 2);
}

#[tokio::test]
async fn interfaces_lists_lo0_with_all() {
    let env = Env::new("cli-ifaces");
    let (code, out, _) = env.run(&["--json", "interfaces", "--all"]).await;
    assert_eq!(code, 0);
    let list: Vec<Value> = serde_json::from_str(&out).unwrap();
    let lo0 = list
        .iter()
        .find(|i| i["name"] == "lo0")
        .expect("lo0 listed");
    assert_eq!(lo0["kind"], "loopback");
    assert!(lo0["ipv4"]
        .as_array()
        .unwrap()
        .contains(&Value::from("127.0.0.1/8")));
    let (_, out, _) = env.run(&["--json", "interfaces"]).await;
    let list: Vec<Value> = serde_json::from_str(&out).unwrap();
    assert!(
        list.iter().all(|i| i["name"] != "lo0"),
        "hidden without --all"
    );
    assert!(env.logs().contains("ni-cli started"));
}

#[tokio::test]
async fn profiles_can_be_saved_listed_shown_and_deleted() {
    let env = Env::new("cli-profiles");
    let script = env.dir.join("Lab.rsc");
    std::fs::write(&script, b"/system identity set name=lab\n").unwrap();
    let script = script.display().to_string();
    let save = [
        "profiles",
        "save",
        "--name",
        "Lab",
        "--version",
        "long-term",
        "--package",
        "wifi-qcom",
        "--script",
        &script,
    ];
    let (code, out, err) = env.run(&save).await;
    assert_eq!(code, 0, "{err}");
    assert_eq!(out, "saved profile Lab\n");
    let (code, out, _) = env.run(&["--json", "profiles", "list"]).await;
    assert_eq!(code, 0);
    let list: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        list,
        serde_json::json!([{"name": "Lab", "version": "long-term",
            "extra_packages": ["wifi-qcom"],
            "config": {"mode": "custom_script", "local_name": "lab.rsc"}}])
    );
    let (_, out, _) = env.run(&["profiles", "list"]).await;
    assert!(out.starts_with("Lab "), "{out}");
    assert!(
        out.contains("routeros,wifi-qcom") && out.contains("script lab.rsc"),
        "{out}"
    );
    let (code, out, _) = env.run(&["profiles", "show", "lab"]).await;
    assert_eq!(code, 0);
    assert!(out.contains("\"local_name\": \"lab.rsc\""), "{out}");
    assert_eq!(env.run(&["profiles", "delete", "LAB"]).await.0, 0);
    let (code, _, err) = env.run(&["profiles", "show", "Lab"]).await;
    assert_eq!(code, 1);
    assert!(err.contains("profile_not_found"), "{err}");
    let (code, _, err) = env
        .run(&["profiles", "save", "--name", "a/b", "--version", "stable"])
        .await;
    assert_eq!(code, 1);
    assert!(err.contains("invalid_name"), "{err}");
}

#[tokio::test]
async fn preflight_failure_exits_with_4() {
    let env = Env::new("cli-preflight");
    let (code, out, _) = env
        .run(&["--json", "preflight", "--iface", "nosuch0"])
        .await;
    assert_eq!(code, 4);
    let v: Value = serde_json::from_str(out.lines().next().unwrap()).unwrap();
    assert_eq!(
        (
            v["event"].as_str(),
            v["severity"].as_str(),
            v["code"].as_str()
        ),
        (Some("preflight"), Some("error"), Some("unknown_interface"))
    );
    let (code, out, _) = env
        .run(&[
            "preflight",
            "--iface",
            "lo0",
            "--server-mac",
            "02:00:00:00:00:01",
            "--bootp-port",
            "0",
            "--tftp-port",
            "0",
            "--netinstall-port",
            "0",
        ])
        .await;
    assert_eq!(code, 0);
    assert!(
        out.contains("warning: unsuitable_interface (interface: lo0, kind: loopback)"),
        "{out}"
    );
    assert!(out.ends_with("preflight passed\n"), "{out}");
}

/// Common `flash` arguments for `lo0` with the simulated LAN.
fn flash_args(srv: &TestServer, lan: &SimLan) -> Vec<String> {
    [
        "--json",
        "--download-base",
        &format!("{}/dl/", srv.base),
        "--upgrade-base",
        &format!("{}/up", srv.base),
        "flash",
        "--iface",
        "lo0",
        "--version",
        "stable",
        "--package",
        "wireless",
        "--server-mac",
        "02:00:00:00:00:01",
        "--bootp-port",
        "0",
        "--tftp-port",
        "0",
        "--netinstall-port",
        "0",
        "--bootp-reply-to",
        &lan.bootp_addr().to_string(),
        "--netinstall-dest",
        &lan.netinstall_addr().to_string(),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn flash_installs_one_simulated_device_and_exits() {
    let env = Env::new("cli-flash");
    let srv = TestServer::start().await;
    let (_, main, wireless) = mirror(&srv);
    let lan = SimLan::bind().await.unwrap();
    let args = flash_args(&srv, &lan);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut child = env.cmd(&args).spawn().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();

    let warning = json_line(&mut lines, |v| v["event"] == "preflight").await;
    assert_eq!(warning["severity"], "warning");
    let started = json_line(&mut lines, |v| v["event"] == "started").await;
    lan.connect(addr(&started, "netinstall")).await;
    json_line(&mut lines, |v| v["event"] == "kernels_ready").await;

    let reply = lan
        .boot(DEVICE, "ARM64__boot", addr(&started, "bootp"), WAIT)
        .await
        .expect("BOOTP reply");
    let tftp = SocketAddr::from((reply.server, addr(&started, "tftp").port()));
    tftp_get(tftp, &reply.file).await.unwrap();
    let mut sim = SimConfig::new(DEVICE, "hAP ax2", "arm64");
    sim.hello_interval = Duration::from_millis(100);
    lan.add(DeviceSim::new(sim, Instant::now())).await;

    let done = json_line(&mut lines, |v| {
        v["event"] == "device" && v["state"] == "done"
    })
    .await;
    assert_eq!(done["mac"], "48:A9:8A:00:00:0C");
    json_line(&mut lines, |v| v["event"] == "stopped").await;
    let status = tokio::time::timeout(WAIT, child.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.code(), Some(0));

    let report = lan.wait_terminated(DEVICE, WAIT).await.unwrap();
    let files: Vec<(&str, &[u8])> = report
        .received
        .iter()
        .map(|f| (f.name.as_str(), f.data.as_slice()))
        .collect();
    assert_eq!(
        files,
        [
            ("routeros-7.25.1-arm64.npk", main.as_slice()),
            ("wireless-7.25.1-arm64.npk", wireless.as_slice())
        ]
    );
    let logs = env.logs();
    assert!(logs.contains("device flashed"), "{logs}");
    assert!(logs.contains("flash finished"), "{logs}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ctrl_c_stops_auto_repeat_cleanly() {
    let env = Env::new("cli-sigint");
    let srv = TestServer::start().await;
    mirror(&srv);
    let lan = SimLan::bind().await.unwrap();
    let mut args = flash_args(&srv, &lan);
    args.push("--auto-repeat".into());
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut child = env.cmd(&args).spawn().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    json_line(&mut lines, |v| v["event"] == "kernels_ready").await;
    let pid = child.id().unwrap().to_string();
    let kill = std::process::Command::new("kill")
        .args(["-INT", &pid])
        .status()
        .unwrap();
    assert!(kill.success());
    json_line(&mut lines, |v| v["event"] == "stopped").await;
    let status = tokio::time::timeout(WAIT, child.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        status.code(),
        Some(0),
        "auto-repeat ends normally on Ctrl-C"
    );
}
