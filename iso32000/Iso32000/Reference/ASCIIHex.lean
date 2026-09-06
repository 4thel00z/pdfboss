import Iso32000.Reference.Bytes

/-!
The ASCIIHexDecode filter of ISO 32000-1 §7.4.2, as an executable reference:
each pair of hexadecimal digits is one byte, white space between digits is
ignored, `>` marks the end of data, and an odd trailing digit is read as if
followed by `0`. Any other character is an error.

`decode_encode` proves that decoding the encoding of any byte sequence gives
that sequence back, so the vectors written from these definitions are
consistent by construction.
-/

namespace Iso32000.Reference.ASCIIHex

/-- The `>` end-of-data marker. -/
def eod : Nat := 62

/-- The upper-case hexadecimal digit for a value below 16. -/
def hexDigit (n : Nat) : Nat := if n < 10 then 48 + n else 55 + n

/-- The value of a hexadecimal digit in either case, or `none`. -/
def hexValue (c : Nat) : Option Nat :=
  if 48 ≤ c ∧ c ≤ 57 then some (c - 48)
  else if 65 ≤ c ∧ c ≤ 70 then some (c - 55)
  else if 97 ≤ c ∧ c ≤ 102 then some (c - 87)
  else none

/-- Two upper-case digits per byte, then the end-of-data marker. -/
def encode (bs : List Nat) : List Nat :=
  bs.flatMap (fun b => [hexDigit (b / 16), hexDigit (b % 16)]) ++ [eod]

/-- Reads characters with the high nibble of an unfinished byte in `pending`. -/
def decodeFrom : List Nat → Option Nat → Except String (List Nat)
  | [], none => .ok []
  | [], some hi => .ok [hi * 16]
  | c :: cs, pending =>
    if c == eod then
      match pending with
      | none => .ok []
      | some hi => .ok [hi * 16]
    else if isWhitespace c then decodeFrom cs pending
    else
      match hexValue c, pending with
      | some v, none => decodeFrom cs (some v)
      | some v, some hi => (decodeFrom cs none).map (fun rest => (hi * 16 + v) :: rest)
      | none, _ => .error "not a hexadecimal digit"

def decode (cs : List Nat) : Except String (List Nat) := decodeFrom cs none

theorem hexValue_hexDigit (n : Nat) (h : n < 16) : hexValue (hexDigit n) = some n := by
  revert n
  decide

theorem hexDigit_ne_eod (n : Nat) (h : n < 16) : (hexDigit n == eod) = false := by
  revert n
  decide

theorem hexDigit_not_whitespace (n : Nat) (h : n < 16) : isWhitespace (hexDigit n) = false := by
  revert n
  decide

theorem decodeFrom_encode (bs : List Nat) (h : AllBytes bs) : decodeFrom (encode bs) none = .ok bs := by
  induction bs with
  | nil => simp [encode, decodeFrom, eod]
  | cons b rest ih =>
    have hb : b < 256 := h.head
    have hhi : b / 16 < 16 := by omega
    have hlo : b % 16 < 16 := by omega
    have hsplit : b / 16 * 16 + b % 16 = b := by omega
    simp only [encode, List.flatMap_cons, List.cons_append, List.nil_append]
    rw [decodeFrom, hexDigit_ne_eod _ hhi, hexDigit_not_whitespace _ hhi, hexValue_hexDigit _ hhi]
    simp only [Bool.false_eq_true, if_false]
    rw [decodeFrom, hexDigit_ne_eod _ hlo, hexDigit_not_whitespace _ hlo, hexValue_hexDigit _ hlo]
    simp only [Bool.false_eq_true, if_false]
    have ih' := ih h.tail
    simp only [encode] at ih'
    rw [ih', hsplit]
    rfl

/-- Decoding the encoding of any byte sequence gives the sequence back. -/
theorem decode_encode (bs : List Nat) (h : AllBytes bs) : decode (encode bs) = .ok bs :=
  decodeFrom_encode bs h

end Iso32000.Reference.ASCIIHex
