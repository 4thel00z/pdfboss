import Iso32000.Catalogue.Syntax
import Iso32000.Catalogue.Graphics
import Iso32000.Catalogue.Text
import Iso32000.Catalogue.Rendering
import Iso32000.Catalogue.Interactive
import Iso32000.Catalogue.Multimedia
import Iso32000.Catalogue.Interchange
import Iso32000.Catalogue.Annexes

/-!
The conformance ledger: one row per clause of ISO 32000 with pdfboss's
declared status. `Gate.lean` proves the rows consistent with the source tree.
-/

namespace Iso32000

def Catalogue.features : List Feature :=
  Catalogue.chapter7 ++ Catalogue.chapter8 ++ Catalogue.chapter9 ++ Catalogue.chapter10and11 ++
    Catalogue.chapter12 ++ Catalogue.chapter13 ++ Catalogue.chapter14 ++ Catalogue.annexes

end Iso32000
