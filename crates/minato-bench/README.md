# minato-bench

Drives a running MinatoWM through its control pipe and measures how long
window operations take. It opens its own test windows on spare workspaces
(8 and 9 by default) and never touches other windows; afterwards every
monitor is put back on the workspace it was showing.

## Usage

1. Quit MinatoWM, then start it with the control pipe enabled:

   ```powershell
   $env:MINATO_CONTROL = '1'; .\minato.exe
   ```

2. Run scenarios (don't use the PC for a few minutes; windows will appear
   and workspaces will switch on the focused monitor):

   ```powershell
   .\minato-bench.exe run scenarios\open_close.json scenarios\ws_switch.json `
       scenarios\idle_switch.json scenarios\slow_apps.json --out result.json
   ```

   A markdown summary is printed; `result.json` holds it plus every raw
   sample (per-batch and per-animation timings).

3. Restart MinatoWM normally afterwards.

`minato-bench ctl '<json>'` sends a single request, e.g.
`{"cmd":"snapshot"}` or `{"cmd":"perf","clear":true}`.

## Columns

| column | meaning |
|---|---|
| wm | time the WM thread took for the request |
| settle | until the test windows stopped moving, appearing and repainting |
| anim start / end | until the animation started moving / finished |
| freeze wait | WM thread waiting for the frozen picture to be on screen |
| capture / flush | screen capture / waiting for the compositor, per freeze |
| fresh | freezes that first had to show the overlay window (costs an extra frame) |
| set_pos / show / hide / focus | Win32 window calls made by the WM |
| slow frames | animation frames longer than 1.5 refresh periods / all frames |

## Scenario format

```json
{
  "name": "example",
  "workspaces": [7, 8],
  "steps": [
    { "op": "delay", "paint_ms": 30, "size_ms": 15 },
    { "op": "workspace", "index": 7 },
    { "op": "open", "count": 4, "label": "open" },
    { "op": "cycle", "label": "switch", "repeat": 10, "interval_ms": 900,
      "workspaces": [7, 8], "warm_up_ms": 80 },
    { "op": "cycle", "label": "layout", "repeat": 4,
      "actions": [ { "type": "cycle_layout", "dir": "next" } ] },
    { "op": "close", "count": 4, "label": "close" }
  ]
}
```

Steps with a `label` are measured. `delay` makes the test windows slow to
paint / react to moves, like heavy apps on a weak CPU. `warm_up_ms`
simulates pressing Alt that long before the hotkey's key. `wait: false` on
a `cycle` sends at a fixed rate without waiting (key mashing). Actions use
the same JSON as `hotkeys` in `config.json`.
