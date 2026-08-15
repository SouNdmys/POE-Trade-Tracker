# POE Trade Tracker

Native Windows currency-exchange tracker for Path of Exile 1 & 2. Watches the in-game
currency exchange panel, recognizes order books in tens of milliseconds, and finds
arbitrage opportunities with an exact-rational, fully auditable engine.

Successor to POE2-Trade-Tracker (Electron) and POE1-Trade-Tracker, architected on the
shipped [POE Alarm](https://github.com/SouNdmys/POE-Alarm) Rust + GPUI workspace.

- Rust, GPUI front end, no async runtime (std::mpsc actors)
- Windows.Media.Ocr primary + bundled PP-OCRv5 recognition-only ONNX fallback
- Fail-skip recognition: an uncertain frame is skipped, never guessed
- License: PolyForm Noncommercial 1.0.0

## Build

```powershell
cargo build --release
cargo test --workspace --all-targets
```

## Workspace

| Crate | Responsibility |
|---|---|
| `ptt-core` | Platform-free domain core: exact decimal, text canonicalization/matching, shared ids |
| `ptt-catalog` | Closed per-game asset catalogs and OCR lexicons (SHA-256 pinned data) |
| `ptt-vision` | Desktop capture, masks/fingerprints, row-band detection |
| `ptt-ocr-win` | Windows.Media.Ocr adapter (process-lifetime MTA actor) |
| `ptt-ocr-onnx` | Offline PP-OCRv5 recognition-only backend + CTC target support |
| `ptt-platform-win` | Hotkeys, HUD overlay, region selection |
| `ptt-settings` | Versioned, crash-safe JSON settings store |
| `ptt-runtime` | Background actor runtime (skeleton; ported in P2) |
