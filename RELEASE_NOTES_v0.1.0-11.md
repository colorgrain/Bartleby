# Bartleby v0.1.0-11 — Release Notes

This release covers all changes since **v0.1.0-10**. It is above all a
**reliability release**: a full audit of the code base found several ways a
copy or a verification could be reported as successful when it was not. They
are fixed, and the copy engine now has integration tests. It also brings a new
window look, camera colour metadata, ASC MHL v2.0 histories that travel with
the card, and Unicode text in PDF reports.

---

## ⚠ Changes in behaviour to know before updating

- **ASC MHL history now lives in the card folder.** With "copy as subfolder",
  the history is written to `{destination}/{card}/ascmhl/` (it used to be
  `{destination}/ascmhl/`), as ascmhl, Silverstack or ShotPut Pro do. If the
  card already carries an `ascmhl/` history, it is copied and continued.
- **MHL histories are append-only.** The "Existing MHL" prompt (Replace / Keep
  both / Skip) is gone: a new generation is always added, generations are
  never deleted.
- **`.xxh3` checksum files use the `XXH3_` prefix** written and expected by
  `xxhsum`. The Verifier reads both the new and the old form.
- **One job at a time.** Starting a copy (or a verification) while one is
  running is refused instead of making the first one uncontrollable.

---

## Integrity fixes

- **Copy without verification** no longer reports "✓" when files failed to
  copy.
- **Verification reads the disk, not RAM.** A read error during the
  cache-bypassing re-read (bad sector, unplugged drive) used to fall back
  silently to a cached read and could "verify" a damaged file. The fallback is
  now limited to file systems that do not support direct reads, drops the
  cache first, and is logged. On macOS, cached pages are invalidated before
  every verification read.
- **Flush errors count.** A failed `syncfs` (Linux) or `FlushFileBuffers`
  (Windows) — e.g. a USB drive dropping out during write-back — now fails the
  job, including in "size only" and "no hash" modes.
- **Size-only mode**: no more cascading false "SIZE MISMATCH" after a failed
  copy.
- **Per-destination status.** A file that failed on one destination stays in
  the checksum file, MHL and reports of the other destinations; on the failed
  one it is listed as FAILED, never with the source hash.
- **Copied size checked**: a short copy, or a source that changed during the
  copy, is an error. A failed or cancelled copy no longer leaves a partial
  file behind (an existing file the copy never touched is kept).
- **Nothing is skipped silently**: symbolic links, special files (FIFOs,
  devices) and OS system folders left out of a copy are listed in the log.
  System folder names (`.Trashes`, `RECYCLER`…) are only skipped at the source
  root. A FIFO no longer blocks the copy.
- The hash pipeline no longer reads into uninitialised memory and treats an
  early stop of the reader as an error.

## Data-loss and security guards

- A destination equal to the source, inside it, or listed twice is refused
  before anything is written (copying a card "as subfolder" into its own
  parent folder used to truncate the source files).
- "Skip and keep" never overwrites the existing file, even when it cannot be
  renamed.
- A crafted file extension could execute code while building the PDF/HTML
  file icons. Fixed.
- A checksum file or MHL can no longer make the Verifier read files outside
  the checked folder.
- CSV values starting with `=`, `+`, `-` or `@` are neutralised so a crafted
  file name is not run as a spreadsheet formula.

## ASC MHL v2.0

- `ascmhl_chain.xml` is written and kept up to date (C4 of every generation).
- Files already recorded by an earlier generation are marked `verified` (or
  `failed` if their hash changed), new ones `original`.
- The `ascmhl/` folder is no longer listed as media in the MHL or the checksum
  file.
- Output checked with the reference `ascmhl` tool: schema validation, history
  read and extended by `ascmhl create`, and tamper detection for MD5, SHA-1,
  XXH64, XXH3, XXH128 and C4.

## Verifier

- Re-hashes with the same cache-bypassing reads as the copy engine, and can be
  paused or cancelled inside a large file.
- Reads checksum files with a UTF-8 BOM, names with double or trailing spaces,
  `md5sum` escaped lines and the BSD format (`MD5 (file) = hash`).
- A checksum file or MHL with no usable entry is an error, not "0 failures".
- The post-verification MHL continues the same history and chain file.

## Reports

- **Unicode PDF**: file and folder names in Polish, Czech, Greek, Cyrillic,
  Vietnamese… are printed (embedded Noto Sans). Characters it lacks (Japanese,
  Chinese, Korean, Arabic…) use a TrueType font installed on the system, or
  appear as `?` — they are never dropped. Arabic and Hebrew are shown without
  joining or right-to-left layout.
- **PDF layout**: hashes are always printed in full (on two lines if needed —
  C4 IDs were cut); names no longer overflow into the next column; column
  headers no longer overlap.
- Each PDF / HTML report is generated **once per job** and copied to the other
  destinations when identical.
- Checksum file and MHL are written **before** the PDF/HTML, so a slow or
  failing report cannot prevent them.
- ffmpeg, mediainfo and the icon helper are stopped if they hang (a corrupt
  file could freeze the job at "Generating reports…").
- Lighter HTML reports (smaller embedded thumbnails and logo).
- CSV opens correctly in Excel (UTF-8 BOM) and is fully escaped.

## Camera metadata

- Gamut / transfer read from MXF picture descriptors (ARRI, Canon, Sony),
  the Sony XML sidecar (now for XAVC S / HS **MP4** clips too) and the RED R3D
  header. Only values present in the files are shown.
- Images: the Gamut column shows the colour primaries (it showed "YUV"/"RGB").
- Image sequences (DPX, EXR, ARRIRAW): metadata read once per sequence instead
  of once per frame; frames get an icon instead of a thumbnail each.

## Copy

- Copied folders keep the modification date of their source folder.
- Destination folder names built from templates are cleaned of characters
  exFAT / NTFS refuse (`: ? * " < > |`); `.` and `..` levels are refused.
- Template variables may contain digits, `_` and `-` (`%cam_a`, `%cam2`);
  `%j` (day of year) is no longer off by one around DST changes.

## Interface

- New drawn window chrome with four styles (Adwaita, Breeze, macOS, Windows)
  and custom colour themes.
- Drag & drop goes to the window it is dropped on (a drop on the Verifier used
  to edit a job too, and vice versa).
- The job list is locked while a copy runs.
- Every job has a remove (×) button, also when it is the only one (it is then
  cleared); a configured job asks for confirmation.
- Cancel also works while a dialog is waiting for an answer; a double click on
  a dialog no longer freezes the window.
- The log panel stays fast on jobs of thousands of files.
- The hash algorithm choice is remembered.
- Settings: an unreadable `settings.json` is kept as
  `settings.json.corrupt-<date>` instead of being overwritten; save errors are
  shown. `bartleby.log` is now actually written.

## Platforms and packaging

- **macOS: universal build** (Apple Silicon and Intel).
- Bundled **FFmpeg 9.0.2** and **MediaInfo CLI 26.10**; the downloaded
  sidecars are checked against SHA-256 sums kept in the repository.
- Arch Linux package: `libnotify` and `python-gobject` listed as optional
  dependencies.
- AppImage: the app icon (`.DirIcon`) was a broken absolute link, so launchers
  and AppImage managers showed no icon; the desktop entry now has a category
  (Audio/Video › Video) and an up-to-date description.

---

## Supported hash algorithms

| Algorithm | Output file | Notes |
|-----------|-------------|-------|
| None | — | Copy only, no checksum |
| Size only | — | Size comparison only |
| MD5 | `.md5` | Compatible with `md5sum -c` |
| SHA-1 | `.sha1` | Compatible with `sha1sum -c` |
| XXH64 | `.xxh64` | Compatible with `xxhsum -c` |
| XXH3-64 | `.xxh3` | Compatible with `xxhsum -c` (`XXH3_` prefix) |
| XXH128 | `.xxh128` | Compatible with `xxhsum -c` |
| C4 ID | `.c4` | Content-addressable identifier |

MHL generation is available for all algorithms except None and Size.

---

## Installing

Download the installer for your platform from the [Releases](../../releases) page.

**Linux**: `.deb` (Debian/Ubuntu), `.AppImage`, or the Arch package
(`.pkg.tar.zst`)

```bash
sudo dpkg -i bartleby_*.deb
# or
chmod +x Bartleby_*.AppImage && ./Bartleby_*.AppImage
# or (Arch)
sudo pacman -U Bartleby_*_x86_64.pkg.tar.zst
```

**macOS**: `.dmg` (universal) — drag to `/Applications`, right-click → Open on
first launch to bypass Gatekeeper.

**Windows**: `.msi` installer — run and follow the prompts.

mediainfo and ffmpeg are bundled in all installers. No separate installation required.

---

> **Beta software** — Bartleby is under active development. Back up your data independently of any copy tool.
