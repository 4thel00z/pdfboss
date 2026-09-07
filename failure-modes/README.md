# Failure modes

Before/after renders of bugs fixed in this repository. Each pair is named
after the short hash of the commit that fixed it (plus a slug when one commit
fixed several): `<hash>-before.png` is the render at that commit's parent,
`<hash>-after.png` the render at the commit.

| Commit | Failure |
| --- | --- |
| `ac63897` | Group 3 fax stream with fill bits before every end-of-line pattern and `/EndOfLine` unset decoded zero rows; the page's only image was skipped as truncated and painted as zero samples — a solid black page under `/DeviceGray`. |
| `2ddb64f` (text-state) | Character spacing set inside `q`/`Q` leaked into the runs that followed: text state parameters (`Tc`/`Tw`/`Tz`/`TL`/`Tf`/`Ts`) were held per content stream instead of in the graphics state, so Quartz-generated pages — one `q BT … Tc … ET Q` bracket per run — rendered later runs with inflated advances that overlapped the next run's absolute position. |
| `2ddb64f` (cmyk) | DeviceCMYK→RGB used the additive `1 − min(1, ink + K)`, saturating every channel of a deep color to zero: the rich navy boxes of this financial report painted pure black. The multiplicative `(1−ink)·(1−K)` keeps the hue. |
| `6ba4cec` | TIFF predictor 2 was undone for 8-bit components only, so a 16-bit gray image stored with `/Predictor 2` painted its raw differences: a black square with stray edges instead of the gradient. The fix adds 16-bit components as big-endian values and sub-byte components inside their packed bytes. |
| `a09ef43` | JPEG samples skipped the `/Decode` array and the `DCTDecode` `/ColorTransform` parameter was never read. Left: a Photoshop-style CMYK JPEG (stored as 255 minus ink, `/Decode [1 0 1 0 1 0 1 0]`) painted solid black instead of cyan, magenta, yellow and black. Right: an RGB JPEG stored without the YCbCr transform under `/ColorTransform 0` painted false colours instead of red, green, blue and gray. |
| `a09ef43` (adobe-rgb) | A three-component JPEG whose Adobe APP14 flag says no colour transform, but whose component ids are 1, 2, 3, was decoded as YCbCr because the decoder crate consults the ids first: the USDA GAIN Report globe on this page painted magenta. The marker's flag now wins, as Table 13 says, and the globe is blue. |
