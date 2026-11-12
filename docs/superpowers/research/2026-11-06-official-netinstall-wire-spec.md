# Spike 5 — official netinstall-cli 7.25.1 wire spec

Setup: official netinstall-cli 7.25.1 in Ubuntu 26.04 VM, netns `ns-srv` (veth-srv 02:00:00:00:00:01, 192.168.88.1/24).
Device = `sim.py` in `ns-dev` (raw AF_PACKET, no IP; builds BOOTP/ARP/TFTP/Netinstall itself), MAC 64:D1:54:AA:BB:01,
`ARM64__boot`, RB5009UG+S+, arm64. Packages: routeros-7.25.1-arm64.npk (13984181 B), container-7.25.1-arm64.npk
(1187985 B); sha256 of these and the tarball match MikroTik's published `.sha256`.

## 0. Config modes

| Flags | OFFR | Extra files after packages | TERM |
|---|---|---|---|
| none (keep) | `OFFR\n\nkeep\nremoveBranding\n\0` | – | `TERM\n\0` |
| -r | `OFFR\n\n\nremoveBranding\n\0` | – | `TERM\n\0` |
| -e | = -r | autorun.scr (0 B, no data frame) | same |
| -s f | = -r | autorun.scr = f | same |
| -r -s f | byte-identical to -s | autorun.scr = f | same |
| -r -sm m | = -r | mode.scr = m | same |
| -e -sm m | = -r | mode.scr, then empty autorun.scr | same |
| -r -s f -sm m | = -r | autorun.scr, then mode.scr | same |
| -b | `OFFR\n\nkeep\n\n\0` | – | same |
| -r -b / -e -b | `OFFR\n\n\n\n\0` | (-e: empty autorun) | same |
| -k file | `OFFR\n<file with all \n removed>\nkeep\nremoveBranding\n\0` | – | same |
| --reboot | unchanged | – | `TERM\nInstallation successful\n\0` |
| --shutdown | unchanged | – | `TERM\nshutdown\n\0` |

CLI refuses `-e -s` ("Cannot apply empty config and script at the same time") and `-sm` without `-r`/`-e`
("Cannot apply mode script and keep old config at the same time").
Fields: 1 = license key (`-k`); 2 = `keep` iff none of -r/-e/-s; 3 = `removeBranding` iff `-b` **absent** (polarity as observed).
Trailing NUL counted in the length field.

OFFR hex: keep `4f4646520a0a6b6565700a72656d6f76654272616e64696e670a00`; -r/-e/-s `4f4646520a0a0a72656d6f76654272616e64696e670a00`;
-b `4f4646520a0a6b6565700a0a00`; -r -b `4f4646520a0a0a0a00`.

## 1. CLI usage (verbatim)

```
Version: 7.25.1(2026-11-02 13:11:17)
./netinstall-cli [-r] [-e] [-b] [-m [-o]] [-f] [-c] [-v] [--reboot] [--shutdown] [-k <keyfile>] [-s <userscript>] [-sm <modescript>] [--mac <mac address>] {-i <interface> | -a <client-ip>} [PACKAGE]+
	-r  apply default configuration
	-e  apply empty configuration
	    -r and -e are mutually exclusive
	    by default existing configuration will be kept
	-b  remove branding
	-m  allow multiple installs
	-o  install each device once (only with -m)
	-f  ignore size constraints
	-c  allow concurrent netinstall servers
	-v  verbose mode
```
(no args additionally: "No packages supplied", rc 1). Verified: `-i` (client = server IP + 1), `-a ip` (interface by subnet;
BOOTP yiaddr = that IP), `--mac` (others logged "Ignoring <mac>, wrong MAC address" at BOOTP), `-m` (process stays),
`-c` (no PING). System package is sent first regardless of command-line order; the rest in order.

## 2. Flow (e01_default)

```
PING\n / PONG\n (zero MACs, seq 0)                    startup probe
DEV  [dev][ff..ff] seq1 ack0 DSCV\nABCD-1234\n\nRB5009UG+S+\narm64\n7.1\nx\ny\n   (from 0.0.0.0)
SRV  [ff..ff][dev] seq1 ack0 OFFR\n\nkeep\nremoveBranding\n\0                     (~0.4 s after hello)
DEV  seq1 ack1 YACK\n
SRV  seq2 ack0 ""            DEV STRT
SRV  seq3 ack1 ""            DEV RETR
SRV  seq4 ack2 FILE\nrouteros-7.25.1-arm64.npk\n13984181\n\0   DEV RETR
SRV  1024-byte chunks …      DEV RETR each   (last 437 B, seq 13661)
SRV  seq13662 ""  (poll)     DEV RETR
SRV  seq13663 ""  (poll)     DEV WTRM
SRV  seq13664 TERM\n\0       (no reply)
```

- UDP 5000 → 255.255.255.255:5000; server sends from 192.168.88.1:5000 (SO_BINDTODEVICE, SO_BROADCAST); receives on SOCK_RAW/IPPROTO_UDP; a second socket bound to 255.255.255.255:5000 for PING.
- Header: server **swaps the MAC fields of the device frame it answers** (server `[0:6]` = device `[6:12]`; OFFR has `[0:6]=ff..ff`, `[6:12]=device`); never its own MAC. Bytes 12–13 = 0. Length includes the NUL of text messages. seq starts at 1 per session, +1 per new frame (reached 14830). ack = 0, 0, then seq−2. Device: `[0:6]` = device MAC, `seq = ack = server seq`, bare word.
- OFFR: YACK proceeds; NACK ignored → OFFR re-sent with same seq every 1.0 s forever.
- Format: device must answer STRT (a RETR answer derails the server: header skipped, raw data streamed). Server waits forever, no retransmit.
- Chunks: one RETR each; **no second RETR after the last chunk**. 0-byte file: header only, no data frame.
- After each file an empty poll: RETR → next FILE header (or another poll if none left; server polls forever while device says RETR); WTRM → TERM immediately (remaining files skipped). **No `FILE\n` end marker.**
- No retransmission of data/empty frames: a lost reply stalls until the device re-sends its last reply (0.5 s device re-send recovers).
- *Inference (not observed):* real device answers first poll after a file with RETR and a consecutive poll with WTRM (`sim.py --after-files-reply auto` passes every run).
- Hello: ≥ 8 newline-separated fields required (6–7 → "Not enough device parameters"); field 0 must be `DSCV` (else ignored silently); arch (field 4) must match a supplied package ("Can't Netinstall <mac>, please supply the <arch> system package"); min_os > package version not enforced; license-key field unused; non-zero source IP accepted; **BOOTP not required** before Netinstall. Fields 7/8 meaning unknown (tried x/y, 0/0, 1/1, 1000/1000, 1e9/0, 0/1e9).

## 3. BOOTP / TFTP

- BOOTP: requests < 300 bytes ignored. BOOTP and DHCPDISCOVER get the same reply: plain BOOTP broadcast :67 → 255.255.255.255:68, 300 B, op 2, flags 0, ciaddr 0, yiaddr .2 (or `-a` IP), siaddr = server IP, giaddr 0, sname empty, `file=/image`, cookie then 0xff (no options). No reply if no system package matches the option-60 arch (`Mips_boot` → mips, `ARM64__boot` → arm64).
- TFTP: DATA from **port 69**; without options 1452-byte blocks; `blksize=1024` → OACK then 1024-B blocks; `tsize=0` → OACK with 7838584. OACK immediately followed by DATA 1 (no wait for ACK 0; an ACK 0 re-sends block 1). After the last short block, an extra zero-length block. Served image = embedded AArch64 ELF (offset 31457367, 7838584 B, sha256 21e70aa2…028e) → confirms the Plan 3 extraction approach.

## 4. PING/PONG

Startup `PING\n`, ~0.5 s listen; foreign `PONG\n` → exit "Concurrent netinstall servers detected (override with -c)", rc 1. Server answers its own PING. `-c` → no PING. Never used with devices.

## 5. Self-echo hazard

The raw socket sees the server's own broadcasts and treats them as device frames keyed by `[0:6]`. Device replies with `[6:12]` = ff..ff / server MAC / device MAC → phantom sessions; in x17 this corrupted the upload (wrong sha256, 13984222 B instead of 13984181). Only zeros in `[6:12]` were clean. Our server: drop frames from our own IPs; accept only known device MACs or hellos.

## 6. Differences vs our implementation (ni-proto / ni-sim / ni-bootp / ni-tftp)

1. OFFR is static `OFFR\n\n` → must be `OFFR\n<key>\n<keep>\n<brand>\n\0`.
2. FILE and TERM need a trailing `\0`.
3. Remove `Expect::SecondIs` (no double RETR after the last chunk).
4. 0-byte file: no data frame (we send one empty chunk).
5. Add `mode.scr`.
6. Replace "empty → `FILE\n` → WTRM" with "poll until WTRM" (bounded), keep `FILE\n` as fallback.
7. TERM reboot/shutdown variants.
8. ack = seq−2 not seq−1 (cosmetic; devices seem to ignore it).
9. Server `[0:6]` should echo device `[6:12]` (ff..ff for OFFR) instead of our own MAC.
10. NACK on OFFR = retry, not failure.
11. min_os: warn only.
12. Sort the system package first.
13. ni-sim: send ≥ 8 hello fields and zeros in `[6:12]`.
14. BOOTP: we send DHCP options, official none (ours is a superset); accept requests < 300 bytes.
15. TFTP: we use 512-B blocks from an ephemeral port; official 1452-B blocks from port 69. Ours is RFC-correct; switch only if hardware requires.
16. Optional: PING/PONG concurrency check.
17. Keep our same-seq retransmit (harmless if devices de-duplicate).

## 7. Recommendation

`ConfigMode { Keep, Default, Empty, Script(Bytes) }`, `mode_script: Option<Bytes>` (only when mode ≠ Keep), `remove_branding: bool`,
`license_key: Option<String>` (keyfile with `\n`/`\r` stripped), `term: Default | Reboot | Shutdown`.
OFFR: `keep` = (mode == Keep); `brand` = "removeBranding" iff !remove_branding. Files after packages: Script → autorun.scr;
Empty → autorun.scr (0 B, header only); mode.scr ordered like the official tool (after autorun with Script, before the empty autorun with Empty).

```
OFFR → YACK (NACK = retry) → "" → STRT → "" → RETR →
  per file: FILE hdr\0 → RETR → chunks → RETR each → "" poll → RETR
  "" poll → WTRM (repeat while RETR, bounded; fallback FILE\n) → TERM\n<msg>\0
```
WTRM before all files are sent → fail loudly.

## 8. Open questions (hardware)

Device answers to the post-file polls; what devices put in `[6:12]`; hello fields 7/8 and the `-f` size-check trigger;
device-side meaning of `removeBranding`; whether devices de-duplicate a re-sent same-seq frame.
Untested: `-o`, `-f`, `--mac` filtering of the hello, TFTP without BOOTP, x86/PXE/UEFI, seq wrap past 65535.

## 9. Run index

e01–e16 config-mode matrix; x01–x18 behaviour probes (dhcp, nobootp, srcip, nack, drop, tftpopts, clientip, macfilter,
twopkg, mips, fmtdelay, resend, macok, hello6, wrongarch, -m, fsrc, pkg order); y01 drop with device re-send;
y02/y03 TFTP blksize/tsize; y04 format reply RETR; c01/c02 concurrency without/with -c; hp_* hello-field probes;
probe_zero/probe_fsrc strace; t_s_retr first script-delivery run.

Raw artifacts (simulator and probe scripts, decoders, per-run transcripts, frame dumps and pcaps)
lived in a temporary session directory and on the Linux test VM; both were deleted on 2026-11-11.
This document is the retained record of the findings.

Sources: pyNetinstall upstream https://github.com/dvtirol/pynetinstall (mirror: https://github.com/rfdrake/pynetinstall).
