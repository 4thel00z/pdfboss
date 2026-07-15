# pdfboss-fonts

Data package bundling the OFL 1.1 substitute faces used by `pdfboss[full]`.

Ships 10 TrueType fonts from the Croscore family — Arimo (variable weight,
regular and italic), Tinos (regular, bold, italic, bold-italic), and Cousine
(regular, bold, italic, bold-italic) — plus their `OFL.txt` license and
`NOTICE`. These faces are used by `pdfboss`'s renderer as substitute fonts
when a PDF references a non-embedded font.

## Usage

```python
import pdfboss_fonts

pdfboss_fonts.font_dir()  # -> absolute path to the bundled .ttf directory
```

## License

The bundled fonts are licensed under the SIL Open Font License, Version 1.1
(OFL-1.1). See `pdfboss_fonts/fonts/OFL.txt` and `pdfboss_fonts/fonts/NOTICE`.
