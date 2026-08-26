# Failure modes

Before/after renders of bugs fixed in this repository. Each pair is named
after the short hash of the commit that fixed it: `<hash>-before.png` is the
render at that commit's parent, `<hash>-after.png` the render at the commit.

| Commit | Failure |
| --- | --- |
| `6c54436` | Group 3 fax stream with fill bits before every end-of-line pattern and `/EndOfLine` unset decoded zero rows; the page's only image was skipped as truncated and painted as zero samples — a solid black page under `/DeviceGray`. |
