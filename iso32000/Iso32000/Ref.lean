/-!
References into ISO 32000: numbered clauses, annex clauses and tables.
-/

namespace Iso32000

/-- A place in the standard: clause `7.4.2` is `.clause [7, 4, 2]`, annex
clause `D.2` is `.annex 'D' [2]`, and annex `D` itself is `.annex 'D' []`. -/
inductive Ref where
  | clause (path : List Nat)
  | annex (letter : Char) (path : List Nat)
  deriving DecidableEq, Repr, Inhabited

namespace Ref

/-- `a.covers b` holds when `b` is `a` itself or a sub-clause of `a`.
Comparison is segment-wise: `7.4` covers `7.4.2` but not `7.40`. -/
def covers : Ref → Ref → Bool
  | .clause a, .clause b => a.isPrefixOf b
  | .annex la a, .annex lb b => la == lb && a.isPrefixOf b
  | _, _ => false

/-- `a.related b` holds when one of the two covers the other. -/
def related (a b : Ref) : Bool := a.covers b || b.covers a

/-- Nesting depth: `7` is 1, `7.4.2` is 3, annex `D` is 1, `D.2` is 2. -/
def depth : Ref → Nat
  | .clause p => p.length
  | .annex _ p => p.length + 1

/-- The chapter number of a clause reference; annexes have none. -/
def chapter : Ref → Option Nat
  | .clause (c :: _) => some c
  | _ => none

/-- The chapter number of a clause, or `100` for every annex: the key the
gate partitions its work by, since a clause can only cover, be cited as or
be a heading of its own chapter. -/
def chapterKey : Ref → Nat
  | .clause (c :: _) => c
  | .clause [] => 0
  | .annex _ _ => 100

/-- The reference one level up, or `none` at the top. -/
def parent : Ref → Option Ref
  | .clause [] => none
  | .clause p => some (.clause p.dropLast)
  | .annex _ [] => none
  | .annex l p => some (.annex l p.dropLast)

/-- `7.4.2` or `D.2`, the way the standard writes it. -/
def render : Ref → String
  | .clause p => ".".intercalate (p.map toString)
  | .annex l [] => String.singleton l
  | .annex l p => String.singleton l ++ "." ++ ".".intercalate (p.map toString)

/-- Document order: clauses before annexes, then lexicographic on the path. -/
def key : Ref → List Nat
  | .clause p => 0 :: p
  | .annex l p => 1 :: l.toNat :: p

end Ref

/-- A heading of the standard: a reference and its title. -/
structure Heading where
  ref : Ref
  title : String
  deriving Repr, Inhabited

/-- A numbered table of the standard and the clause its caption appears in. -/
structure Table where
  number : Nat
  within : Ref
  deriving Repr, Inhabited

end Iso32000
