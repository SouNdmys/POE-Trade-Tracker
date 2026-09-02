# POE Trade Tracker

POE Trade Tracker is a Windows desktop program that reads the in-game currency exchange panel
off your screen while you flip through pairs, and keeps every order book it reads. There is no
game API and no price site behind it: what it knows is what you have looked at. From that pile
of books it works out what each currency is worth against a settlement currency you choose,
which currencies are scarce and which are oversupplied, what multi-hop routes exist between the
pairs you have actually visited, and what each route returns if you come back the other way. It
shows that across eight pages.

It does not tell you what to trade. The rule written at the top of
[docs/CORE-TRADING-MODEL.md](docs/CORE-TRADING-MODEL.md) is that a number you can display and
let the person judge beats a threshold tuned to judge for them, and the rest of the program
answers to that. It is a tool for making your own judgement faster.

Three limits worth knowing before you read further:

- **Windows only**, x64. Capture, hotkeys, the overlay, OCR, the updater and the settings store
  are all `cfg(windows)`-gated.
- **You have to calibrate it first**, against a screenshot of your own client. The shipped
  region presets are drawn for 2560×1440 windowed fullscreen; at any other resolution they will
  not line up and framing the three regions yourself is not optional.
- **It only knows the pairs you flipped past.** Nothing runs while you are elsewhere, nothing
  is fetched, and putting a currency on the watchlist does not make the program go get data —
  it only means coverage keeps measuring it.

Licensed under PolyForm Noncommercial 1.0.0: source-available, free for personal use,
commercial use not granted. See [Licence](#licence).

<!--
SCREENSHOT — drop the file at docs/screenshots/radar.png and reference it here as
![The radar page](docs/screenshots/radar.png)

Nothing is committed yet, so there is no image tag above on purpose; a broken one is worse
than none. What the shot should show: the Radar page at the default 1180x640 window in the
dark theme, with a populated route table on the left and one route selected so the detail
panel on the right shows its per-leg books. Radar is the page that makes the point in one
picture — it is the only page that answers before you ask. Crop to the window; no desktop,
no taskbar. A second shot of the Monitor page (health band + last book read) is worth adding
under Requirements if you want two.

docs/ is not gitignored except for docs/poe2-ui/ and docs/*.zip, so a PNG under
docs/screenshots/ commits normally.
-->

## Requirements

**Windows, x64.** There is no Linux or macOS build — screen capture, the global hotkeys, the
overlay and the OCR engine are all Win32/WinRT.

**Windows OCR language packs.** Recognition uses `Windows.Media.Ocr` as its primary engine, so
the recognizers have to be installed on the machine:

- An **English** recognizer is required by every profile, including the Chinese ones — the rate
  and stock lanes are pinned to English because ratios and stock are Arabic numerals in every
  client. Windows 11 ships `en-US`, but the code does not assume it; check rather than assume.
- The 繁體中文 profiles additionally need a **Traditional Chinese** recognizer. It reads the
  panel-title strip *and* the two currency-name slots, and the title strip is attested first —
  so a machine without it does not read the names badly, it reads nothing at all.
  Only `zh-Hant-TW`, `zh-TW`, `zh-Hant-HK`, `zh-HK`, `zh-Hant-MO` and `zh-MO` satisfy it.
  **Simplified Chinese does not** — `zh-Hans-CN` scores zero against that preference.

The bundled PP-OCRv5 backend does **not** cover a missing recognizer. It is a fallback for a
name that Windows OCR read but the catalogue could not resolve; a missing recognizer fails one
gate earlier, before the fallback is ever consulted.

Engines are built lazily on first recognition, so a missing recognizer shows up as frames
failing once you start watching, not as an error at launch. Every frame is skipped with the
reason `OCR unavailable` / `OCR 不可用`, which does not name the language it wanted.

Adding a **display** language is not the same as adding the OCR feature. Under Settings → Time
& language → Language & region, "Optical character recognition" is a separately tickable
optional language feature, so the recognizer can be installed without Windows itself switching
language. The in-app guide (Settings → How to use) carries the full click path, including the PowerShell commands. To check what is
installed, from **Windows PowerShell 5.1** (`powershell.exe`, not `pwsh`):

```powershell
[Windows.Media.Ocr.OcrEngine,Windows.Media,ContentType=WindowsRuntime] | Out-Null
[Windows.Media.Ocr.OcrEngine]::AvailableRecognizerLanguages | Select LanguageTag, DisplayName
```

**Display.** Capture is a GDI `BitBlt` of a rectangle you calibrated, and the calibration
corpus was taken in **windowed fullscreen at 2560×1440**. Exclusive fullscreen was never
verified and is not claimed to work. The exchange panel must be in its centred default
position: opening the stash or character panel shifts it sideways, and a shifted panel fails
the identity gates and is skipped.

**Games and clients.** Four selectable profiles — PoE 1 and PoE 2, each in English or
Traditional Chinese. One recognition route serves both games; a game is a layout value, not a
second pipeline. How much real-frame evidence stands behind each profile differs a lot:

| Profile | Annotated frames | |
|---|---|---|
| PoE 2 · 繁體中文 | 51 | The calibrated reference. [docs/P1-CALIBRATION-NOTES.md](docs/P1-CALIBRATION-NOTES.md) documents it end to end: text polarity, geometry, layout constants, popup position modes. |
| PoE 1 · 繁體中文 | 26 | Real exchange-panel frames. The +14px Traditional-Chinese table offset was measured against them, not guessed. |
| PoE 1 · English | 12 | Ground truth from a hand-annotated fixture store — a human marked every field on ten real screenshots. |
| PoE 2 · English | 6 | The smallest corpus of the four. |

All four corpora are 2560×1440. They are driven by the `book-probe --manifest` binary against
`tests/manifests/`, not by `cargo test`.

The presets are a starting point, not a requirement. Any resolution works if you draw your own
three rectangles on the Calibrate page — a user-drawn region is used exactly as drawn, with no
language offset applied, because it was already drawn on their own client.

## Install

There is no installer, no elevation prompt, and no registry writes.

1. Download the release zip from the Releases page.
2. **Create a folder and extract into it.** The zip carries no folder of its own: nine files
   arrive as four loose ones plus `assets\` and `licenses\`, so extracting into Downloads
   scatters them.
3. Run `ptt-app.exe`. The window opens at 1180×640.

**Do not extract into `C:\Program Files`.** There is no installer and the program has no way to
elevate itself, so every write there fails. The updater probes the folder for writability
before it touches anything and refuses with "this folder is not writable — move the program out
of Program Files and try again"; that refusal is the good outcome, the bad one would be a
half-swapped folder.

The binaries are not code-signed, so a downloaded build will get a SmartScreen prompt the first
time you run it.

## First run

This is the part people get wrong. Nothing is read until the three regions are framed.

1. **In Settings → Basics, pick your game and client language.** The reader matches panel text
   against that profile's wordlist, and the regions you are about to frame are stored under it
   too — the wrong profile reads nothing and never says why. The default is PoE 2 with a
   Traditional Chinese client.
2. **Open the currency exchange in game and take a screenshot** — a normal one, saved as PNG or
   JPEG. The program does not grab the screen for you; the Calibrate page opens a file picker
   for a still you supply. That is deliberate: the panel only exists while the game has focus,
   and a live fullscreen overlay made people draw rectangles that looked right and were not.
3. **On the Calibrate page, load that screenshot and frame three areas on it:** the "I need"
   name slot, the "I have" name slot, and the two order tables. Text only — leave the icons
   out, they degrade OCR. The page reads "2 / 3 regions framed" until all three are done.
4. **Press "save these three".** Framing without saving changes nothing. A watch that is
   already running restarts to pick up the new geometry.
5. **Press start watch**, or hit `Ctrl+Alt+F10` without leaving the game.
6. **In game, flip through the pairs you care about.** A panel is read once it stops moving, so
   give each one a second.
7. **Come back to the app.** The watchlist and the radar only know the pairs you flipped past.

Two behaviours that look like faults and are not: a panel that has not changed is read once and
then left alone, and a panel that is not where you framed it is skipped rather than guessed at.
Both are the gates working. The in-app guide (Settings → How to use) carries this same list plus
a short troubleshooting section, in English and Chinese.

**Hotkeys.** Two are registered, system-wide, so they work with the game in front:

| | |
|---|---|
| `Ctrl+Alt+F10` | start / stop watching |
| `Alt+F11` | show / hide the overlay |

They cannot be changed from the interface — you edit `settings.json` and restart — and only
three fixed combinations per action are accepted, because a free-text binding lets a settings
file ask for a combination that cannot be registered, leaving you with a key that silently does
nothing. Anything unrecognised is normalised to the default and written back to the file.
Settings → Basics prints whether the watch key actually registered, so a combination another
program already owns is visible rather than dead.

## What each page answers

The nav order is "what do I look at first each day", not a menu tree.

| Page | |
|---|---|
| **Monitor** | Is the watcher alive, what did it just read, and what that book is worth. |
| **Analytics** | What each currency is worth, who is buying it, and whether the settlement anchor itself has drifted. |
| **Watchlist** | What is being watched, whether it is healthy, and what to capture next. |
| **Radar** | Every route the captured books already imply, ranked — the page that answers before you ask. |
| **Convert** | I hold this and want that: what a route returns, and whether to take the fill or list against it. |
| **History** | What one pair has been doing, as a summary, a chart, and a note on what looks off. |
| **Calibrate** | Where the three regions sit on your screen, drawn on a screenshot. |
| **Settings** | Game and language, the overlay, season and storage, the algorithm numbers, a usage guide, and About. |

A few things those one-liners do not fit:

- **Arbitrage detection always runs on instant, immediately-fillable prices.** Profitability
  there is a *sufficient* condition: swapping the legs where you are the maker to a listing one
  tick under the competing front can only raise the profit, never make the opportunity vanish.
  The three tiers are instant fill, opportunity, and greedy — and greedy is labelled a bet on
  drift, not a better price for the same trade.
- **Radar ranks by round-trip return**, not by "better than direct", and does not assume a
  stake. Quantity lives in the detail panel's what-if box.
- **On Convert the profit percentage is a function of rate alone** and does not move when you
  change the amount you type; a regression test holds that. Routes whose *rate* is worse than
  going direct are hidden. Routes whose rate is better but whose depth is thin are always shown
  with a note — rank orders the list, it never hides a row, because the program has no data on
  what a currency is strategically worth to you.
- **Rates are exact rationals end to end.** The only places an exact rate becomes a floating
  point number are the two chart-plotting closures — the History line and the Analytics
  sparkline — because a pixel cannot hold a fraction, and nothing derived from either is
  allowed back into a model.
- **Seasons are always a manual action.** The program never infers a rollover, because a
  misfire would silently erase history. Rolling over archives by clamping — old rows stay on
  disk, outside every window — until an explicit two-click purge removes them.
- **The interface is fully bilingual**, English and Chinese, with compile-time enforcement: a
  new enum variant that has not been named in both languages does not build.
- **Dark and light palettes**, chosen in Settings and remembered. Dark is the default and the
  design baseline. It deliberately does not follow the OS day/night setting: the window sits
  beside a game, so brightness should follow the game, not the desktop.

## Updates

On every launch the program asks GitHub Releases once whether there is a newer version. That is
the whole automatic part — **nothing downloads and nothing installs without you pressing a
button.** The check is anonymous: no token, no account, no machine id, and no request body.
What GitHub sees is what it sees for any anonymous download — an address, and a `User-Agent`
naming the program and its version. There is no setting to turn it off.

The About segment has a manual "Check now" with a 60-second cooldown, because GitHub allows 60
anonymous calls per IP per hour and an unthrottled button would burn the hour in a minute.

If you press install, the package is downloaded, every file in it is checked against the
`MANIFEST.json` the package carries, all the new files are written beside their destinations
first, and only once every one has landed does the swap begin. A failed check deletes the
download rather than leaving tens of megabytes behind. `ptt-app.exe` and `onnxruntime.dll`
cannot be deleted while running, so they are renamed aside to `.old` and swept on the next
launch.

**What that verification proves, exactly:** the manifest travels inside the zip it describes.
Matching hashes prove the package arrived intact — not that it came from the author. Proving
that would need offline public-key signing, which this does not have and does not pretend to
have.

Afterwards you have to restart the program yourself. Starting a new watch is deliberately
blocked until you do: the new native recognizer is already on disk while the old executable is
still running, and loading it into the old process would end the process with no message that
connects to the update.

## Where your data lives

Everything is under `%LOCALAPPDATA%\PoeTradeTracker\`:

| | |
|---|---|
| `settings.json` | settings, calibration rectangles, hotkeys, per-game tuning |
| `market.sqlite` | every capture, rollup and season — one file, both games |
| `updates\pending-update.zip` | a downloaded update, deleted on failure |

No server, no account, no sync, no sharing, no telemetry. Your market data never leaves the
machine.

Settings are read leniently — missing, unreadable or malformed all yield defaults rather than an
error — and written atomically (temp file, fsync, rename). A file written by a newer schema puts
the store into read-only mode instead of being clobbered.

Nothing is pruned by default: raw retention is 0 days, which means keep everything. Settings →
Season & storage shows the database size and offers "clear pre-season raw data" and "compact
database" as explicit actions. Compacting is disabled while a watch runs, because rewriting the
file blocks the capture writer.

Uninstalling is deleting the program folder and deleting `%LOCALAPPDATA%\PoeTradeTracker\`.

## What this does to your game client

It reads pixels. That is the entire interaction.

- It copies a rectangle of the desktop — the one you calibrated — with GDI `BitBlt`. Not the
  whole screen, and not "the game window": it has no idea which window it is looking at, it
  reads screen coordinates.
- It never finds, opens or reads the game process. There is no `FindWindow`,
  `GetForegroundWindow`, `EnumWindows`, `OpenProcess`, `ReadProcessMemory`,
  `WriteProcessMemory`, or `CreateRemoteThread` anywhere in the workspace.
- It never reads the game's memory, its files, or its log.
- It injects no input. No `SendInput`, no `keybd_event`, no `mouse_event`, no `SetCursorPos`, no
  `PostMessage`/`SendMessage` to any window. Nothing is clicked or typed for you; you place
  every order yourself.
- There is no keyboard hook of any kind. It registers exactly two global hotkeys through
  `RegisterHotKey`.
- One thing worth getting ahead of, because someone will find the code: a low-level mouse hook
  (`WH_MOUSE_LL`) exists in `ptt-platform-win`, inherited from POE Alarm. **The application
  never installs it** — its only callers are the platform self-test and that test's integration
  harness. In normal operation no hook is active and no mouse event is ever swallowed.
- The overlay is a topmost layered tool window that is click-through by default, so it cannot
  take a click away from the game. It becomes clickable only while you are dragging it into
  place. It is excluded from capture only when it geometrically overlaps a calibrated region,
  so the tool does not read its own overlay back as game text; a HUD parked elsewhere stays
  screenshottable.
- The only network traffic is the updater: a GET to
  `https://api.github.com/repos/SouNdmys/POE-Trade-Tracker/releases/latest` once per launch,
  and — only if you press install — a GET of that release's download URL. There is no POST or
  PUT anywhere in the workspace, no telemetry and no crash reporting.
- The only places it writes are `%LOCALAPPDATA%\PoeTradeTracker\` and — when you press install
  on an update — its own program folder, which is what installing an update means. No registry
  writes, no system settings, no autostart entry, no clipboard access, and it spawns no
  processes.

Whether reading your own screen is acceptable under the game's terms is your call to make; the
program takes no position on it and neither does this document.

## Building from source

**Toolchain:** Rust 1.88 or newer, MSVC, edition 2024. There is no `rust-toolchain.toml`, so
your default toolchain is what gets used. The Windows SDK's `rc.exe` compiles the app icon and
version resource into the exe; it is optional — without it the build still succeeds and the exe
just falls back to the system default icon, with a cargo warning saying so.

**The one step cargo cannot do for you.** `ptt-ocr-onnx` builds `ort` with `load-dynamic` and
default features off, so nothing downloads and nothing links the ONNX runtime; it is resolved
at runtime, and `onnxruntime.dll` is deliberately not committed. Fetch it:

```powershell
git clone https://github.com/SouNdmys/POE-Trade-Tracker
cd POE-Trade-Tracker
.\packaging\fetch-onnxruntime.ps1 -Configuration debug   # for cargo run
cargo run -p ptt-app
```

Both scripts run from a PowerShell prompt at the repository root, and both work under
`powershell -File` and `pwsh -File` alike.

The script downloads ONNX Runtime 1.28.0 (win-x64, ~79 MB), verifies the extracted DLL against
a pinned SHA-256 **in a scratch directory** before copying it anywhere the program would load
it — a substituted native library would be a code-execution surface — and then places it in
`target\<configuration>\`. It is idempotent: a correct copy already in place is left alone.
`-Force` re-fetches.

**Skipping that step does not break the build, which is the trap.** `cargo build` succeeds,
the app runs, and recognition quietly loses the PP-OCRv5 fallback for currency names. The only
signal is a stderr warning, and the app is a GUI-subsystem process with no console. You just
see fewer currencies recognised. Packaging does fail loudly, but only at the very end and only
with "payload file onnxruntime.dll not found".

The PP-OCRv5 model and dictionary themselves (16.6 MB) **are** committed, under `assets/ocr/`.
Only the runtime DLL is fetched.

**The two baselines.** Both must exit 0, and clippy must be silent — the repository has no
`[lints]` table, so plain clippy is the bar:

```powershell
cargo test --workspace
cargo clippy --workspace --all-targets
```

Both run from a clean clone. The single test that needs a private screenshot corpus is
`#[ignore]`d behind an environment variable. There is no CI in this repository; the baselines
are run locally.

**Packaging.**

```powershell
.\packaging\fetch-onnxruntime.ps1     # -> target\release\onnxruntime.dll
.\packaging\package-preview.ps1       # build, stage, verify, zip
```

`package-preview.ps1` runs `cargo build --release --locked -p ptt-app`, then stages an explicit
allow-list of files rather than the contents of `target\release` — which also holds about
fifteen probe binaries that must not ship. It re-reads the expected OCR asset hashes straight
out of `crates/ptt-ocr-onnx/src/assets.rs` instead of keeping its own copy, so the package is
checked against the program's own expectations, and it asserts the layout the program actually
resolves at runtime: `assets/ocr/` beside the exe, `onnxruntime.dll` beside the exe. The staged,
uncompressed payload has a 55 MiB budget. Output lands in `target\package\`.

Bumping the ONNX runtime pin is a calibration event, not a version-string edit: the OCR
fallback was calibrated against that build.

**Environment variables.** `PTT_ONNXRUNTIME_DLL` overrides where the runtime is loaded from;
`PTT_DEBUG_OCR`, `PTT_DEBUG_GRID`, `PTT_DEBUG_COMPARATOR` and `PTT_OCR_SCALE` are diagnostics.

**The crates**, bottom up:

| | |
|---|---|
| `ptt-trade-domain` | Market identity, ratios, quotes and quote edges — the bottom layer everything is built on |
| `ptt-trade-engine` | Exact multi-tier fills, bounded conversion, cycle analysis |
| `ptt-market-book` | Picking one self-consistent "current book" out of the observations |
| `ptt-strategy` | Execution safety, route accounting, listing strategy, market policy, valuation and price history, market pulse |
| `ptt-workflows` | Focus groups, coverage gaps, the probe queue, the opportunity radar |
| `ptt-storage` | SQLite persistence |
| `ptt-settings` | Versioned, crash-safe JSON settings |
| `ptt-catalog` | Closed per-game asset catalogs and OCR lexicons |
| `ptt-core` | Affix matching and the compound rule engine |
| `ptt-vision` | Desktop capture and the blue-text vision hot path |
| `ptt-ocr-onnx` | Offline PP-OCRv5 recognition backend |
| `ptt-ocr-win` | `Windows.Media.Ocr` adapter |
| `ptt-recognition` | Per-profile order-book field recognition routing |
| `ptt-platform-win` | Isolated Win32 platform services |
| `ptt-monitoring` | The auto-watch loop: fingerprint gate, stability, double-read confirmation, de-duplication |
| `ptt-runtime` | Page models and reports, the collection pipeline, the daily rollup, and the `*_probe` verification binaries |
| `ptt-app` | The GPUI desktop app |

Read [docs/CORE-TRADING-MODEL.md](docs/CORE-TRADING-MODEL.md) before changing anything that
touches trading logic. It is the development spine — the three tiers, why arbitrage detection
runs on instant prices, how the panel denominates stock — and every other design decision
answers to it. It is written in Chinese, as are the other working documents in `docs/`.

## Licence

**PolyForm Noncommercial License 1.0.0.** Full text in [LICENSE.md](LICENSE.md); SPDX
identifier `PolyForm-Noncommercial-1.0.0`. Copyright SouNd <soundmys1994@gmail.com>.

In plain terms:

- You may **use** it for any noncommercial purpose. The licence names personal study, hobby
  projects, private entertainment, and research or testing without any anticipated commercial
  application as permitted, and extends the same permission to charities, schools, public
  research bodies and government institutions regardless of funding source.
- You may **redistribute** it, and you may **modify it and build on it**.
- If you pass any part of it on, you must pass on these terms — or the URL
  <https://polyformproject.org/licenses/noncommercial/1.0.0> — along with it.
- **Commercial use is not granted.** Every permission in the licence is conditioned on a
  permitted purpose, and only noncommercial purposes qualify. Selling it, bundling it into a
  paid product or service, or using it in the course of a commercial business are outside the
  grant.
- No sublicensing and no transferring your licence. A first violation can be cured within 32
  days of written notice; after that the permissions end.
- No warranty and no liability, as far as the law allows.

This is a **source-available** licence, not an OSI-approved open-source one: restricting the
permitted field of use to noncommercial purposes is exactly what the Open Source Definition's
no-discrimination-against-fields-of-endeavor clause rules out. The distinction is worth stating
plainly rather than letting "the source is on GitHub" imply the other thing.

## Acknowledgements

This is an unofficial tool. It is not affiliated with, endorsed by, or associated with Grinding
Gear Games. Path of Exile and Path of Exile 2 are trademarks of Grinding Gear Games, and the
in-game item names this program matches against are theirs.

Where the data came from:

- The **PoE 2 catalogue** (660 currency-exchange assets) was transcribed from **poe2db**, a
  third-party fan database. The **PoE 1 catalogue** (1,047 assets) was transcribed from
  in-game selector screenshots in both languages. Each entry carries four fields — id,
  Traditional Chinese name, English name, aliases — and nothing else: no icons, no artwork, no
  item descriptions, no game mechanics. Anything describing what a currency *does* in game
  would be weight in every build and an invitation to make decisions from it. No game art,
  screenshots or client assets are committed to this repository.

Third-party software:

- **ONNX Runtime** (MIT, Microsoft Corporation) — the native inference runtime, pinned to
  1.28.0, fetched by script and redistributed inside the release zip.
- **PP-OCRv5 / PaddleOCR** (Apache-2.0, PaddlePaddle Authors) — the offline recognition model
  and its dictionary.
- **gpui** and **gpui-component** (both Apache-2.0) — the user interface.
- **Lucide** (ISC) — the two embedded SVG icons match Lucide's `check` and `chevron-down`.
- **SQLite** (public domain) — compiled into the executable via `rusqlite`'s bundled feature.

Every one of the 499 packages that link into the Windows build carries a permissive licence;
there is no GPL, LGPL, MPL, CDDL or EPL code in the shipped binary. The release zip ships
`LICENSE.md` plus notices for ONNX Runtime and PaddlePaddle under `licenses/`.

This program is the successor to POE2-Trade-Tracker (Electron) and POE1-Trade-Tracker, built on
the [POE Alarm](https://github.com/SouNdmys/POE-Alarm) Rust + GPUI workspace.
