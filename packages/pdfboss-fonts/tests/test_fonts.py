import os
import pdfboss_fonts

def test_font_dir_exists_and_holds_ttfs():
    d = pdfboss_fonts.font_dir()
    assert os.path.isdir(d)
    ttfs = [f for f in os.listdir(d) if f.endswith(".ttf")]
    assert len(ttfs) == 10, f"expected 10 bundled faces, found {ttfs}"
    # The canonical basenames the renderer's face_filename() asks for.
    for name in ["Arimo[wght].ttf", "Tinos-Regular.ttf", "Cousine-Regular.ttf"]:
        assert os.path.exists(os.path.join(d, name)), name

def test_ofl_license_shipped():
    assert os.path.exists(os.path.join(pdfboss_fonts.font_dir(), "OFL.txt"))
