# Failure modes

Before/after renders of bugs fixed in this repository. Each pair is named
after the short hash of the commit that fixed it: `<hash>-before.png` is the
render at that commit's parent, `<hash>-after.png` the render at the commit.

| Commit | Failure |
| --- | --- |
| `ac63897` | Group 3 fax stream with fill bits before every end-of-line pattern and `/EndOfLine` unset decoded zero rows; the page's only image was skipped as truncated and painted as zero samples — a solid black page under `/DeviceGray`. |
| `ee7f114` | Character spacing set inside `q`/`Q` leaked into the runs that followed: text state parameters (`Tc`/`Tw`/`Tz`/`TL`/`Tf`/`Ts`) were held per content stream instead of in the graphics state, so Quartz-generated pages — one `q BT … Tc … ET Q` bracket per run — rendered later runs with inflated advances that overlapped the next run's absolute position. |
| `933ad8a` | DeviceCMYK→RGB used the additive `1 − min(1, ink + K)`, saturating every channel of a deep color to zero: the rich navy boxes of this financial report painted pure black. The multiplicative `(1−ink)·(1−K)` keeps the hue. |
