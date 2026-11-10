# Mac Netinstall – User guide

*Magyar változat: [user-guide.hu.md](user-guide.hu.md)*

> **Status:** validated on real hardware with a **hEX RB750Gr3** (`mmips`, RouterOS 7.25.1):
> all four configuration modes (default, empty, custom script, keep existing) and auto-repeat
> work; a **hAP ac²** (`arm`) was also installed with an extra package, and a
> **CRS125-24G-1S-2HnD** (`mipsbe`). Other architectures
> (arm64, smips, ppc, tile), the mode script, remove
> branding, the license key and the shutdown variant have **not yet been tested** on
> hardware. For such models keep a fallback: a device you can recover with MikroTik's
> official Netinstall (Windows/Linux) if needed.
>
> **Disclaimer:** the app is provided “as is”, without any warranty. Netinstall erases the
> device: use it at your own risk. The authors accept no liability for damage, including
> devices that become unusable (“bricked”). See [Disclaimer](#13-disclaimer).

## Contents

1. [What it is](#1-what-it-is)
2. [Requirements](#2-requirements)
3. [Installing the app](#3-installing-the-app)
4. [Preparing the Mac](#4-preparing-the-mac)
5. [Creating a profile](#5-creating-a-profile)
6. [Flashing a device step by step](#6-flashing-a-device-step-by-step)
7. [Several devices and auto-repeat](#7-several-devices-and-auto-repeat)
8. [Settings](#8-settings)
9. [Log and troubleshooting](#9-log-and-troubleshooting)
10. [Command line (ni-cli)](#10-command-line-ni-cli)
11. [Known limitations](#11-known-limitations)
12. [Feedback](#12-feedback)
13. [Disclaimer](#13-disclaimer)

## 1. What it is

Mac Netinstall is a macOS app (Apple Silicon) that re-installs RouterOS on MikroTik
RouterBOARD devices over Netinstall – for when MikroTik's official Netinstall does not run
on your Mac. It is not a MikroTik product and is not affiliated with MikroTik.

Netinstall **formats the device's storage and installs a clean RouterOS** on it. Files,
containers and other user data are lost. What happens to the configuration is decided by
the profile's configuration mode (see [section 5](#5-creating-a-profile)). According to
MikroTik's documentation, Netinstall does not erase the license key or the RouterBOOT
settings.

The app downloads the RouterOS boot kernels and packages from mikrotik.com at runtime and
keeps them locally; it does not ship any MikroTik binaries.

## 2. Requirements

- A **Mac** with Apple Silicon (M1 or later), **macOS 13** or later. Intel Macs are not
  supported.
- A **wired Ethernet adapter**: the built-in port, a USB or Thunderbolt adapter, or a
  dock's Ethernet port. Wi-Fi and Thunderbolt Bridge (IP over Thunderbolt) ports cannot be
  used, so the app does not show them.
- An **Ethernet cable** between the Mac and the RouterBOARD. Ideally the adapter is
  connected only to the router (not to your office network).
- **Internet** for the first download. Downloaded boot kernels and packages stay on the
  Mac, so the same version can later be installed offline.
- The **RouterBOARD** itself and its power supply. x86 and ARM64 UEFI devices are not
  supported.

## 3. Installing the app

1. From the [Releases](https://github.com/SepsiG/Mac-Netinstall/releases) page download
   `Mac-Netinstall_<version>_aarch64.dmg` and `Mac-Netinstall_<version>_aarch64.dmg.sha256`.
2. (Recommended) Verify the download. In Terminal, in the folder that holds both files:

   ```sh
   shasum -a 256 -c Mac-Netinstall_<version>_aarch64.dmg.sha256
   ```

   It prints `Mac-Netinstall_<version>_aarch64.dmg: OK`.
3. Open the `.dmg` and drag **Mac Netinstall** onto **Applications**.
4. **First launch.** The app is ad-hoc signed, not notarized by Apple, so macOS blocks it
   the first time.
   - **macOS 15 and later:** try to open it, then open System Settings → Privacy &
     Security, scroll to the message about "Mac Netinstall", click **Open Anyway** and
     confirm.
   - **macOS 13–14:** Control-click the app → Open works as well.
5. **Permissions.** At the first launch the app opens the **macOS permissions** sheet. Click
   **Request permissions**: macOS asks right away (not in the middle of an install)
   - whether Mac Netinstall may find and connect to devices on your local network
     (**Local network**) — **allow it**, the app cannot answer the devices without it;
   - if the macOS firewall is on, whether it may accept incoming connections — **allow it**.

   The sheet then shows what macOS answered (✓ / ✗) and, for anything not allowed, opens the
   right page of System Settings (Privacy & Security → Local Network, or the firewall). After
   changing a setting, click **Check again**. **Later** closes the sheet; it opens again at the
   next launch until you have requested the permissions. You can open it any time from
   **Settings → Check permissions**.

While it runs, the app is in the Dock; the Dock icon's badge shows how many devices are
being prepared or flashed.

## 4. Preparing the Mac

The app **does not change** your Mac's network settings (so it needs no admin rights).
The server address is the Ethernet adapter's **manual IPv4 address in macOS**, which you
set yourself:

1. Plug in the Ethernet adapter.
2. Open System Settings → Network → the adapter → Details → TCP/IP.
3. Configure IPv4: Manually, IP address `192.168.88.1`, subnet mask `255.255.255.0`, then
   OK.
4. If **Internet Sharing** is on (System Settings → General → Sharing), turn it off: its
   DHCP server conflicts with the app. The same goes for any other DHCP or TFTP server on
   the Mac.

Another subnet works too (/30 or larger); `192.168.88.1/24` is just the usual example.

### What the app shows

The **Interface** list at the top of the main window shows only usable wired Ethernet
interfaces (name, display name, current IPv4 address). Next to it is the selected
adapter's state:

| Shown | Meaning |
|---|---|
| **Manual: 192.168.88.1/24** (with your address) | Fine: a manual IPv4 address is set. |
| **DHCP – set it to manual** | The adapter gets its address over DHCP; set it to manual (see above). |
| **No IPv4 configured** | No address; set a manual IPv4 address. |
| **link up** | There is a link (cable plugged in, the other end powered on). |
| **no link – cable not plugged in?** | No link. You can start anyway; serving begins when the link comes up. |

The ↻ button (**Refresh the interface list**) reads the interfaces again. While stopped,
the app also re-checks the cable and the address every few seconds by itself.

If there is no suitable adapter at all, you see:
**No usable wired Ethernet interface. Connect a USB or Thunderbolt Ethernet adapter.**

The status line under the toolbar shows the result of the pre-start check (preflight).
When everything is fine it says **✓ Ready to start**. If something is missing, the
**To do:** lines say exactly what to set up; if you press **Start** anyway, they turn red
as errors and nothing starts.

## 5. Creating a profile

A profile describes what to install. Choose one in the **Profile** row of the main window,
or create one with **New…**; change an existing one with **Edit…**. Profiles are listed as
"name · version".

| Field | Description |
|---|---|
| **Name** | 1–64 letters, digits, spaces or `. _ - +`, not starting with a dot or a space. |
| **Packages from** | **Download from MikroTik** (default) or **Local package files (.npk)** (see [Installing from local .npk files](#installing-from-local-npk-files)). |
| **RouterOS version** | Chosen in the version picker (see [Choosing the version](#choosing-the-version)): a channel that always installs its current version, a concrete RouterOS 7 or archived 6.x version, or a version typed by hand. |
| **Extra packages** | The `routeros` system package is always installed; tick extra packages here (e.g. `wifi-qcom`). **List for** shows which packages exist for an architecture – it only filters the list; the app detects the device's architecture by itself. |
| **Configuration** | What happens to the configuration (see below). |
| **After install** | **Reboot**, **Reboot with the message “Installation successful”** or **Shut down**. |
| **Advanced** | Mode script, remove branding, license key, installing below the device's minimum version (see below). |

### Choosing the version

The **RouterOS version** list has three groups and a search box (**Search versions**):

- **Always the newest** – **Stable**, **Long-term**, **Testing**, **Development** and
  **RouterOS v6 (long-term)**, each with the version it names right now in parentheses.
  A profile with a channel always installs that channel's current version.
- **RouterOS 7** – concrete releases from MikroTik's changelog, newest first, with their
  channel (Stable, Long-term) and release date.
- **Archive (6.x, end of life)** – RouterOS 6 releases from 6.30 on.
- **Custom version…** – type a version (e.g. `7.20.8` or `6.49.23`); the app checks
  whether download.mikrotik.com publishes it (**published** / **not published on
  download.mikrotik.com**).

The list comes from mikrotik.com and is kept for a day; the line under it says how old it
is, **Refresh** fetches it again. Offline the last list (or a small built-in list) is used.

### RouterOS 6 (archive versions)

RouterOS 6 (6.30 to 6.49.x) can be installed too. Things to know:

- **RouterOS 6 is end of life.** Use it only when you have a reason to (e.g. an old
  device or a configuration that needs v6).
- **Never install a version older than the device's minimum (factory) version.** That can
  leave the device unusable. The device reports its minimum version when it is discovered;
  the app **refuses** an older version before anything is erased: **“RouterOS … is older
  than this device's minimum version …”**. Newer devices often do not support v6 at all.
- Old releases do not exist for every architecture (e.g. 6.30.4 has no `arm` or `mmips`
  package; `arm64` starts around 6.45): **“RouterOS … is not published for the …
  architecture.”**
- The device boots the current RouterOS 7 netinstall kernels, which install v6 packages.
- In v6 the main package bundles `system`, `ipv6`, `wireless`, `hotspot`, `mpls`,
  `routing`, `ppp`, `dhcp`, `security` and `advanced-tools`; the extra-package list shows
  only the real extras.
- The mode script needs RouterOS 7.22 or later, so it cannot be used with v6.

A suggestion for a first try: install `6.49.23` on a hEX you can recover.

### Installing from local .npk files

Choose **Local package files (.npk)** in **Packages from**, then **Choose .npk files…**
(several at once). The list shows each file's package name, architecture and version as
read from the file. Remove one with ×.

- One file must be a main (`system`/`routeros`) package; it is installed first.
- Every file must be built for the device's architecture; this is checked before the
  device is formatted.
- The installed version is read from the main package (shown as “version unknown” if it
  cannot be read; then the mode script cannot be used).
- The boot kernels still come from the current stable RouterOS 7 netinstall: that needs an
  internet connection once, or kept downloads (see [Settings](#8-settings)).
- The files are copied into the app's own folder; if one is deleted there, choose it again.

### Configuration modes

| Mode | What happens | When to use it |
|---|---|---|
| **Keep the existing configuration** | The device keeps its current settings. Per MikroTik's documentation this restores the configuration and users; user files and containers are not kept. | Repairing or re-installing RouterOS when the settings are fine. |
| **RouterOS default configuration** | RouterOS applies its factory default configuration on first boot. | A clean, "factory" state. **This is the app's default.** |
| **Empty configuration** | No configuration at all: reach the device through its MAC address (WinBox) or a console. | Setting up from scratch. |
| **Run my script (.rsc)** | Your RouterOS script runs on first boot instead of the default configuration. Pick the file with **Choose…**. | Batch installs with a common baseline. |

> **Note:** the official `netinstall-cli` keeps the existing configuration by default;
> this app defaults to **RouterOS default configuration**. If you want to keep the
> settings, choose that mode in the profile.

An empty script file is refused; to install without any configuration, choose **Empty
configuration**.

### Advanced options

- **Mode script (mode.scr)** – an extra RouterOS script sent to the device as `mode.scr`
  (the official tool's `-sm`). Per MikroTik's documentation it runs once, on first boot,
  before the other configuration scripts, and needs RouterOS 7.22 or later. Not together
  with keeping the existing configuration, and not for versions before 7.22 (RouterOS 6
  included).
- **Remove branding** – sends the official tool's `-b` flag. What a device does with it
  has not been verified.
- **License key:** – **Choose key file…**. Only for devices that need a software key (e.g.
  x86), like the official tool's `-k`. RouterBOARDs carry their license on the device, so
  you normally leave this empty. The key is stored in the profile file, which only your
  user can read.
- **Allow below the device's minimum version (risky)** – off by default. Normally a version
  older than the minimum version the device reports is refused before anything is erased,
  because it can make the device unusable. Turn it on only if you know the device supports
  that version.

**Save** stores the profile, **Cancel** closes the editor (it asks first if there are
unsaved changes). An existing profile can be removed with **Delete…**.

## 6. Flashing a device step by step

1. Connect the Mac's Ethernet adapter to the RouterBOARD's **boot port** (usually
   **ether1**, or the port labelled "BOOT"; if unsure, see the device's manual).
2. Choose the adapter in the **Interface** list and the profile in the **Profile** list.
   If the app should start installing on its own, tick **Auto-repeat** (see
   [section 7](#7-several-devices-and-auto-repeat)).
3. Wait until the status line shows **✓ Ready to start**, then click **Start**. The first
   time, the app shows the **Disclaimer**: tick **I understand and use it at my own risk**
   and click **Accept** (**Cancel** starts nothing). If needed,
   the app downloads the boot kernels now. On the right of the status line, **Boot
   kernels:** shows their state (e.g. `RouterOS 7.25.1`, with a percentage while
   downloading). While running, the left side shows a line like **Serving on en7
   (192.168.88.1)**.
4. Boot the RouterBOARD in **Etherboot (Netinstall) mode** – see below.
5. Watch the device table. With **Auto-repeat** on, installing starts by itself. Without
   it, the device stops at **Waiting for a profile**: click **Flash** in its row (the
   selected profile is installed).
6. When the status becomes **Done …** (e.g. "Done 2 minutes ago"), the install is
   finished and the device reboots or shuts down as the profile says. After the reboot it
   starts from its own storage with the new RouterOS.
7. When you are finished, click **Stop**.

### Etherboot mode on the RouterBOARD

MikroTik's official description:
[Netinstall – MikroTik documentation](https://help.mikrotik.com/docs/spaces/ROS/pages/24805390/Netinstall).
In short:

- **With the reset button:** power the device off, press and hold the reset button, power
  it on, and keep holding until the device appears in the app's device table.
- **From RouterOS** (if you can still reach it): run this, then reboot the device:

  ```
  /system routerboard settings set boot-device=try-ethernet-once-then-nand
  ```

  The device tries to boot from the network once, then boots from its own storage as
  usual.
- **Port:** ether1 or the port labelled "BOOT". If a device does not appear, check that it
  is plugged into the right port.
- If a protected bootloader is set up on the device, the reset button behaves differently;
  MikroTik's documentation explains it.

### The device table

Columns: **MAC address**, **Model**, **Arch**, **Status**, **Progress**, **Version**,
**Actions**. Under **Version** the configuration mode is shown too (e.g. "default
configuration"). If this is not the first attempt for a device, hovering over its status
shows which one (e.g. "attempt 2").

| Status | Meaning |
|---|---|
| **Booting** | The device is booting from the network (a BOOTP request arrived). |
| **Sending boot kernel** | The boot kernel is being sent to the device (TFTP), with progress. |
| **Found** | The boot kernel is running and the device reported in (model, architecture). |
| **Waiting for a profile** | No profile assigned: **Flash** or **Auto-repeat**. |
| **Waiting for a free slot** | As many devices are being flashed as Settings allow. |
| **Preparing** | Downloading/checking the needed packages. |
| **Starting**, **Formatting**, **Uploading**, **Finishing**, **Rebooting** | The install steps; **Progress** shows a percentage. |
| **Done …** | Installed successfully, with how long ago. |
| an error message (red row) | The install failed; the text gives the reason (see [section 9](#9-log-and-troubleshooting)). |

After you stop serving, the table shows the last known state with the note **Last known
state; the server is stopped.**

### Stopping or quitting while flashing

If you press **Stop** or quit the app while a device is being flashed, it asks first
(**Stop serving?** / **Quit Mac Netinstall?**). The reason: a half-formatted or
half-uploaded device can be left without a working system and then has to be netbooted
again. Choose **Keep going** (or **Cancel**) and wait for it to finish, unless you are
sure you want to abort.

## 7. Several devices and auto-repeat

- **Auto-repeat** (toolbar, only with a profile selected): flash every new device with
  this profile, without asking. For batches, boot the devices into Etherboot mode one
  after another.
- **Devices at once** (Settings): **1** by default, so one device is flashed at a time and
  the others are **Waiting for a free slot**. You can raise it up to 64; the boot kernel
  and packages share the same cable.
- Cooldown (Settings → "Ignore a flashed device for (seconds)"): 300 seconds (5
  minutes) by default, at least 30. For this long, a device that was just flashed is
  ignored so it is not flashed again while it reboots. Even afterwards it is served again
  only when it netboots again.
- **Only this** / **Serve all** (row buttons while running): **Only this** serves only that
  one device (the status line shows "Serving only …"); **Serve all** serves every device
  again. Useful when several routers are on the same network.
- **Retry** – try a failed install again.
- **Cancel** – stop preparing or flashing a device, or take a queued device out of the
  queue. It always asks first; while flashing it warns that the device can be left without
  a working system (**Stop install** / **Keep going**).
- **Forget** – removes the device from the table so it is recognised again. A device that
  was just flashed cannot be forgotten during its cooldown (the button is greyed out; its
  tooltip: "Just flashed and restarting: it can be forgotten once its cooldown is over.").
- **Dock:** the Dock icon's badge shows the number of devices being prepared or flashed;
  when a device finishes in the background, the icon bounces once.

## 8. Settings

Opened with the **Settings** button in the toolbar.

- **Language** – **Same as macOS**, **English** or **Magyar**.
- **Permissions** – **Check permissions** opens the macOS permissions sheet (see
  [section 3](#3-installing-the-app)).
- **Flashing** – **Devices at once** and the cooldown (see
  [section 7](#7-several-devices-and-auto-repeat)).
- **Advanced**
  - **Address pool** – addresses handed out to booting devices. Automatic by default: the
    adapter's subnet without its own address (the dialog shows the computed range).
    **Custom range** lets you enter your own range inside the subnet (not its first or last
    address); **Reset to automatic** goes back.
  - **Download timeout (minutes)** – 30 minutes by default; only needs increasing on very
    slow connections.
- **Downloaded RouterOS files** – the boot kernels and packages downloaded from
  mikrotik.com, per version, with their size and location
  (`~/Library/Caches/io.github.sepsig.mac-netinstall`). File states:

  | Shown | Meaning |
  |---|---|
  | **✓ verified** | Matches the published checksum. |
  | **no published checksum** | MikroTik publishes no checksum for it. |
  | **not checked yet** | Check it with **Verify files**. |
  | **damaged** / **incomplete** | Delete it (it is downloaded again when needed). |
  | **unfinished** | An interrupted download. |

  You can delete one file (×), a version (**Delete version**), the damaged files
  (**Delete damaged files**) or everything (**Delete all**); each asks first. The **Your
  files (scripts, packages)** group holds the scripts chosen for profiles; if you delete
  them, choose the file again in the profile. A version in use cannot be deleted:
  **"RouterOS … is in use right now (served or being installed). Stop first, or delete
  other files."**

- **Keep downloaded files after quitting** (under **Downloaded RouterOS files**) – off by
  default: the downloaded kernels and packages are **deleted when the app quits** (and
  leftovers at the next launch). Your own files (scripts, packages) and the version lists
  are always kept. Turn it on for offline work or when you install many devices over
  several days: without kept files the app cannot fall back to the last used version
  offline.
- **About** – the app's version, its license (GNU GPL v3), the disclaimer and links to the
  GitHub page, the full disclaimer and the license.

**Save** stores the settings, **Cancel** discards the changes.

## 9. Log and troubleshooting

### Log

The **Log** panel at the bottom of the window can be expanded; the newest line is at the
top. Drag the line above it to make it taller or shorter (or focus it and use the ↑/↓
keys; double-click restores the default height); the app remembers the height. With the
**All devices** selector you can filter to one device (MAC address). **Pause** holds the
list still so you can read it (scrolling down in the list does the same); new lines are
still collected, a badge counts them, and **Resume** (or scrolling back to the top) shows
them. **Export…** saves every kept line (of the chosen device) to a text file, oldest
first; **Clear** empties the panel. Identical lines in a row show once, with a count (e.g. ×93)
and the latest time; the export has every line.

The app also writes a detailed log file to
`~/Library/Logs/io.github.sepsig.mac-netinstall/` (`app.<date>.log`, and
`ni-cli.<date>.log` for `ni-cli`; a new file every day, the last 7 are kept). In Finder:
Go → Go to Folder…, and paste the path.

Errors also appear in a bar at the top of the window; close it with × (**Dismiss**).

### Common problems

| Message / symptom | What to do |
|---|---|
| **No usable wired Ethernet interface. …** | Connect a wired Ethernet adapter, then ↻. Wi-Fi and Thunderbolt Bridge cannot be used. |
| **… has no manual IPv4 address in macOS.** / **… has no IPv4 address.** | Set a manual IPv4 address ([section 4](#4-preparing-the-mac)). |
| **… gets its IPv4 address over DHCP.** | Switch it to manual: System Settings → Network → the adapter → Details → TCP/IP → Configure IPv4: Manually. |
| **no link – cable not plugged in?** / **Cable not connected or the device is off — you can start now; serving begins when the link is up.** / while running **No link: no cable** | Plug in the cable and power on the router. macOS hides a fixed address while there is no cable; you can start anyway. |
| **Port 67 (BOOTP) is used by another program.** (or 69, TFTP) | Another DHCP or TFTP server is running, most likely Internet Sharing (System Settings → General → Sharing). Turn it off and try again. |
| **… carries the default route, so it looks like a real network.** | A warning: other machines on that network could get addresses from the app. Use a separate adapter connected only to the router. |
| **The subnet (/…) has no address to hand out besides the server's own; …** | Use a /30 or larger subnet. |
| **Boot kernels:** **unavailable** / **No boot kernels: …** | The first download needs the internet; afterwards they stay in the cache. While running, try again with **Retry download**. Offline, this is shown: **Offline: using RouterOS … from the cache.** |
| **The RouterOS version cannot be determined: …** | Check the internet connection, or the version in the profile. |
| The device does not appear in the table | Etherboot mode did not work (try the reset button again, holding it longer), the wrong port is used (ether1/BOOT), a bad cable, or no link. Check the log: a **Not served: …** line gives the reason (e.g. "just flashed (cooldown)", "another device is selected", "unsupported device type"). |
| **The device stopped answering (step: …).** | The connection was lost during the install. Check the cable, boot the device into Etherboot mode again and click **Retry**. |
| **The device refused the install (step: …).** | The device did not accept the install. Boot it into Etherboot mode again and retry; if it keeps happening, send a log. |
| **The device stopped receiving with … still to send: the install is incomplete.** | Install it again. |
| **… received every file but never confirmed it was ready; the install was finished anyway. Check that the device starts up.** | A warning: the install was closed, but the device did not confirm it. Check that it boots properly; if not, install again. Please send a log in this case. |
| **This device booted the RouterOS … kernel, so it cannot install …. Power it off and on to netboot again.** | You changed the profile (version) after the device booted. Boot it into Etherboot mode again. |
| **… is built for “…”, but this device reports “…”. Nothing was installed and the device was not formatted.** | A package does not match the device's architecture (every package names the architecture it is built for; the app checks them all before it starts). The device is untouched. Check that the right model is connected; if it keeps happening, report it (see [Feedback](#12-feedback)). |
| **No RouterOS package is built for this device's architecture “…” … Please report it …** | The device reports an architecture the app could not match to any RouterOS download. Nothing was installed. Please open a [GitHub issue](https://github.com/SepsiG/Mac-Netinstall/issues) with the whole message (it lists the model and the device's hello) and the exported log. |
| **macOS blocks this app's local network access: the devices get no answer.** (red, in the status line and in the device's row) | The Local Network permission is missing or the macOS question is still unanswered. Click **Open System Settings** (Privacy & Security → Local Network), turn Mac Netinstall on, then netboot the device again. The permissions sheet also opens once to check it. |
| **Sending failed (Netinstall, …×).** | A send failed for another reason (e.g. the adapter went away). If it keeps happening, check the cable and the adapter, and the permissions (**Settings → Check permissions**). |
| **The engine stopped unexpectedly. Restart Mac Netinstall.** | Restart the app and send the log file. |

## 10. Command line (ni-cli)

`ni-cli` uses the same engine from the command line (for debugging, scripting and hardware
testing). **It is not included in the DMG**; build it from source (Rust via rustup):

```sh
git clone https://github.com/SepsiG/Mac-Netinstall.git
cd Mac-Netinstall
cargo build --release -p ni-cli
# the binary: target/release/ni-cli
```

Main commands: `interfaces`, `preflight`, `assets`, `profiles`, `flash`. `flash` prints
the disclaimer and refuses to run until you accept it with `--accept-disclaimer` (or by
setting `MAC_NETINSTALL_ACCEPT_DISCLAIMER=1`). It shares the
profiles and downloaded files with the app. Examples:

```sh
# List usable interfaces
ni-cli interfaces

# Check with a stored profile
ni-cli preflight --iface en7 --profile "hAP base"

# One device: stable RouterOS, keeping the existing configuration
ni-cli flash --accept-disclaimer --iface en7 --version stable --config keep

# A batch: own script, every new device, until Ctrl-C;
# without re-sends (see Known limitations)
ni-cli flash --accept-disclaimer --iface en7 --version 7.25.1 --package wifi-qcom \
  --script base.rsc --auto-repeat --retransmits 0
```

Without `--auto-repeat` the first device that appears is flashed and the command exits.
While a device is being flashed, the first Ctrl-C only warns; pressing it again within 5
seconds aborts. All options: `ni-cli --help` and `ni-cli <command> --help`.

## 11. Known limitations

- **Only partly validated on real hardware.** Tested so far: a hEX RB750Gr3 (`mmips`) with
  the default, empty, script and keep configuration modes and auto-repeat, a hAP ac²
  (`arm`) with an extra package, and a CRS125-24G-1S-2HnD (`mipsbe`). Not yet tested: arm64,
  smips, ppc and tile (whether their
  boot kernels boot), the mode script, remove
  branding, the license key and the shutdown variant. Keep the official Netinstall as a
  fallback for untested models. The open points are listed in the
  [roadmap](superpowers/plans/2026-11-06-00-roadmap.md).
- By default the app re-sends unanswered Netinstall frames; whether real devices handle
  this correctly is unverified. If an install stalls at a step, `ni-cli`'s
  `--retransmits 0` turns it off.
- Apple Silicon Macs only, macOS 13 or later.
- x86 (PXE) and ARM64 UEFI devices are not supported. RouterOS 7 and RouterOS 6 from
  6.30 on; installing v6 with the RouterOS 7 netinstall kernels has not been tested on
  hardware yet, nor the oldest PowerPC boards or arm64 with v6.
- Ad-hoc signed, not notarized: the first launch has to be allowed (see
  [section 3](#3-installing-the-app)).
- Without the Local Network permission the app cannot reach devices.
- The app does not configure the Mac's network; you set the manual IPv4 address yourself.

## 12. Feedback

Report bugs or suggestions at
[GitHub Issues](https://github.com/SepsiG/Mac-Netinstall/issues).

Please attach:

- the exported log (**Log** → **Export…**) and/or the log file from
  `~/Library/Logs/io.github.sepsig.mac-netinstall/`;
- the device's model and architecture (the table's **Model** and **Arch** columns);
- the RouterOS version installed and, if you know it, the one the device ran before;
- the profile settings (version, packages, configuration mode, after install) –
  **do not send the license key**;
- your Mac model, macOS version and Ethernet adapter type.

## 13. Disclaimer

- Mac Netinstall is free software, provided **“as is”, without any warranty**, as stated
  in sections 15–16 of the [GNU General Public License v3](../LICENSE).
- Netinstall **erases the device's storage** and reinstalls RouterOS. **Use it at your own
  risk.**
- The authors and contributors accept **no liability for any damage or loss**, including
  devices becoming unusable (“bricked”), lost configuration or data, downtime, or damage
  from installing old, archived or end-of-life RouterOS versions, versions below the
  device's minimum version, or wrong packages.
- Test on a device you can recover (e.g. with MikroTik's official Netinstall) before using
  it in production.
- Mac Netinstall is not affiliated with, endorsed or supported by MikroTik. MikroTik,
  RouterOS and RouterBOARD are trademarks of their owner.

The app asks you to accept this once, before the first **Start** (and again if the text
changes).
