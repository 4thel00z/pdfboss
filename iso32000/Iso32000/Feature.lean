import Iso32000.Ref

/-!
The conformance ledger's vocabulary: what pdfboss claims about a clause of
ISO 32000, and what a citation found in the source tree looks like.
-/

namespace Iso32000

/-- What pdfboss claims for a clause. Each status carries obligations that
`Gate.lean` turns into theorems. -/
inductive Status where
  /-- The clause is handled; code cites it and at least one test cites it. -/
  | implemented
  /-- Part of the clause is handled; code cites it and the note says what is
  missing. -/
  | incomplete
  /-- Nothing handles the clause yet; the note says so. -/
  | notImplemented
  /-- pdfboss deliberately leaves the clause alone; the note says why. -/
  | outOfScope
  deriving DecidableEq, Repr

/-- One row of the ledger. `ref` uses the ISO 32000-1:2008 numbering, which
the outline and most source citations use; `ref2` names the ISO 32000-2
clause when the second edition numbers it differently. -/
structure Feature where
  ref : Ref
  title : String
  status : Status
  ref2 : Option Ref := none
  note : String := ""
  deriving Repr

/-- A mention of the standard found in the source tree by `iso32000-index`.
`part` is `some 1` for `ISO 32000-1`, `some 2` for `ISO 32000-2`, and `none`
when the text names no part. `ref` is `none` when the mention names no clause
or table the index could resolve. -/
structure Citation where
  part : Option Nat
  ref : Option Ref
  file : String
  line : Nat
  inTest : Bool
  deriving Repr

namespace Feature

/-- The reference a citation of `part` is compared against. Without a
distinct `ref2`, the second edition is taken to number the clause the same
way. -/
def refFor (f : Feature) (part : Option Nat) : Ref :=
  match part, f.ref2 with
  | some 2, some r => r
  | _, _ => f.ref

/-- Whether a citation names this feature's clause or a sub-clause of it. A
citation of a parent clause does not count. -/
def citedBy (f : Feature) (c : Citation) : Bool :=
  match c.ref with
  | none => false
  | some r => (f.refFor c.part).covers r

def citedInCode (f : Feature) (citations : List Citation) : Bool :=
  citations.any fun c => !c.inTest && f.citedBy c

def citedInTests (f : Feature) (citations : List Citation) : Bool :=
  citations.any fun c => c.inTest && f.citedBy c

end Feature

end Iso32000
