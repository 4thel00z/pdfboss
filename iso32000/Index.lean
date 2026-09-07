import Iso32000.Feature
import Iso32000.Outline

/-!
`lake exe iso32000-index <repo-root>` scans the Rust and Python sources for
mentions of ISO 32000 and writes them to `Iso32000/Generated.lean`.

A mention is the text `ISO 32000`, an optional part (`-1`, `-2`), and then
a clause (`7.4.2`, `§7.4.2`, `clause 7.4.2`), an annex clause (`Annex D.2`)
or a table (`Table 11`, resolved through the outline to the clause its
caption sits in). Anything else after `ISO 32000` is recorded with no
reference so the report can list it.

A line is in test context when its file sits under a `tests/` directory or
follows a `#[cfg(test)]` attribute that opens a module.
-/

open Iso32000 System

namespace Index

def scannedRoots : List String := ["crates", "python", "tests"]

def skippedDirs : List String := ["target", "benches", "__pycache__", ".venv"]

def isSourceFile (path : FilePath) : Bool :=
  match path.extension with
  | some "rs" => true
  | some "py" => true
  | _ => false

def dropWord (s : String) (word : String) : Option String :=
  if s.startsWith word then some (s.drop word.length).toString else none

def separatorChars : List Char := [' ', '`', ',', ':', '(', '§', '\t']

/-- Skips the punctuation and filler words allowed between `ISO 32000-n` and
the reference proper. -/
partial def skipSeparators (s : String) : String :=
  let trimmed := (s.dropWhile (fun c => separatorChars.contains c)).toString
  let stripped := ["clause", "Clause", "Section", "section", "sub-clause", "subclause"].foldl
    (fun acc word => match dropWord acc word with
      | some rest => rest
      | none => acc) trimmed
  if stripped == s then s else skipSeparators stripped

/-- Parses `7.4.2` or `7.4.2.` (trailing period tolerated) into segments. -/
def parseDotted (s : String) : Option (List Nat × String) :=
  let token := (s.takeWhile (fun c => c.isDigit || c == '.')).toString
  let rest := (s.drop token.length).toString
  let cleaned := (token.dropEndWhile (fun c => c == '.')).toString
  if cleaned.isEmpty then none
  else
    let parts := cleaned.splitOn "."
    match parts.mapM String.toNat? with
    | some segments => if segments.isEmpty then none else some (segments, rest)
    | none => none

def parsePart (s : String) : Option Nat × String :=
  match dropWord s "-1", dropWord s "-2" with
  | some rest, _ => (some 1, dropEdition rest)
  | _, some rest => (some 2, dropEdition rest)
  | none, none => (none, s)
where
  dropEdition (s : String) : String :=
    if s.startsWith ":" then (s.dropWhile (fun c => c == ':' || c.isDigit)).toString else s

def resolveTable (part : Option Nat) (number : Nat) : Option Ref :=
  match part with
  | some 2 => none
  | _ => (Outline.tables.find? (·.number == number)).map (·.within)

def parseAnnex (s : String) : Option Ref :=
  match s.toList with
  | letter :: '.' :: rest =>
    if !letter.isUpper then none
    else if rest.head?.any Char.isDigit then
      (parseDotted (String.ofList rest)).map fun (path, _) => .annex letter path
    else some (.annex letter [])
  | letter :: rest =>
    if letter.isUpper && (rest.isEmpty || rest.head?.any (fun c => !c.isAlphanum)) then
      some (.annex letter [])
    else none
  | [] => none

/-- The reference named by the text following `ISO 32000`, if any. -/
def parseReference (part : Option Nat) (s : String) : Option Ref :=
  let s := skipSeparators s
  match dropWord s "Table " with
  | some rest =>
    match (rest.takeWhile Char.isDigit).toString.toNat? with
    | some number => resolveTable part number
    | none => none
  | none =>
    match dropWord s "Annex " with
    | some rest => parseAnnex rest
    | none =>
      match s.front.isDigit, parseDotted s with
      | true, some (path, _) =>
        match path with
        | chapter :: _ => if 1 ≤ chapter && chapter ≤ 14 then some (.clause path) else none
        | [] => none
      | _, _ => none

/-- The text of a comment line without its leader (`///`, `//!`, `//`, `#`,
`*`), so a reference wrapped onto the next line can be read. -/
def commentBody (line : String) : String :=
  let trimmed := line.trimAscii.toString
  let leaders := ["///", "//!", "//", "#", "*"]
  match leaders.find? (fun l => trimmed.startsWith l) with
  | some leader => (trimmed.drop leader.length).trimAscii.toString
  | none => trimmed

/-- The text after the first reference in `s`, when it introduces another
reference of the same standard: `, §8.2`, `; Table 11`, ` and Annex D.2`. -/
def continuation (s : String) : Option String :=
  let rest := skipSeparators s
  let rest := match dropWord rest "and " with
    | some r => skipSeparators r
    | none => rest
  if rest.startsWith "§" || rest.startsWith "Table " || rest.startsWith "Annex " then some rest
  else
    match parseDotted rest with
    | some (_ :: _ :: _, _) => some rest
    | _ => none

/-- Every reference introduced by one `ISO 32000-n` mention: the first one and
the comma-separated ones that follow it. -/
partial def referencesIn (part : Option Nat) (s : String) : List Ref :=
  match parseReference part s with
  | none => []
  | some r =>
    let afterRef := skipSeparators s
    let afterRef := ["Table ", "Annex "].foldl (fun acc w => (dropWord acc w).getD acc) afterRef
    let afterRef := (afterRef.dropWhile (fun c => c.isDigit || c == '.' || c.isUpper)).toString
    match continuation afterRef with
    | some more => r :: referencesIn part more
    | none => [r]

/-- Every mention of the standard on one line, one citation per reference.
When the mention ends the line, the reference is looked for at the start of
the next line. -/
def citationsOnLine (file : String) (lineNumber : Nat) (inTest : Bool) (line : String)
    (nextLine : String) : List Citation :=
  match line.splitOn "ISO 32000" with
  | [] => []
  | _ :: rests => rests.flatMap fun rest =>
    let (part, afterPart) := parsePart rest
    let refs := match referencesIn part afterPart with
      | [] =>
        if (skipSeparators afterPart).isEmpty then referencesIn part (commentBody nextLine)
        else []
      | found => found
    match refs with
    | [] => [{ part, ref := none, file, line := lineNumber, inTest }]
    | found => found.map fun r => { part, ref := some r, file, line := lineNumber, inTest }

def isTestPath (relative : String) : Bool :=
  relative.startsWith "tests/" || (relative.splitOn "/").contains "tests"

/-- Test context begins at a `#[cfg(test)]` attribute whose next non-blank
line opens a module. -/
def opensTestModule (lines : Array String) (i : Nat) : Bool :=
  lines[i]!.trimAscii.toString == "#[cfg(test)]" &&
    (nextNonBlank (i + 1)).any fun l =>
      let l := l.trimAscii.toString
      ["mod ", "pub mod ", "pub(crate) mod ", "pub(super) mod "].any (l.startsWith ·)
where
  nextNonBlank (j : Nat) : Option String :=
    (lines.toList.drop j).find? (fun l => !l.trimAscii.toString.isEmpty)

def scanFile (relative : String) (contents : String) : List Citation := Id.run do
  let lines := (contents.splitOn "\n").toArray
  let mut inTest := isTestPath relative
  let mut found : Array Citation := #[]
  for i in [0:lines.size] do
    if !inTest && opensTestModule lines i then
      inTest := true
    let nextLine := lines[i + 1]?.getD ""
    found := found ++ (citationsOnLine relative (i + 1) inTest lines[i]! nextLine).toArray
  return found.toList

def relativeTo (root : FilePath) (path : FilePath) : String :=
  let rootText := root.toString
  let full := path.toString
  if full.startsWith (rootText ++ "/") then (full.drop (rootText.length + 1)).toString else full

def collectFiles (root : FilePath) : IO (Array FilePath) := do
  let mut files : Array FilePath := #[]
  for sub in scannedRoots do
    let dir := root / sub
    if !(← dir.pathExists) then continue
    let walked ← dir.walkDir fun p => pure !(skippedDirs.contains (p.fileName.getD ""))
    files := files ++ walked.filter isSourceFile
  return files

def orderCitations (a b : Citation) : Bool :=
  a.file < b.file || (a.file == b.file && a.line < b.line)

def renderNat? : Option Nat → String
  | some n => s!"some {n}"
  | none => "none"

def renderPath (path : List Nat) : String :=
  "[" ++ ", ".intercalate (path.map toString) ++ "]"

def renderRef : Ref → String
  | .clause path => s!".clause {renderPath path}"
  | .annex letter path => s!".annex '{letter}' {renderPath path}"

def renderRef? : Option Ref → String
  | some r => s!"some ({renderRef r})"
  | none => "none"

def renderCitation (c : Citation) : String :=
  s!"  ⟨{renderNat? c.part}, {renderRef? c.ref}, {c.file.quote}, {c.line}, {c.inTest}⟩"

def renderModule (citations : Array Citation) : String :=
  let rows := ",\n".intercalate (citations.toList.map renderCitation)
  "import Iso32000.Feature\n\n" ++
  "/-!\nMentions of ISO 32000 in the pdfboss source tree, written by\n" ++
  "`lake exe iso32000-index`. Regenerate rather than edit.\n-/\n\n" ++
  "namespace Iso32000\n\n" ++
  "def Generated.citations : List Citation := [\n" ++ rows ++ "\n]\n\n" ++
  "end Iso32000\n"

end Index

open Index in
def main (args : List String) : IO UInt32 := do
  let root : FilePath := (args.head?.getD ".." : String)
  let output : FilePath := ((args[1]?).getD "Iso32000/Generated.lean" : String)
  let files ← collectFiles root
  let mut citations : Array Citation := #[]
  for file in files.qsort (fun a b => a.toString < b.toString) do
    let contents ← IO.FS.readFile file
    citations := citations ++ (scanFile (relativeTo root file) contents).toArray
  let sorted := citations.qsort orderCitations
  IO.FS.writeFile output (renderModule sorted)
  let resolved := sorted.filter (·.ref.isSome)
  IO.eprintln (s!"{files.size} files, {sorted.size} mentions, {resolved.size} resolved, " ++
    s!"{(resolved.filter (·.inTest)).size} in tests")
  return 0
