"""Bundled OFL 1.1 substitute fonts for ``pdfboss[full]`` (Croscore: Arimo, Tinos, Cousine)."""
import os

__version__ = "0.1.0"

def font_dir() -> str:
    """Absolute path to the directory holding the bundled ``.ttf`` files.

    ``pdfboss``'s renderer consumes this as its substitute-font directory
    when ``fonts="full"`` is requested.
    """
    return os.path.join(os.path.dirname(os.path.abspath(__file__)), "fonts")
