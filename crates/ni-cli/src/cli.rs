//! Command-line arguments.

use clap::{Args, Parser, Subcommand, ValueEnum};
use ni_assets::VersionSpec;
use ni_proto::Mac;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;

/// Exit codes (also listed in `--help`).
pub mod exit {
    /// Success.
    pub const OK: u8 = 0;
    /// Runtime error: engine, network, assets, profile store.
    pub const ERROR: u8 = 1;
    /// Invalid arguments.
    pub const USAGE: u8 = 2;
    /// A device failed to flash: the single device, or with
    /// `--auto-repeat` any device.
    pub const DEVICE_FAILED: u8 = 3;
    /// Preflight found an error (interface, addresses, ports, assets).
    pub const PREFLIGHT: u8 = 4;
    /// Interrupted (Ctrl-C / SIGTERM) before the single device finished,
    /// or while any device was being prepared or flashed.
    pub const INTERRUPTED: u8 = 130;
}

const EXIT_CODES: &str = "Exit codes: 0 success, 1 error, 2 invalid arguments, \
3 a device failed, 4 preflight failed, 130 interrupted (also: stopped while a device \
was being flashed). While a device is being flashed, the first Ctrl-C only warns; \
press it again within 5 s to abort. With --count, devices still being flashed when the \
count is reached are waited for. With --json, errors are also printed to stdout as \
{\"event\":\"error\",\"code\":...,\"message\":...}.";

/// Environment variable that unlocks the hidden testing flags.
pub const TESTING_ENV: &str = "NI_CLI_TESTING";

/// Hidden testing flags that redirect downloads or frames
/// (`--download-base`, `--upgrade-base`, `--netinstall-dest`) are refused
/// unless [`TESTING_ENV`] is `1`: pointed at the wrong place they would
/// install foreign files or send frames elsewhere. Error: `testing_flag_not_allowed` (`flag`, `env`).
pub fn check_testing_flags(cli: &Cli, allowed: bool) -> Result<(), ni_engine::Failure> {
    if allowed {
        return Ok(());
    }
    let net = match &cli.command {
        Command::Preflight { net, .. } => Some(net),
        Command::Flash(f) => Some(&f.net),
        _ => None,
    };
    let used = [
        ("--download-base", cli.download_base.is_some()),
        ("--upgrade-base", cli.upgrade_base.is_some()),
        (
            "--netinstall-dest",
            net.is_some_and(|n| n.netinstall_dest.is_some()),
        ),
    ];
    match used.iter().find(|(_, on)| *on) {
        Some((flag, _)) => Err(ni_engine::Failure::new("testing_flag_not_allowed")
            .with("flag", flag)
            .with("env", format!("{TESTING_ENV}=1"))),
        None => Ok(()),
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "ni-cli",
    version,
    about = "MikroTik Netinstall for macOS: boot RouterBOARDs over Ethernet and install RouterOS.",
    after_help = EXIT_CODES
)]
pub struct Cli {
    /// Print results and engine events as JSON (one object per line).
    #[arg(long, global = true)]
    pub json: bool,
    /// Download cache [default: ~/Library/Caches/io.github.sepsig.mac-netinstall].
    #[arg(long, global = true, value_name = "DIR")]
    pub cache_dir: Option<PathBuf>,
    /// Profile directory [default: ~/Library/Application Support/io.github.sepsig.mac-netinstall/profiles].
    #[arg(long, global = true, value_name = "DIR")]
    pub profiles_dir: Option<PathBuf>,
    /// Log directory [default: ~/Library/Logs/io.github.sepsig.mac-netinstall].
    #[arg(long, global = true, value_name = "DIR")]
    pub log_dir: Option<PathBuf>,
    /// Log filter for the log file, e.g. `debug` or `ni_engine=trace`.
    #[arg(long, global = true, env = "NI_LOG", default_value = "info")]
    pub log_level: String,
    /// Testing: base URL replacing `https://download.mikrotik.com/routeros/`.
    #[arg(long, global = true, hide = true)]
    pub download_base: Option<String>,
    /// Testing: base URL replacing `https://upgrade.mikrotik.com/routeros`.
    #[arg(long, global = true, hide = true)]
    pub upgrade_base: Option<String>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List the interfaces a RouterBOARD can be netinstalled through (real
    /// wired Ethernet NICs: built-in, USB, Thunderbolt or dock adapters),
    /// with their kind and IPv4 addresses.
    Interfaces {
        /// List every interface (Wi-Fi, Thunderbolt IP ports, bridges,
        /// tunnels, virtual and loopback ones too).
        #[arg(long)]
        all: bool,
    },
    /// Check an interface, the ports and (optionally) a profile's assets.
    Preflight {
        #[command(flatten)]
        net: NetArgs,
        /// Also check this stored profile's assets.
        #[arg(long)]
        profile: Option<String>,
    },
    /// Manage the download cache.
    Assets {
        #[command(subcommand)]
        command: AssetsCommand,
    },
    /// Manage stored profiles.
    Profiles {
        #[command(subcommand)]
        command: ProfilesCommand,
    },
    /// Serve BOOTP/TFTP/Netinstall on an interface and flash devices.
    ///
    /// Without --auto-repeat the first device that appears is flashed and
    /// the command exits. With it, every new device is flashed until
    /// Ctrl-C (or --count devices have finished).
    Flash(FlashArgs),
}

#[derive(Debug, Subcommand)]
pub enum AssetsCommand {
    /// Download (or verify cached) boot kernels and packages.
    Fetch {
        /// Channel (stable, long-term, testing, development, v6-long-term) or version (7.25.1, 6.49.23).
        #[arg(long)]
        version: VersionSpec,
        /// Package architecture(s) to fetch packages for (arm64, mipsbe, ...).
        #[arg(long = "arch")]
        archs: Vec<String>,
        /// Extra package(s) besides routeros.
        #[arg(long = "package")]
        packages: Vec<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum ProfilesCommand {
    /// List stored profiles.
    List,
    /// Print one profile as JSON.
    Show { name: String },
    /// Create or replace a profile.
    Save {
        #[arg(long)]
        name: String,
        #[command(flatten)]
        recipe: RecipeArgs,
    },
    /// Delete a profile.
    Delete { name: String },
}

/// What to install, given on the command line.
#[derive(Debug, Clone, Args)]
pub struct RecipeArgs {
    /// Channel (stable, long-term, testing, development, v6-long-term) or version (7.25.1, 6.49.23).
    #[arg(long)]
    pub version: VersionSpec,
    /// Extra package(s) besides routeros, e.g. wifi-qcom.
    #[arg(long = "package")]
    pub packages: Vec<String>,
    #[command(flatten)]
    pub config: ConfigArgs,
}

/// The device configuration options; the official netinstall-cli's
/// `-r` / `-e` / `-s` / `-sm` / `-b` / `-k` / `--reboot` / `--shutdown`.
#[derive(Debug, Clone, Default, Args)]
pub struct ConfigArgs {
    /// The device's configuration after the install: keep the existing
    /// one, RouterOS's default configuration, or none at all [default:
    /// default; with --script: the script].
    #[arg(long, value_enum)]
    pub config: Option<ConfigArg>,
    /// RouterOS script (.rsc) to run on first boot (not with --config keep
    /// or empty).
    #[arg(long)]
    pub script: Option<PathBuf>,
    /// Advanced: the official tool's mode script (.rsc, sent as mode.scr;
    /// not with --config keep).
    #[arg(long, value_name = "FILE")]
    pub mode_script: Option<PathBuf>,
    /// Remove branding (the official tool's -b).
    #[arg(long)]
    pub remove_branding: bool,
    /// Software license key file (the official tool's -k).
    #[arg(long, value_name = "FILE")]
    pub license_key: Option<PathBuf>,
    /// When the install is done: reboot, reboot with the message
    /// "Installation successful", or shut down [default: reboot].
    #[arg(long, value_enum)]
    pub after: Option<AfterArg>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ConfigArg {
    Keep,
    Default,
    Empty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum AfterArg {
    Reboot,
    RebootMessage,
    Shutdown,
}

#[derive(Debug, Clone, Args)]
pub struct NetArgs {
    /// Interface the routers are connected to, e.g. en7.
    #[arg(long)]
    pub iface: String,
    /// Advanced: allow an interface that is not a wired Ethernet adapter
    /// (Wi-Fi, a Thunderbolt IP port, a bridge, a virtual one); it is then
    /// only a warning. RouterBOARDs netinstall over wired Ethernet only.
    #[arg(long)]
    pub allow_any_interface: bool,
    /// Advanced override of our IPv4 address on it. By default it is the
    /// adapter's manual IPv4 from macOS (System Settings → Network → TCP/IP;
    /// known even before the cable is plugged in), else its first address.
    #[arg(long)]
    pub server_ip: Option<Ipv4Addr>,
    /// Netmask for --server-ip while it is not on the interface [default: 255.255.255.0].
    #[arg(long, requires = "server_ip")]
    pub netmask: Option<Ipv4Addr>,
    /// Address range handed out, as FIRST-LAST [default: the subnet's hosts].
    #[arg(long, value_parser = parse_pool)]
    pub pool: Option<(Ipv4Addr, Ipv4Addr)>,
    /// Testing: listener address [default: 0.0.0.0].
    #[arg(long, hide = true)]
    pub bind_ip: Option<Ipv4Addr>,
    /// Testing: BOOTP port [default: 67].
    #[arg(long, hide = true)]
    pub bootp_port: Option<u16>,
    /// Testing: TFTP port [default: 69].
    #[arg(long, hide = true)]
    pub tftp_port: Option<u16>,
    /// Testing: Netinstall port [default: 5000].
    #[arg(long, hide = true)]
    pub netinstall_port: Option<u16>,
    /// Testing: BOOTP reply destination [default: 255.255.255.255:68].
    #[arg(long, hide = true)]
    pub bootp_reply_to: Option<SocketAddr>,
    /// Testing: Netinstall destination [default: 255.255.255.255:5000].
    #[arg(long, hide = true)]
    pub netinstall_dest: Option<SocketAddr>,
}

#[derive(Debug, Clone, Args)]
pub struct FlashArgs {
    #[command(flatten)]
    pub net: NetArgs,
    /// Use this stored profile.
    #[arg(long, required_unless_present = "version", conflicts_with_all = [
        "version", "packages", "config", "script", "mode_script", "remove_branding",
        "license_key", "after",
    ])]
    pub profile: Option<String>,
    /// Channel or version to install (instead of --profile).
    #[arg(long)]
    pub version: Option<VersionSpec>,
    /// Extra package(s) besides routeros (with --version).
    #[arg(long = "package")]
    pub packages: Vec<String>,
    /// Configuration options (with --version).
    #[command(flatten)]
    pub config: ConfigArgs,
    /// Keep going: flash every device that appears, until Ctrl-C.
    #[arg(long)]
    pub auto_repeat: bool,
    /// With --auto-repeat: start no new device once this many have finished,
    /// wait for those still being flashed, then exit.
    #[arg(long, requires = "auto_repeat")]
    pub count: Option<usize>,
    /// Serve only this device (MAC address).
    #[arg(long)]
    pub only_mac: Option<Mac>,
    /// Devices flashed at the same time (more than one only on request).
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u16).range(1..=64))]
    pub max_parallel: u16,
    /// Seconds a finished device is ignored (it reboots); at least 30.
    #[arg(long, default_value_t = 300, value_parser = clap::value_parser!(u64).range(30..))]
    pub cooldown: u64,
    /// Re-sends of an unanswered Netinstall frame (0 disables them; whether
    /// devices de-duplicate re-sent chunks is unverified on hardware).
    #[arg(long, default_value_t = 3)]
    pub retransmits: u32,
    /// Accept the disclaimer (printed without it): no warranty, Netinstall
    /// erases the device, use at your own risk. Or set
    /// MAC_NETINSTALL_ACCEPT_DISCLAIMER=1.
    #[arg(long)]
    pub accept_disclaimer: bool,
}

/// The environment variable that accepts the disclaimer like
/// `--accept-disclaimer`.
pub const ACCEPT_DISCLAIMER_ENV: &str = "MAC_NETINSTALL_ACCEPT_DISCLAIMER";

fn parse_pool(s: &str) -> Result<(Ipv4Addr, Ipv4Addr), String> {
    let (a, b) = s
        .split_once('-')
        .ok_or_else(|| "expected FIRST-LAST, e.g. 192.168.88.10-192.168.88.200".to_string())?;
    let parse = |x: &str| {
        x.trim()
            .parse::<Ipv4Addr>()
            .map_err(|e| format!("{x:?}: {e}"))
    };
    Ok((parse(a)?, parse(b)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;
    use clap::CommandFactory;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("ni-cli").chain(args.iter().copied()))
    }

    #[test]
    fn definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn any_interface_only_on_request() {
        let flash = |extra: &[&str]| {
            let mut args = vec!["flash", "--iface", "en1", "--version", "stable"];
            args.extend(extra);
            let Command::Flash(f) = parse(&args).unwrap().command else {
                panic!("flash expected")
            };
            f.net.allow_any_interface
        };
        assert!(!flash(&[]));
        assert!(flash(&["--allow-any-interface"]));
        let cli = parse(&["preflight", "--iface", "en1", "--allow-any-interface"]).unwrap();
        let Command::Preflight { net, .. } = cli.command else {
            panic!("preflight expected")
        };
        assert!(net.allow_any_interface);
    }

    #[test]
    fn one_device_at_a_time_by_default() {
        let cli = parse(&["flash", "--iface", "en7", "--version", "stable"]).unwrap();
        let Command::Flash(f) = cli.command else {
            panic!("flash expected")
        };
        assert_eq!(f.max_parallel, 1);
        let mut cmd = Cli::command();
        let flash = cmd.find_subcommand_mut("flash").unwrap();
        let help = flash.render_long_help().to_string();
        assert!(help.contains("[default: 1]"), "{help}");
    }

    #[test]
    fn flash_with_version_packages_and_options() {
        let cli = parse(&[
            "--json",
            "flash",
            "--iface",
            "en7",
            "--version",
            "stable",
            "--package",
            "wifi-qcom",
            "--package",
            "container",
            "--script",
            "setup.rsc",
            "--auto-repeat",
            "--count",
            "3",
            "--only-mac",
            "48:a9:8a:00:00:01",
            "--max-parallel",
            "2",
            "--retransmits",
            "0",
            "--pool",
            "192.168.88.10-192.168.88.20",
        ])
        .unwrap();
        assert!(cli.json);
        let Command::Flash(f) = cli.command else {
            panic!("flash expected")
        };
        assert_eq!(f.net.iface, "en7");
        assert_eq!(f.version, Some("stable".parse().unwrap()));
        assert_eq!(f.packages, ["wifi-qcom", "container"]);
        assert_eq!(f.config.script, Some(PathBuf::from("setup.rsc")));
        assert!(f.auto_repeat);
        assert_eq!(f.count, Some(3));
        assert_eq!(f.only_mac, Some(Mac([0x48, 0xa9, 0x8a, 0, 0, 1])));
        assert_eq!((f.max_parallel, f.retransmits, f.cooldown), (2, 0, 300));
        assert_eq!(
            f.net.pool,
            Some((
                Ipv4Addr::new(192, 168, 88, 10),
                Ipv4Addr::new(192, 168, 88, 20)
            ))
        );
    }

    #[test]
    fn configuration_options_parse_and_conflict_with_a_profile() {
        let cli = parse(&[
            "flash",
            "--iface",
            "en7",
            "--version",
            "stable",
            "--config",
            "empty",
            "--mode-script",
            "m.rsc",
            "--remove-branding",
            "--license-key",
            "k.key",
            "--after",
            "reboot-message",
        ])
        .unwrap();
        let Command::Flash(f) = cli.command else {
            panic!("flash expected")
        };
        assert_eq!(f.config.config, Some(ConfigArg::Empty));
        assert_eq!(f.config.mode_script, Some(PathBuf::from("m.rsc")));
        assert!(f.config.remove_branding);
        assert_eq!(f.config.license_key, Some(PathBuf::from("k.key")));
        assert_eq!(f.config.after, Some(AfterArg::RebootMessage));
        for extra in [
            &["--config", "keep"][..],
            &["--remove-branding"],
            &["--after", "shutdown"],
            &["--license-key", "k.key"],
        ] {
            let mut args = vec!["flash", "--iface", "en7", "--profile", "p"];
            args.extend_from_slice(extra);
            assert_eq!(
                parse(&args).unwrap_err().kind(),
                ErrorKind::ArgumentConflict,
                "{extra:?}"
            );
        }
        assert_eq!(
            parse(&[
                "profiles",
                "save",
                "--name",
                "x",
                "--version",
                "7.25.1",
                "--config",
                "maybe"
            ])
            .unwrap_err()
            .kind(),
            ErrorKind::InvalidValue
        );
    }

    #[test]
    fn flash_needs_a_profile_or_a_version_but_not_both() {
        let kind = |args: &[&str]| parse(args).unwrap_err().kind();
        assert_eq!(
            kind(&["flash", "--iface", "en7"]),
            ErrorKind::MissingRequiredArgument
        );
        assert_eq!(
            kind(&[
                "flash",
                "--iface",
                "en7",
                "--profile",
                "p",
                "--version",
                "stable"
            ]),
            ErrorKind::ArgumentConflict
        );
        assert_eq!(
            kind(&["flash", "--version", "stable"]),
            ErrorKind::MissingRequiredArgument
        );
        assert_eq!(
            kind(&["flash", "--iface", "en7", "--version", "6.29"]),
            ErrorKind::ValueValidation
        );
        assert_eq!(
            kind(&[
                "flash",
                "--iface",
                "en7",
                "--version",
                "stable",
                "--count",
                "2"
            ]),
            ErrorKind::MissingRequiredArgument
        );
        assert_eq!(
            kind(&[
                "flash",
                "--iface",
                "en7",
                "--version",
                "stable",
                "--max-parallel",
                "0"
            ]),
            ErrorKind::ValueValidation
        );
        assert_eq!(
            kind(&[
                "flash",
                "--iface",
                "en7",
                "--version",
                "stable",
                "--only-mac",
                "nope"
            ]),
            ErrorKind::ValueValidation
        );
        assert!(parse(&["flash", "--iface", "en7", "--profile", "lab"]).is_ok());
    }

    #[test]
    fn hidden_testing_flags_parse() {
        let cli = parse(&[
            "flash",
            "--iface",
            "lo0",
            "--version",
            "7.25.1",
            "--bootp-port",
            "0",
            "--tftp-port",
            "0",
            "--netinstall-port",
            "0",
            "--bootp-reply-to",
            "127.0.0.1:6800",
            "--netinstall-dest",
            "127.0.0.1:6801",
            "--bind-ip",
            "127.0.0.1",
            "--download-base",
            "http://127.0.0.1:1/dl/",
        ])
        .unwrap();
        let Command::Flash(f) = cli.command else {
            panic!()
        };
        assert_eq!(f.net.bootp_port, Some(0));
        assert_eq!(
            f.net.netinstall_dest,
            Some("127.0.0.1:6801".parse().unwrap())
        );
        assert_eq!(cli.download_base.as_deref(), Some("http://127.0.0.1:1/dl/"));
        let help = Cli::command().render_long_help().to_string();
        assert!(!help.contains("download-base"), "testing flags are hidden");
        assert!(help.contains("130 interrupted"));
    }

    #[test]
    fn cooldown_has_a_lower_bound() {
        let args = |c: &'static str| {
            parse(&[
                "flash",
                "--iface",
                "en7",
                "--version",
                "stable",
                "--cooldown",
                c,
            ])
        };
        assert_eq!(args("29").unwrap_err().kind(), ErrorKind::ValueValidation);
        let Command::Flash(f) = args("30").unwrap().command else {
            panic!()
        };
        assert_eq!(f.cooldown, 30);
    }

    #[test]
    fn testing_flags_need_the_environment_variable() {
        let ok = parse(&["flash", "--iface", "en7", "--version", "stable"]).unwrap();
        assert!(check_testing_flags(&ok, false).is_ok());
        for args in [
            &["--download-base", "http://x/", "interfaces"][..],
            &["--upgrade-base", "http://x/", "interfaces"],
            &[
                "preflight",
                "--iface",
                "en7",
                "--netinstall-dest",
                "127.0.0.1:1",
            ],
        ] {
            let cli = parse(args).unwrap();
            let err = check_testing_flags(&cli, false).unwrap_err();
            assert_eq!(err.code, "testing_flag_not_allowed", "{args:?}");
            assert_eq!(err.param("env"), Some("NI_CLI_TESTING=1"));
            assert!(check_testing_flags(&cli, true).is_ok());
        }
        // Ports and the bind address stay available (they cannot redirect
        // anything off the machine).
        let cli = parse(&["preflight", "--iface", "lo0", "--bootp-port", "0"]).unwrap();
        assert!(check_testing_flags(&cli, false).is_ok());
    }

    #[test]
    fn netmask_needs_a_server_ip() {
        let base = ["flash", "--iface", "en7", "--version", "stable"];
        let with = |extra: &[&'static str]| {
            let mut v = base.to_vec();
            v.extend_from_slice(extra);
            parse(&v)
        };
        assert_eq!(
            with(&["--netmask", "255.255.255.0"]).unwrap_err().kind(),
            ErrorKind::MissingRequiredArgument
        );
        let cli = with(&["--server-ip", "192.168.88.1", "--netmask", "255.255.0.0"]).unwrap();
        let Command::Flash(f) = cli.command else {
            panic!()
        };
        assert_eq!(f.net.netmask, Some(Ipv4Addr::new(255, 255, 0, 0)));
    }

    #[test]
    fn other_subcommands() {
        assert!(matches!(
            parse(&["interfaces", "--all"]).unwrap().command,
            Command::Interfaces { all: true }
        ));
        let cli = parse(&[
            "profiles",
            "save",
            "--name",
            "lab",
            "--version",
            "long-term",
        ])
        .unwrap();
        let Command::Profiles {
            command: ProfilesCommand::Save { name, recipe },
        } = cli.command
        else {
            panic!()
        };
        assert_eq!(name, "lab");
        assert_eq!(recipe.version, "long-term".parse().unwrap());
        let cli = parse(&["assets", "fetch", "--version", "7.25.1", "--arch", "arm64"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Assets {
                command: AssetsCommand::Fetch { .. }
            }
        ));
        assert!(parse(&["preflight", "--iface", "en7", "--profile", "lab"]).is_ok());
        assert_eq!(
            parse(&[
                "flash",
                "--iface",
                "en7",
                "--version",
                "stable",
                "--pool",
                "1.2.3.4"
            ])
            .unwrap_err()
            .kind(),
            ErrorKind::ValueValidation
        );
    }
}
