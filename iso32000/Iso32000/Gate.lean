import Iso32000.Checks

/-!
The quality gate. Every theorem here holds the ledger in `Catalogue` to the
citations `iso32000-index` found in the source tree and to the outline of the
standard. A false claim in the ledger, a clause whose tests stopped citing it,
or a clause of the standard with no ledger row fails the build.
`lake exe iso32000-report --check` names the cause.

The ledger theorems are decided by `native_decide`: Lean compiles the check,
runs it, and the kernel accepts the verdict through the `Lean.ofReduceBool`
axiom, so they trust the compiler as well as the kernel. Deciding them inside
the kernel alone (`decide +kernel`) takes longer than ten minutes on a ledger
of this size. The roundtrip theorems of the reference decoders in
`Iso32000.Reference` use the kernel only.
-/

namespace Iso32000

theorem every_row_is_in_one_slice : Gate.unslicedRows.isEmpty = true := by
  native_decide

theorem every_row_sits_in_its_own_chapter : Gate.misfiledRows.isEmpty = true := by
  native_decide

theorem ledger_rows_are_unique : Gate.duplicateRefs.isEmpty = true := by
  native_decide

theorem every_row_meets_its_obligations : Gate.offenders.isEmpty = true := by
  native_decide

theorem every_required_clause_is_spoken_to : Gate.unaddressed.isEmpty = true := by
  native_decide

theorem every_cited_clause_exists : Gate.danglingCitations.isEmpty = true := by
  native_decide

end Iso32000
