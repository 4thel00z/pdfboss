import Iso32000.Reference.ASCIIHex
import Iso32000.Reference.ASCII85
import Iso32000.Reference.RunLength

/-!
`lake exe iso32000-vectors <dir>` writes the differential test vectors for the
stream filters of ISO 32000-1 §7.4.2, §7.4.3 and §7.4.5. Every expected output
is computed by the executable reference in `Iso32000.Reference`, whose
roundtrip theorems the kernel has checked; the Rust tests in
`crates/pdfboss-core/tests/iso32000_vectors.rs` hold pdfboss's decoders to
these outputs.

One vector per line: `name<TAB>input-hex<TAB>expected-hex`, or `ERR` in place
of the expected output when the reference rejects the input. Lines starting
with `#` are comments.
-/

open Iso32000.Reference System

namespace Vectors

def hexDigits : Array Char := "0123456789abcdef".toList.toArray

def hexOf (bs : List Nat) : String :=
  String.ofList (bs.flatMap fun b => [hexDigits[b / 16 % 16]!, hexDigits[b % 16]!])

def bytesOf (s : String) : List Nat := s.toList.map (·.toNat)

/-- A fixed linear congruential sequence, so the vectors are reproducible. -/
def randomBytes (seed length : Nat) : List Nat :=
  let step (state : Nat) : Nat := (state * 1103515245 + 12345) % 2147483648
  (List.range length).foldl (fun (acc, state) _ =>
    let next := step state
    (acc ++ [next / 65536 % 256], next)) ([], seed) |>.1

def lengths : List Nat := (List.range 21) ++ [31, 32, 33, 63, 64, 65, 127, 128, 129, 255, 256, 257, 1000]

structure Vector where
  name : String
  input : List Nat
  expected : Except String (List Nat)

def render (v : Vector) : String :=
  let expected := match v.expected with
    | .ok bs => hexOf bs
    | .error _ => "ERR"
  s!"{v.name}\t{hexOf v.input}\t{expected}"

def randomVectors (label : String) (encode : List Nat → List Nat)
    (decode : List Nat → Except String (List Nat)) : List Vector :=
  lengths.map fun n =>
    let bytes := randomBytes (n + 7) n
    { name := s!"{label}-random-{n}", input := encode bytes, expected := decode (encode bytes) }

def handVectors (label : String) (decode : List Nat → Except String (List Nat))
    (cases : List (String × List Nat)) : List Vector :=
  cases.map fun (name, input) => { name := s!"{label}-{name}", input, expected := decode input }

def asciiHex : List Vector :=
  randomVectors "ascii-hex" ASCIIHex.encode ASCIIHex.decode ++
  handVectors "ascii-hex" ASCIIHex.decode [
    ("upper", bytesOf "48656C6C6F>"),
    ("lower", bytesOf "48656c6c6f>"),
    ("whitespace", bytesOf "48 65\n6C\t6C\r6F >"),
    ("odd-digit-padded", bytesOf "48656C6C6F7>"),
    ("missing-eod", bytesOf "4142"),
    ("eod-only", bytesOf ">"),
    ("data-after-eod", bytesOf "41>42"),
    ("empty", [])
  ]

def ascii85 : List Vector :=
  randomVectors "ascii85" ASCII85.encode ASCII85.decode ++
  handVectors "ascii85" ASCII85.decode [
    ("hello-world", bytesOf "87cURD]i,\"Ebo80~>"),
    ("z-group", bytesOf "z~>"),
    ("two-z-groups", bytesOf "zz~>"),
    ("z-between-groups", bytesOf "87cURDz]i,\"Ebo80~>"),
    ("whitespace", bytesOf "87cU\nRD]i,\"Eb o80\r\n~>"),
    ("missing-eod", bytesOf "87cURD]i,\"Ebo80"),
    ("max-group", bytesOf "s8W-!~>"),
    ("group-overflow", bytesOf "s8W-\"~>"),
    ("z-inside-group", bytesOf "87z~>"),
    ("eod-only", bytesOf "~>"),
    ("empty", [])
  ]

def runLength : List Vector :=
  randomVectors "run-length" RunLength.encode RunLength.decode ++
  handVectors "run-length" RunLength.decode [
    ("literal", [2, 65, 66, 67, 128]),
    ("repeat-three", [254, 65, 128]),
    ("repeat-two", [255, 66, 128]),
    ("repeat-128", [129, 67, 128]),
    ("mixed", [1, 65, 66, 253, 67, 0, 68, 128]),
    ("missing-eod", [2, 65, 66, 67]),
    ("eod-only", [128]),
    ("data-after-eod", [0, 65, 128, 0, 66]),
    ("full-literal-run", 127 :: (List.range 128) ++ [128]),
    ("empty", [])
  ]

def fileText (title : String) (vectors : List Vector) : String :=
  s!"# {title}\n# Written by `lake exe iso32000-vectors`; regenerate rather than edit.\n" ++
  "\n".intercalate (vectors.map render) ++ "\n"

end Vectors

open Vectors in
def main (args : List String) : IO UInt32 := do
  let dir : FilePath := (args.head?.getD "vectors" : String)
  IO.FS.createDirAll dir
  IO.FS.writeFile (dir / "ascii_hex.txt")
    (fileText "ASCIIHexDecode, ISO 32000-1 §7.4.2" asciiHex)
  IO.FS.writeFile (dir / "ascii85.txt")
    (fileText "ASCII85Decode, ISO 32000-1 §7.4.3" ascii85)
  IO.FS.writeFile (dir / "run_length.txt")
    (fileText "RunLengthDecode, ISO 32000-1 §7.4.5" runLength)
  IO.eprintln s!"{asciiHex.length + ascii85.length + runLength.length} vectors written to {dir}"
  return 0
