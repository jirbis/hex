# Hex on Intel macOS (experimental fork build)

This fork has an **experimental**, independently packaged Intel macOS build for the
MacBook Pro 16-inch (2019), Intel Core i9 and AMD Radeon Pro 5500M 8 GB.
It is **not** an upstream release. CPU dictation was tested on the actual
Radeon Pro 5500M MacBook (macOS 15.1); an explicit Metal ASR override was
incorrect. See [upstream Intel issue](https://github.com/anomalyco/hex/issues/953).

## Get the Intel application

Open [Intel macOS build actions](https://github.com/jirbis/hex/actions/workflows/intel-macos.yml).
Choose a successful run on `feature/intel-macos-hex` (or `dev` after merge),
and download its
`hex-intel-macos-x86_64` artifact. Extract `hex-intel-macos-x86_64.zip`
and drag **Hex Test.app** to Applications. The app uses the separate bundle ID
`anomaly.hex.test` and does **not** check for upstream updates.

The bundle is ad-hoc signed and not notarized by Apple. Only install it if
you trust the fork and its workflow; macOS may require manually allowing the
app through Privacy & Security. Do not disable Gatekeeper globally.

The GitHub Actions runner validates the x86_64 binary and code signature.
This is **not** proof of a functioning hotkey, microphone permissions, GPU
inference, or dictation on your actual laptop.

## Build yourself on an Intel Mac

- macOS Sequoia 15.x for running the app (physically tested on 15.1);
  building with Xcode 26.3 requires macOS 15.6+
- Xcode 26.3, Command Line Tools selected with `xcode-select`
- Rust stable and Cargo
- Around 20 GB of free disk space for source/toolchains/build outputs

```sh
git clone https://github.com/jirbis/hex.git
cd hex
git checkout feature/intel-macos-hex
xcodebuild -version
rustup default stable
xcrun --sdk macosx metal --version || xcodebuild -downloadComponent MetalToolchain
HEX_TEST_BUILD=1 scripts/build-app.sh
# App: target/app/Hex Test.app
```

## CPU vs AMD GPU

On **Intel macOS only**, the Parakeet/transcribe.cpp model loader defaults to
**CPU** to avoid a slow/incompatible Metal device. The rendering engine is
separately pinned to **Metal** in the packaged app via
`LSEnvironment.RUNE_GPU_BACKEND=metal`, because the default Rune/wgpu
fallback has crashed on older Intel Macs.

Test the same speech model and audio with each backend:

- `HEX_BACKEND=cpu` — baseline, default.
- `HEX_BACKEND=cpuaccel` — alternate CPU implementation (if available).
- `HEX_BACKEND=metal` — experimental GPU diagnostic only. On the Radeon 5500M,
  the unfixed ggml concurrent-dispatch backend silently emits incorrect text;
  use it only to reproduce a bug, not for real dictation. Hex rejects a silent
  CPU fallback when Metal was requested.

The hidden `benchmark-transcription` command **requires a JSON manifest**
(the bare command without one is invalid). Record or supply the same WAV
for both devices, and place this `benchmark.json` in the same folder as
`test.wav` (with the exact reference words you spoke):

```json
{
  "clips": [
    {
      "id": "same-voice",
      "audio": "test.wav",
      "expected": "Точный текст, произнесённый на записи",
      "group": "ordinary"
    }
  ]
}
```

Then run (with the same GGUF model, preferably the one installed by Hex):

```sh
HEX="/Applications/Hex Test.app/Contents/MacOS/hex"
cd ~/Desktop/hex-bench
HEX_BACKEND=cpu "$HEX" benchmark-transcription benchmark.json --warmups 1 --runs 5 > cpu.jsonl
HEX_BACKEND=cpuaccel "$HEX" benchmark-transcription benchmark.json --warmups 1 --runs 5 > cpuaccel.jsonl
HEX_BACKEND=metal "$HEX" benchmark-transcription benchmark.json --warmups 1 --runs 5 > metal.jsonl
```

For a non-default model, add `--model /absolute/path/to/model.gguf` to
**all three commands**. Do not compare different GGUF files.

Each file contains a `{"type":"model","backend":...}` record and one
`{"type":"summary",...}` with `load_ms`, `median_run_ms`,
`median_x_realtime` and `wer`. Use median run time for warmed-up
inference; report loading time separately. A Metal request that fails should
be marked as unsupported, **not** as a slow CPU run.

```sh
python3 - <<'PY'
import json
for backend in ('cpu','cpuaccel','metal'):
    try:
        with open(f'{backend}.jsonl') as f:
            records = [json.loads(line) for line in f if line.strip()]
        device = next(r for r in records if r['type'] == 'model')
        row = next(r for r in records if r['type'] == 'summary')
        print(f"{backend:8} backend={device['backend']:27} load={row['load_ms']:.0f}ms"
              f" median={row['median_run_ms']:.0f}ms"
              f" x_realtime={row['median_x_realtime']:.2f}"
              f" WER={row['wer']:.3f}")
    except (OSError, StopIteration, ValueError) as error:
        print(f"{backend:8} unavailable/failed: {error}")
PY
```


For the **GUI launched through Finder**, an environment variable set in an
interactive terminal may not be inherited by Launch Services. Quit Hex Test,
then use `launchctl setenv HEX_BACKEND metal` before relaunching via `open`;
reset the Launch Services override with `launchctl unsetenv HEX_BACKEND`.
Check the Hex logs for the actual backend, not merely GPU activity in
Activity Monitor.

### Physical Radeon Pro 5500M result (2026-11-20)

**Metal speech inference is currently broken on this Intel Mac.** With the
same dictation setup, CPU produces correct text but `HEX_BACKEND=metal`
produced long repetitions of `tamb tamb tamb ...` although the speaker said
different words. This is a **correctness failure, not a performance result**.
A separate earlier crash report recorded SIGABRT in `transcription-engine`;
the common cause has not been established.

In standalone transcribe.cpp tests, `GGML_METAL_CONCURRENCY_DISABLE=1`
fixed Parakeet/GigaAM transcript corruption; the corrected Metal path was
slower than CPU (Parakeet 281 ms CPU / 456 ms Metal on a 3.5 s clip).
This ggml fix **has not been integrated into this Hex app**. Whisper still
produced incorrect GPU transcripts and must remain on CPU. CI only verifies
that an app can be built and packaged, not ASR correctness.

**Keep `HEX_BACKEND` unset (default CPU on Intel), or set it explicitly to
`cpu` and fully restart the app.** On the test Mac:

```sh
launchctl unsetenv HEX_BACKEND
# or: launchctl setenv HEX_BACKEND cpu
```

`RUNE_GPU_BACKEND=metal` in the bundle concerns **UI rendering**, not
transcription. The display Metal backend may be used independently from
CPU speech inference.

Before attempting another GPU speed measurement, use a saved WAV and the
same GGUF file to verify a *correct transcript* on Metal. See the investigation
on [fork PR #1](https://github.com/jirbis/hex/pull/1) and upstream
[ggml-org/llama.cpp#30702](https://github.com/ggml-org/llama.cpp/issues/30702)
(silent Metal output corruption on the same Radeon Pro 5500M). These are
strongly related symptoms, not yet a verified shared root cause.


## Verification / limitations

- Source changes: Intel transcription backend selection, Intel-only Rune
  packaging setting, dedicated `macos-15-intel` GitHub Actions workflow.
- The existing Swift bridge already has an Xcode 26.3-compatible
  `#if compiler(>=6.3)` guard for `SystemLanguageModel.contextSize`;
  it needs no additional patch.
- CI produces a ZIP containing an ad-hoc signed `Hex Test.app`, not a
  Developer ID-signed or notarized DMG.
- The CI runner has no Radeon Pro 5500M. Confirm Metal availability, model
  loading, real hotkey, dictation/paste, and German/Russian transcription on
  the MacBook before declaring the port complete.
- Do not copy the CPU/GPU performance figures from the older Radeon Pro 455
  report to this GPU. Measure on the 5500M.
