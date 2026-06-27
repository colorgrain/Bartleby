# Bartleby v0.1.0-10 — Release Notes

This release covers all changes since **v0.1.0-9**. It focuses on transfer
visibility — per-disk speed and a smarter ETA — a new mounted-volume explorer,
and a clearer job-queue status display.

---

## What's new since v0.1.0-9

### Per-disk transfer speed

The progress ticker now reports speed **per medium** rather than a single
aggregate figure. Each device involved in a transfer — the source and every
destination — has its own cumulative byte counter, so you can see at a glance
which disk is the bottleneck when copying to several destinations at once.

### Phase-aware ETA

The remaining-time estimate is now computed in the Rust engine using a
**slowest-medium model** that is aware of the current phase:

- During **copy**, the ETA is governed by the slowest destination's write speed.
- During **verify**, it is governed by the slowest medium's read speed (source
  and destinations are all re-read).

The estimate is hidden during a short warm-up (until enough real throughput data
is gathered) and during the scan, done, and report phases, so the number you see
is one the engine can actually stand behind.

### Mounted-volume explorer

A new explorer panel lists the **mounted volumes** currently available on the
machine, with a manual **Refresh** button. The list also refreshes automatically
when a volume is plugged in or unplugged, so removable media appears (and
disappears) without a restart.

### DaVinci-style job-queue status

Each job in the queue now carries a clear status **badge** — idle, running,
done, or failed — modelled on the queue display in DaVinci Resolve. Right-click
a job to reset its status (for example, to re-run a job that has already
completed).

### Quit guard during a copy

Closing the window while a copy is in progress now prompts for confirmation
instead of tearing the transfer down silently.

---

## Bug fixes

- **Job queue**: the Done/failed status badge is now positioned snug against the
  job's remove (×) button when more than one job is queued. Previously the badge
  and the button each claimed the free space in the header row, leaving an
  awkward gap between them.

---

## Supported hash algorithms

| Algorithm | Output file | Notes |
|-----------|-------------|-------|
| None | — | Copy only, no checksum |
| Size only | — | Size comparison only |
| MD5 | `.md5` | Compatible with `md5sum -c` |
| SHA-1 | `.sha1` | Compatible with `sha1sum -c` |
| XXH64 | `.xxh64` | Via `xxhsum` |
| XXH3-64 | `.xxh3` | Via `xxhsum` |
| XXH128 | `.xxh128` | Via `xxhsum` |
| C4 ID | `.c4` | Content-addressable identifier |

MHL generation is available for all algorithms except None and Size.

---

## Installing

Download the installer for your platform from the [Releases](../../releases) page.

**Linux**: `.deb` (Debian/Ubuntu) or `.AppImage`

```bash
sudo dpkg -i bartleby_*.deb
# or
chmod +x Bartleby_*.AppImage && ./Bartleby_*.AppImage
```

**macOS**: `.dmg` — drag to `/Applications`, right-click → Open on first launch to bypass Gatekeeper.

**Windows**: `.msi` installer — run and follow the prompts.

mediainfo and ffmpeg are bundled in all installers. No separate installation required.

---

> **Beta software** — Bartleby is under active development. Back up your data independently of any copy tool.
