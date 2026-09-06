/-!
Shared vocabulary of the executable references: a byte is a `Nat` below 256,
and the white-space characters are those of ISO 32000-1 §7.2.2, Table 1.
-/

namespace Iso32000.Reference

/-- The six white-space characters of ISO 32000-1 §7.2.2 (Table 1): NUL,
HT, LF, FF, CR and SP. -/
def isWhitespace (c : Nat) : Bool :=
  c == 0 || c == 9 || c == 10 || c == 12 || c == 13 || c == 32

/-- Every element of `bs` is a byte. -/
def AllBytes (bs : List Nat) : Prop := ∀ b ∈ bs, b < 256

theorem AllBytes.nil : AllBytes [] := by
  intro b h
  simp at h

theorem AllBytes.cons {b : Nat} {bs : List Nat} (hb : b < 256) (hbs : AllBytes bs) :
    AllBytes (b :: bs) := by
  intro x hx
  simp at hx
  rcases hx with rfl | hx
  · exact hb
  · exact hbs x hx

theorem AllBytes.head {b : Nat} {bs : List Nat} (h : AllBytes (b :: bs)) : b < 256 :=
  h b (by simp)

theorem AllBytes.tail {b : Nat} {bs : List Nat} (h : AllBytes (b :: bs)) : AllBytes bs :=
  fun x hx => h x (by simp [hx])

theorem AllBytes.take {bs : List Nat} (n : Nat) (h : AllBytes bs) : AllBytes (bs.take n) :=
  fun x hx => h x (List.mem_of_mem_take hx)

theorem AllBytes.drop {bs : List Nat} (n : Nat) (h : AllBytes bs) : AllBytes (bs.drop n) :=
  fun x hx => h x (List.mem_of_mem_drop hx)

end Iso32000.Reference
