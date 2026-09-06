import Iso32000.Catalogue
import Iso32000.Generated
import Iso32000.Outline

/-!
The computable side of the gate: what each ledger row obliges the source tree
to show, which rows fall short, which clauses of the standard no row speaks
to, and which citations name clauses the standard does not have. `Gate.lean`
shows these lists empty; `Report.lean` prints them when they are not.

The work is cut into one slice per chapter (annexes together), because a
clause can only cover, be cited as, or be a heading of its own chapter. Each
row is then compared with a few hundred citations instead of all of them.
-/

namespace Iso32000

/-- What a row's status obliges the source tree to show. -/
def Feature.obligationsMet (f : Feature) (citations : List Citation) : Bool :=
  match f.status with
  | .implemented => f.citedInCode citations && f.citedInTests citations
  | .incomplete => f.citedInCode citations && !f.note.isEmpty
  | .notImplemented => !f.note.isEmpty
  | .outOfScope => !f.note.isEmpty

/-- The clauses of the standard every ledger must speak to: chapters 7 to 14
to depth 3, skipping the introductory `General` sub-clauses, plus the
normative annexes at depth 2. -/
def Outline.required : List Heading :=
  Outline.headings.filter fun h =>
    match h.ref with
    | .clause path =>
      match path with
      | chapter :: _ => 7 ≤ chapter && chapter ≤ 14 && path.length ≤ 3 && h.title != "General"
      | [] => false
    | .annex letter path =>
      ['A', 'B', 'C', 'D', 'E', 'F', 'I'].contains letter && path.length ≤ 1 && h.title != "General"

/-- A clause is spoken to when a row sits at or below it, or when a row above
it declares the whole area incomplete, not implemented or out of scope. An
`implemented` row above a clause does not vouch for that clause. -/
def Heading.spokenTo (h : Heading) (features : List Feature) : Bool :=
  features.any fun f =>
    h.ref.covers f.ref || (f.ref.covers h.ref && f.status != .implemented)

/-- One chapter's share of the gate: its rows, the citations naming its
clauses, its required headings and all its headings. -/
structure Slice where
  keys : List Nat
  features : List Feature
  citations : List Citation
  required : List Heading
  headings : List Heading

def Slice.of (keys : List Nat) (features : List Feature) : Slice :=
  { keys
    features
    citations := Generated.citations.filter fun c => c.ref.any fun r => keys.contains r.chapterKey
    required := Outline.required.filter fun h => keys.contains h.ref.chapterKey
    headings := Outline.headings.filter fun h => keys.contains h.ref.chapterKey }

def Gate.slices : List Slice := [
  Slice.of [7] Catalogue.chapter7,
  Slice.of [8] Catalogue.chapter8,
  Slice.of [9] Catalogue.chapter9,
  Slice.of [10, 11] Catalogue.chapter10and11,
  Slice.of [12] Catalogue.chapter12,
  Slice.of [13] Catalogue.chapter13,
  Slice.of [14] Catalogue.chapter14,
  Slice.of [100] Catalogue.annexes
]

/-- Every row sits in the slice of its own chapter. -/
def Gate.misfiledRows : List Feature :=
  Gate.slices.flatMap fun s => s.features.filter fun f => !s.keys.contains f.ref.chapterKey

/-- The rows whose obligations the source tree does not meet. -/
def Slice.offenders (s : Slice) : List Feature :=
  s.features.filter fun f => !f.obligationsMet s.citations

def Gate.offenders : List Feature := Gate.slices.flatMap Slice.offenders

/-- References that more than one row of the slice claims. -/
def Slice.duplicateRefs (s : Slice) : List Ref :=
  let refs := s.features.map (·.ref)
  refs.filter fun r => refs.count r > 1

def Gate.duplicateRefs : List Ref := Gate.slices.flatMap Slice.duplicateRefs

/-- Required clauses with no ledger row speaking to them. -/
def Slice.unaddressed (s : Slice) : List Heading :=
  s.required.filter fun h => !h.spokenTo s.features

def Gate.unaddressed : List Heading := Gate.slices.flatMap Slice.unaddressed

/-- Citations of the first edition that name a clause the standard does not
have: the cited number must be a heading or an ancestor of one. Second-edition
citations are numbered differently and are not checked. -/
def Slice.danglingCitations (s : Slice) : List Citation :=
  s.citations.filter fun c =>
    match c.part, c.ref with
    | some 2, _ => false
    | _, none => false
    | _, some r => !s.headings.any fun h => r.covers h.ref

def Gate.danglingCitations : List Citation := Gate.slices.flatMap Slice.danglingCitations

/-- Rows that no slice holds, so that the slices together are the ledger. -/
def Gate.unslicedRows : List Feature :=
  let sliced := Gate.slices.flatMap (·.features)
  Catalogue.features.filter fun f => !sliced.any fun g => g.ref == f.ref

end Iso32000
