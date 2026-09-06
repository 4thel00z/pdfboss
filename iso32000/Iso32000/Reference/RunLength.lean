import Iso32000.Reference.Bytes

/-!
The RunLengthDecode filter of ISO 32000-1 §7.4.5, as an executable reference:
a length byte `0` to `127` is followed by that many plus one literal bytes, a
length byte `129` to `255` is followed by one byte to repeat `257 - length`
times, and `128` is the end of data.

`decode_encode` proves that decoding the encoding of any byte sequence gives
that sequence back.
-/

namespace Iso32000.Reference.RunLength

/-- The end-of-data length byte. -/
def eod : Nat := 128

/-- Literal runs of at most 128 bytes, then the end-of-data byte. -/
def encode (bs : List Nat) : List Nat :=
  match bs with
  | [] => [eod]
  | b :: rest =>
    let chunk := (b :: rest).take 128
    (chunk.length - 1) :: (chunk ++ encode ((b :: rest).drop 128))
termination_by bs.length
decreasing_by simp; omega

/-- Reads runs until the end-of-data byte; data that simply ends is accepted. -/
def decode (cs : List Nat) : Except String (List Nat) :=
  match cs with
  | [] => .ok []
  | l :: rest =>
    if l == eod then .ok []
    else if l < 128 then
      if rest.length < l + 1 then .error "truncated literal run"
      else (decode (rest.drop (l + 1))).map (fun tail => rest.take (l + 1) ++ tail)
    else
      match rest with
      | [] => .error "truncated repeat run"
      | b :: rest' => (decode rest').map (fun tail => List.replicate (257 - l) b ++ tail)
termination_by cs.length
decreasing_by all_goals simp; omega

theorem encode_cons (b : Nat) (rest : List Nat) :
    encode (b :: rest) =
      (((b :: rest).take 128).length - 1) ::
        (((b :: rest).take 128) ++ encode ((b :: rest).drop 128)) := by
  rw [encode]

theorem decode_cons (l : Nat) (rest : List Nat) :
    decode (l :: rest) =
      if l == eod then .ok []
      else if l < 128 then
        if rest.length < l + 1 then .error "truncated literal run"
        else (decode (rest.drop (l + 1))).map (fun tail => rest.take (l + 1) ++ tail)
      else
        match rest with
        | [] => .error "truncated repeat run"
        | b :: rest' => (decode rest').map (fun tail => List.replicate (257 - l) b ++ tail) := by
  rw [decode.eq_def]

theorem decode_encode (bs : List Nat) : decode (encode bs) = .ok bs := by
  induction bs using encode.induct with
  | case1 => simp [encode, decode, eod]
  | case2 b rest ih =>
    have hlen : ((b :: rest).take 128).length = min 128 (rest.length + 1) := by
      simp [List.length_take]; omega
    have hsmall : ((b :: rest).take 128).length - 1 < 128 := by omega
    have hne : (((b :: rest).take 128).length - 1 == eod) = false := by
      simp [eod]; omega
    have hfull : ((b :: rest).take 128).length - 1 + 1 = ((b :: rest).take 128).length := by omega
    have hnot : ¬ (((b :: rest).take 128 ++ encode ((b :: rest).drop 128)).length <
        ((b :: rest).take 128).length) := by
      simp [List.length_append]
    rw [encode_cons, decode_cons]
    simp only [hne, hsmall, hfull, hnot, Bool.false_eq_true, ↓reduceIte]
    rw [List.drop_left' rfl, List.take_left' rfl, ih]
    simp only [Except.map]
    rw [List.take_append_drop]

end Iso32000.Reference.RunLength
