//! The `ni-cli` binary: arguments, exit codes, and (Task 5) a real `flash`
//! run against a simulated RouterBOARD over loopback.

use ni_assets::testutil::{tempdir, TempDir};
use serde_json::Value;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

const WAIT: Duration = Duration::from_secs(20);

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
