//! Corpus census of embedded ICC profiles: classifies every `ICCBased`
//! stream in the PDFs under `CORPUS_DIR`. Run with
//! `CORPUS_DIR=... cargo test -p pdfboss-render --test icc_corpus -- --ignored --nocapture`.

use std::collections::BTreeMap;

use pdfboss_core::{Document, Object};

fn classify(doc: &Document, stream: &pdfboss_core::Stream) -> String {
    let n = stream.dict.get_int("N").unwrap_or(-1);
    let data = match doc.stream_data(stream) {
        Ok(data) => data,
        Err(e) => return format!("stream-undecodable n={n} ({e})"),
    };
    match pdfboss_icc::parse(&data) {
        Ok(profile) => match profile.device_equivalent() {
            Some(eq) => format!("equivalent-{eq:?} n={n}"),
            None => format!(
                "transformed n={n} ch={} dev={:.4}",
                profile.channels(),
                identity_deviation(&profile)
            ),
        },
        Err(e) => format!("parse-{e:?} n={n} len={}", data.len()),
    }
}

fn identity_deviation(profile: &pdfboss_icc::Profile) -> f32 {
    let ch = profile.channels().min(3);
    let mut worst = 0.0f32;
    for axis in 0..ch {
        for v in [1.0 / 16.0, 0.25, 0.5, 0.75, 15.0 / 16.0] {
            let mut input = [0.0f32; 4];
            input[axis] = v;
            let out = profile.transform(&input[..profile.channels().min(4)]);
            for (i, o) in out.iter().enumerate() {
                let want = if i == axis { v } else { 0.0 };
                worst = worst.max((o - want).abs());
            }
        }
    }
    worst
}

#[test]
#[ignore = "runs against a local corpus"]
fn census() {
    let dir = std::env::var("CORPUS_DIR").expect("set CORPUS_DIR");
    let mut totals: BTreeMap<String, usize> = BTreeMap::new();
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "pdf"))
        .collect();
    entries.sort();
    for path in entries {
        let doc = match Document::open(&path) {
            Ok(doc) => doc,
            Err(_) => continue,
        };
        let mut kinds: Vec<String> = Vec::new();
        let nums: Vec<u32> = doc.xref().iter().map(|(num, _)| num).collect();
        for num in nums {
            let obj = match doc.get(pdfboss_core::ObjRef { num, gen: 0 }) {
                Ok(obj) => obj,
                Err(_) => continue,
            };
            let Object::Array(items) = &obj else { continue };
            if items.len() < 2 || !matches!(&items[0], Object::Name(n) if n.0 == "ICCBased") {
                continue;
            }
            let Ok(Object::Stream(s)) = doc.resolve(&items[1]) else {
                continue;
            };
            kinds.push(classify(&doc, &s));
        }
        if kinds.is_empty() {
            continue;
        }
        kinds.sort();
        kinds.dedup();
        for kind in &kinds {
            *totals.entry(kind.clone()).or_default() += 1;
        }
        println!(
            "{}: {}",
            path.file_name().unwrap().to_string_lossy(),
            kinds.join(" | ")
        );
    }
    println!("\n== totals (files) ==");
    for (kind, count) in &totals {
        println!("{count:4}  {kind}");
    }
}
