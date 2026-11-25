# All-pass phase section

Use `allpass` to change phase and group delay while keeping magnitude at unity.
The stereo channels have independent state. The device uses no lookahead and
requests no host alignment compensation. This does not mean zero phase delay:
the default coefficient `0` delays the signal by one sample.

| Parameter | Unit | Range | Default |
| --- | --- | --- | --- |
| `coefficient` | normalized digital coefficient | −0.999 … 0.999 | 0 |

For coefficient `a`, the transfer function is
`H(z) = (a + z^-1) / (1 + a z^-1)`. The pole stays strictly inside the unit
circle. Its decay is measured in samples, so the same coefficient rings for
longer in seconds at a lower sample rate. Export and bounce retain a bounded
ring-out separately from alignment latency.

```ocp
@project
@track phase | instrument: synth/lead_sine
  effects:
    - allpass: coefficient=0.5
  @phrase line [1:1 - 2:1]
    C4 q
```

The GUI uses the shared device catalog and parameter control. Agents discover
the same parameter through `devices_catalog`; `allpass` is also an effect and
automation lane name. Cascade sections when you need a more complex phase
response. Keep the coefficient constant when you need the unity-magnitude
response of a time-invariant filter; modulation can change transient levels.
