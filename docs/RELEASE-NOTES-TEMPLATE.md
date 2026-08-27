# Release notes template

Copy the block below into the GitHub release body and fill it in. English, because the release
page is the one thing in this project a stranger reads.

Four sections, and they are all load-bearing:

- **What changed** — so someone can decide whether to update.
- **If you had this configured** — the updater replaces the program's own files and leaves
  `settings.json` and `market.sqlite` alone. Anything a user calibrated, tuned or picked
  survives an update, so a change that alters what those settings *mean* has to be said out
  loud or it will land silently.
- **Checksum** — `MANIFEST.json` covers the files *inside* the zip, not the zip. Publishing the
  zip's own hash here is the only integrity anchor a downloader gets that does not travel
  inside the package it is meant to check. It still proves nothing about who built it; there is
  no signing.
- **Install** — there is no installer, and Program Files is the mistake people actually make.

Get the hash after packaging:

```powershell
Get-FileHash target\package\poe-trade-tracker-<version>-preview.zip -Algorithm SHA256
```

---

## Template

```markdown
## What changed

- 
- 

## If you had this configured

- 

(Delete this section if nothing you had set behaves differently. Say "nothing" rather than
leaving it out if a reader might expect something here.)

## Download

`poe-trade-tracker-<version>-preview.zip`

SHA-256: `<hash>`

Verify with `Get-FileHash <file> -Algorithm SHA256`.

## Install

Create a folder, extract the zip into it, run `ptt-app.exe`. The zip carries no folder of its
own — nine files arrive as four loose ones plus `assets\` and `licenses\` — so extracting
straight into Downloads scatters them.

Do not extract into `C:\Program Files`. There is no installer and the program cannot elevate
itself, so the updater will refuse to write there.

Updating from inside the app: press check, then install, then close and reopen the program.
Your settings, calibration and database are untouched.
```

---

## Worked example

This is a hypothetical 0.2.0, filled in so the shape is obvious. Nothing below describes a real
release.

```markdown
## What changed

- Radar detail panel: the what-if amount box now re-prices every leg, not just the first one.
- Convert: routes with thin depth carry the liquidity note in the list as well as the detail,
  so you can see it without selecting the row.
- Recognition: rows where the rate contradicts its neighbours are skipped with their own
  reason instead of being bucketed under "row grammar", so the monitor's skip list says which
  one it was.
- Fixed: the season banner counted a season that had ended as still running.

## If you had this configured

- The scarcity ratio default moved from 3.0x to 2.0x. If you never changed it, the Analytics
  scarce/oversupplied split will classify more currencies as scarce than it did before. Your
  value is untouched if you did change it — Settings → Algorithm → Liquidity & outliers.
- Calibration, settings and the database carry over. No re-framing needed.

## Download

`poe-trade-tracker-0.2.0-preview.zip`

SHA-256: `9f2c41b8e7a30d5c6148ba92f0e7d3115ca8b4d2769e0f3a8c51d47be2069fd4`

Verify with `Get-FileHash <file> -Algorithm SHA256`.

## Install

Create a folder, extract the zip into it, run `ptt-app.exe`. The zip carries no folder of its
own — nine files arrive as four loose ones plus `assets\` and `licenses\` — so extracting
straight into Downloads scatters them.

Do not extract into `C:\Program Files`. There is no installer and the program cannot elevate
itself, so the updater will refuse to write there.

Updating from inside the app: press check, then install, then close and reopen the program.
Your settings, calibration and database are untouched.
```
