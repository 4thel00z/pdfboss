import Iso32000.Reference.Bytes

/-!
The ASCII85Decode filter of ISO 32000-1 §7.4.3, as an executable reference:
groups of four bytes are written as five characters `!` to `u` holding the
base-85 digits of the 32-bit group value, most significant first; `z` stands
for a group of four zero bytes; white space is ignored; `~>` marks the end of
data; a final group of `n + 1` characters (n = 1, 2, 3) yields `n` bytes,
padding the missing characters with `u` when decoding. A group whose value
exceeds 2^32 - 1, a `z` inside a group, a lone final character, or any other
character is an error.

`decode_encode` proves that decoding the encoding of any byte sequence gives
that sequence back, including partial final groups.
-/

namespace Iso32000.Reference.ASCII85

def tilde : Nat := 126
def greaterThan : Nat := 62
def zero : Nat := 122
def offset : Nat := 33
def limit : Nat := 4294967296

/-- The five base-85 digits of a group value, most significant first. -/
def digits (v : Nat) : List Nat :=
  [v / 52200625 % 85, v / 614125 % 85, v / 7225 % 85, v / 85 % 85, v % 85]

/-- The group value of five base-85 digits. -/
def value5 : List Nat → Nat
  | [d0, d1, d2, d3, d4] => d0 * 52200625 + d1 * 614125 + d2 * 7225 + d3 * 85 + d4
  | _ => 0

def groupValue (b0 b1 b2 b3 : Nat) : Nat := b0 * 16777216 + b1 * 65536 + b2 * 256 + b3

def groupBytes (v : Nat) : List Nat :=
  [v / 16777216 % 256, v / 65536 % 256, v / 256 % 256, v % 256]

/-- The characters of one group. -/
def chars (v : Nat) : List Nat := (digits v).map (· + offset)

/-- Full groups, then a padded partial group written as `n + 1` characters.
The `z` shorthand is never written. -/
def encodeBody : List Nat → List Nat
  | b0 :: b1 :: b2 :: b3 :: rest => chars (groupValue b0 b1 b2 b3) ++ encodeBody rest
  | [b0, b1, b2] => (chars (groupValue b0 b1 b2 0)).take 4
  | [b0, b1] => (chars (groupValue b0 b1 0 0)).take 3
  | [b0] => (chars (groupValue b0 0 0 0)).take 2
  | [] => []

def encode (bs : List Nat) : List Nat := encodeBody bs ++ [tilde, greaterThan]

inductive Kind where
  | eod
  | space
  | zero
  | digit (d : Nat)
  | bad
  deriving DecidableEq, Repr

def classify (c : Nat) : Kind :=
  if c == tilde then .eod
  else if isWhitespace c then .space
  else if c == zero then .zero
  else if offset ≤ c ∧ c ≤ offset + 84 then .digit (c - offset)
  else .bad

/-- The first `n` bytes of a padded final group of five digits. -/
def partialGroup (ds : List Nat) (n : Nat) : Except String (List Nat) :=
  if value5 ds < limit then .ok ((groupBytes (value5 ds)).take n)
  else .error "group value too large"

/-- The final group: `n + 1` digits padded with `u` give `n` bytes. -/
def finish : List Nat → Except String (List Nat)
  | [] => .ok []
  | [_] => .error "a final group of one character"
  | [d0, d1] => partialGroup [d0, d1, 84, 84, 84] 1
  | [d0, d1, d2] => partialGroup [d0, d1, d2, 84, 84] 2
  | [d0, d1, d2, d3] => partialGroup [d0, d1, d2, d3, 84] 3
  | _ => .error "an open group of five digits"

/-- Reads characters with the digits of the open group in `acc`. -/
def decodeFrom : List Nat → List Nat → Except String (List Nat)
  | [], acc => finish acc
  | c :: cs, acc =>
    match classify c with
    | .eod => finish acc
    | .space => decodeFrom cs acc
    | .zero =>
      match acc with
      | [] => (decodeFrom cs []).map (fun rest => [0, 0, 0, 0] ++ rest)
      | _ => .error "z inside a group"
    | .digit d =>
      match acc with
      | [d0, d1, d2, d3] =>
        if value5 [d0, d1, d2, d3, d] < limit then
          (decodeFrom cs []).map (fun rest => groupBytes (value5 [d0, d1, d2, d3, d]) ++ rest)
        else .error "group value too large"
      | _ => decodeFrom cs (acc ++ [d])
    | .bad => .error "not an ASCII85 character"

def decode (cs : List Nat) : Except String (List Nat) := decodeFrom cs []

theorem classify_digit (d : Nat) (h : d < 85) : classify (d + offset) = .digit d := by
  revert d
  decide

theorem classify_tilde : classify tilde = .eod := rfl

theorem value5_digits (v : Nat) (h : v < 4437053125) : value5 (digits v) = v := by
  simp only [value5, digits]
  have e1 : v = 85 * (v / 85) + v % 85 := (Nat.div_add_mod v 85).symm
  have e2 : v / 85 = 85 * (v / 7225) + v / 85 % 85 := by
    rw [show (7225 : Nat) = 85 * 85 from rfl, ← Nat.div_div_eq_div_mul]
    exact (Nat.div_add_mod (v / 85) 85).symm
  have e3 : v / 7225 = 85 * (v / 614125) + v / 7225 % 85 := by
    rw [show (614125 : Nat) = 7225 * 85 from rfl, ← Nat.div_div_eq_div_mul]
    exact (Nat.div_add_mod (v / 7225) 85).symm
  have e4 : v / 614125 = 85 * (v / 52200625) + v / 614125 % 85 := by
    rw [show (52200625 : Nat) = 614125 * 85 from rfl, ← Nat.div_div_eq_div_mul]
    exact (Nat.div_add_mod (v / 614125) 85).symm
  have e5 : v / 52200625 % 85 = v / 52200625 := Nat.mod_eq_of_lt (by omega)
  omega

theorem groupBytes_groupValue (b0 b1 b2 b3 : Nat) (h0 : b0 < 256) (h1 : b1 < 256) (h2 : b2 < 256)
    (h3 : b3 < 256) : groupBytes (groupValue b0 b1 b2 b3) = [b0, b1, b2, b3] := by
  simp only [groupBytes, groupValue, List.cons.injEq, and_true]
  omega

theorem groupValue_lt (b0 b1 b2 b3 : Nat) (h0 : b0 < 256) (h1 : b1 < 256) (h2 : b2 < 256)
    (h3 : b3 < 256) : groupValue b0 b1 b2 b3 < limit := by
  simp only [groupValue, limit]; omega

theorem digit_lt (v k : Nat) : v / k % 85 < 85 := Nat.mod_lt _ (by decide)

theorem mod_lt (v : Nat) : v % 85 < 85 := Nat.mod_lt _ (by decide)

/-- Padding a three-byte group's four characters with one `u`. -/
theorem partial_three (v b0 b1 b2 : Nat) (h0 : b0 < 256) (h1 : b1 < 256) (h2 : b2 < 256)
    (hv : v = b0 * 16777216 + b1 * 65536 + b2 * 256) :
    (groupBytes (value5 [v / 52200625 % 85, v / 614125 % 85, v / 7225 % 85, v / 85 % 85, 84])).take 3
        = [b0, b1, b2] ∧
      value5 [v / 52200625 % 85, v / 614125 % 85, v / 7225 % 85, v / 85 % 85, 84] < limit := by
  have hd : value5 (digits v) = v := value5_digits v (by omega)
  simp only [value5, digits] at hd
  have hlt := mod_lt v
  have hpad : value5 [v / 52200625 % 85, v / 614125 % 85, v / 7225 % 85, v / 85 % 85, 84]
      = v + (84 - v % 85) := by
    simp only [value5]; omega
  rw [hpad]
  generalize hδ : 84 - v % 85 = δ
  have hδle : δ ≤ 84 := by omega
  clear hd hlt hpad hδ
  subst hv
  simp only [groupBytes, limit, List.take_succ_cons, List.take_zero, List.cons.injEq, and_true]
  omega

/-- Padding a two-byte group's three characters with two `u`. -/
theorem partial_two (v b0 b1 : Nat) (h0 : b0 < 256) (h1 : b1 < 256)
    (hv : v = b0 * 16777216 + b1 * 65536) :
    (groupBytes (value5 [v / 52200625 % 85, v / 614125 % 85, v / 7225 % 85, 84, 84])).take 2
        = [b0, b1] ∧
      value5 [v / 52200625 % 85, v / 614125 % 85, v / 7225 % 85, 84, 84] < limit := by
  have hd : value5 (digits v) = v := value5_digits v (by omega)
  simp only [value5, digits] at hd
  have h3 := digit_lt v 85
  have h4 := mod_lt v
  have hpad : value5 [v / 52200625 % 85, v / 614125 % 85, v / 7225 % 85, 84, 84]
      = v + (7224 - (v / 85 % 85 * 85 + v % 85)) := by
    simp only [value5, Nat.reduceMul]; omega
  rw [hpad]
  generalize hδ : 7224 - (v / 85 % 85 * 85 + v % 85) = δ
  have hδle : δ ≤ 7224 := by omega
  clear hd h3 h4 hpad hδ
  subst hv
  simp only [groupBytes, limit, List.take_succ_cons, List.take_zero, List.cons.injEq, and_true]
  omega

/-- Padding a one-byte group's two characters with three `u`. -/
theorem partial_one (v b0 : Nat) (h0 : b0 < 256) (hv : v = b0 * 16777216) :
    (groupBytes (value5 [v / 52200625 % 85, v / 614125 % 85, 84, 84, 84])).take 1 = [b0] ∧
      value5 [v / 52200625 % 85, v / 614125 % 85, 84, 84, 84] < limit := by
  have hd : value5 (digits v) = v := value5_digits v (by omega)
  simp only [value5, digits] at hd
  have h2 := digit_lt v 7225
  have h3 := digit_lt v 85
  have h4 := mod_lt v
  have hpad : value5 [v / 52200625 % 85, v / 614125 % 85, 84, 84, 84]
      = v + (614124 - (v / 7225 % 85 * 7225 + v / 85 % 85 * 85 + v % 85)) := by
    simp only [value5, Nat.reduceMul]; omega
  rw [hpad]
  generalize hδ : 614124 - (v / 7225 % 85 * 7225 + v / 85 % 85 * 85 + v % 85) = δ
  have hδle : δ ≤ 614124 := by omega
  clear hd h2 h3 h4 hpad hδ
  subst hv
  simp only [groupBytes, limit, List.take_succ_cons, List.take_zero, List.cons.injEq, and_true]
  omega

/-- Five digit characters decode as one full group and hand the rest of the
input on with an empty open group. Stated over abstract digits so that the
match reductions never meet the group arithmetic. -/
theorem decodeFrom_digits5 (d0 d1 d2 d3 d4 : Nat) (rest : List Nat) (h0 : d0 < 85) (h1 : d1 < 85)
    (h2 : d2 < 85) (h3 : d3 < 85) (h4 : d4 < 85) :
    decodeFrom ((d0 + offset) :: (d1 + offset) :: (d2 + offset) :: (d3 + offset) :: (d4 + offset) :: rest) []
      = if value5 [d0, d1, d2, d3, d4] < limit then
          (decodeFrom rest []).map (fun tail => groupBytes (value5 [d0, d1, d2, d3, d4]) ++ tail)
        else .error "group value too large" := by
  simp only [decodeFrom, classify_digit d0 h0, classify_digit d1 h1, classify_digit d2 h2,
    classify_digit d3 h3, classify_digit d4 h4, List.nil_append, List.cons_append]

theorem decodeFrom_digits4 (d0 d1 d2 d3 : Nat) (h0 : d0 < 85) (h1 : d1 < 85) (h2 : d2 < 85)
    (h3 : d3 < 85) :
    decodeFrom [d0 + offset, d1 + offset, d2 + offset, d3 + offset, tilde, greaterThan] []
      = partialGroup [d0, d1, d2, d3, 84] 3 := by
  simp only [decodeFrom, classify_digit d0 h0, classify_digit d1 h1, classify_digit d2 h2,
    classify_digit d3 h3, classify_tilde, finish, List.nil_append, List.cons_append]

theorem decodeFrom_digits3 (d0 d1 d2 : Nat) (h0 : d0 < 85) (h1 : d1 < 85) (h2 : d2 < 85) :
    decodeFrom [d0 + offset, d1 + offset, d2 + offset, tilde, greaterThan] []
      = partialGroup [d0, d1, d2, 84, 84] 2 := by
  simp only [decodeFrom, classify_digit d0 h0, classify_digit d1 h1, classify_digit d2 h2,
    classify_tilde, finish, List.nil_append, List.cons_append]

theorem decodeFrom_digits2 (d0 d1 : Nat) (h0 : d0 < 85) (h1 : d1 < 85) :
    decodeFrom [d0 + offset, d1 + offset, tilde, greaterThan] [] = partialGroup [d0, d1, 84, 84, 84] 1 := by
  simp only [decodeFrom, classify_digit d0 h0, classify_digit d1 h1, classify_tilde, finish,
    List.nil_append, List.cons_append]

/-- A full group's five characters decode to its four bytes. -/
theorem decodeFrom_group (b0 b1 b2 b3 : Nat) (rest : List Nat) (h0 : b0 < 256) (h1 : b1 < 256)
    (h2 : b2 < 256) (h3 : b3 < 256) :
    decodeFrom (chars (groupValue b0 b1 b2 b3) ++ rest) []
      = (decodeFrom rest []).map (fun tail => [b0, b1, b2, b3] ++ tail) := by
  have hv : value5 (digits (groupValue b0 b1 b2 b3)) = groupValue b0 b1 b2 b3 :=
    value5_digits _ (by have := groupValue_lt b0 b1 b2 b3 h0 h1 h2 h3; simp only [limit] at this; omega)
  have hlt := groupValue_lt b0 b1 b2 b3 h0 h1 h2 h3
  have hb := groupBytes_groupValue b0 b1 b2 b3 h0 h1 h2 h3
  simp only [chars, digits, List.map_cons, List.map_nil, List.cons_append, List.nil_append]
  rw [decodeFrom_digits5 _ _ _ _ _ _ (digit_lt _ _) (digit_lt _ _) (digit_lt _ _) (digit_lt _ _)
    (mod_lt _)]
  simp only [digits] at hv
  rw [hv]
  simp only [hlt, ↓reduceIte, hb]
  rfl

theorem decodeFrom_partial_three (b0 b1 b2 : Nat) (h0 : b0 < 256) (h1 : b1 < 256) (h2 : b2 < 256) :
    decodeFrom ((chars (groupValue b0 b1 b2 0)).take 4 ++ [tilde, greaterThan]) [] = .ok [b0, b1, b2] := by
  obtain ⟨hbytes, hlt⟩ := partial_three (groupValue b0 b1 b2 0) b0 b1 b2 h0 h1 h2 (by simp [groupValue])
  simp only [chars, digits, List.map_cons, List.map_nil, List.take_succ_cons, List.take_zero,
    List.cons_append, List.nil_append]
  rw [decodeFrom_digits4 _ _ _ _ (digit_lt _ _) (digit_lt _ _) (digit_lt _ _) (digit_lt _ _)]
  simp only [partialGroup, hlt, ↓reduceIte, hbytes]

theorem decodeFrom_partial_two (b0 b1 : Nat) (h0 : b0 < 256) (h1 : b1 < 256) :
    decodeFrom ((chars (groupValue b0 b1 0 0)).take 3 ++ [tilde, greaterThan]) [] = .ok [b0, b1] := by
  obtain ⟨hbytes, hlt⟩ := partial_two (groupValue b0 b1 0 0) b0 b1 h0 h1 (by simp [groupValue])
  simp only [chars, digits, List.map_cons, List.map_nil, List.take_succ_cons, List.take_zero,
    List.cons_append, List.nil_append]
  rw [decodeFrom_digits3 _ _ _ (digit_lt _ _) (digit_lt _ _) (digit_lt _ _)]
  simp only [partialGroup, hlt, ↓reduceIte, hbytes]

theorem decodeFrom_partial_one (b0 : Nat) (h0 : b0 < 256) :
    decodeFrom ((chars (groupValue b0 0 0 0)).take 2 ++ [tilde, greaterThan]) [] = .ok [b0] := by
  obtain ⟨hbytes, hlt⟩ := partial_one (groupValue b0 0 0 0) b0 h0 (by simp [groupValue])
  simp only [chars, digits, List.map_cons, List.map_nil, List.take_succ_cons, List.take_zero,
    List.cons_append, List.nil_append]
  rw [decodeFrom_digits2 _ _ (digit_lt _ _) (digit_lt _ _)]
  simp only [partialGroup, hlt, ↓reduceIte, hbytes]

theorem decodeFrom_encodeBody (bs : List Nat) (h : AllBytes bs) :
    decodeFrom (encodeBody bs ++ [tilde, greaterThan]) [] = .ok bs := by
  induction bs using encodeBody.induct with
  | case1 b0 b1 b2 b3 rest ih =>
    have h0 : b0 < 256 := h.head
    have h1 : b1 < 256 := h.tail.head
    have h2 : b2 < 256 := h.tail.tail.head
    have h3 : b3 < 256 := h.tail.tail.tail.head
    rw [encodeBody, List.append_assoc, decodeFrom_group b0 b1 b2 b3 _ h0 h1 h2 h3,
      ih h.tail.tail.tail.tail]
    rfl
  | case2 b0 b1 b2 =>
    exact decodeFrom_partial_three b0 b1 b2 h.head h.tail.head h.tail.tail.head
  | case3 b0 b1 => exact decodeFrom_partial_two b0 b1 h.head h.tail.head
  | case4 b0 => exact decodeFrom_partial_one b0 h.head
  | case5 => rfl

/-- Decoding the encoding of any byte sequence gives the sequence back. -/
theorem decode_encode (bs : List Nat) (h : AllBytes bs) : decode (encode bs) = .ok bs :=
  decodeFrom_encodeBody bs h

end Iso32000.Reference.ASCII85
