//! Prints what the catalog readers find in one file: page labels, the
//! outline with each item's destination, named destinations and embedded
//! files. Sections without data are left out.
//!
//! Usage: `cargo run -p pdfboss-core --example catalog -- <file.pdf> [max-lines] [section...]`
//! where `max-lines` caps every section (default 20) and the sections are
//! any of `labels`, `outline`, `destinations` and `files` (default all).

use std::collections::HashMap;
use std::error::Error;
use std::path::Path;

use pdfboss_core::destination::{Destination, DestinationPage, Fit};
use pdfboss_core::object::ObjRef;
use pdfboss_core::outline::OutlineItem;
use pdfboss_core::page_label::PageLabel;
use pdfboss_core::Document;

/// Page indexes by object reference, so a destination can be shown as a
/// page number and label.
struct Pages {
    index_of: HashMap<ObjRef, usize>,
    labels: Vec<String>,
}

impl Pages {
    fn read(doc: &Document) -> Pages {
        let count = doc.page_count();
        let mut index_of = HashMap::with_capacity(count);
        let mut labels = Vec::with_capacity(count);
        for index in 0..count {
            if let Some(reference) = doc.page(index).ok().and_then(|page| page.object_ref()) {
                index_of.insert(reference, index);
            }
            labels.push(doc.page_label(index).unwrap_or_default());
        }
        Pages { index_of, labels }
    }

    fn describe(&self, destination: &Destination) -> String {
        let page = match destination.page {
            DestinationPage::Number(n) => format!("page {} of another file", n + 1),
            DestinationPage::Object(reference) => match self.index_of.get(&reference) {
                Some(index) => format!("page {} ({})", index + 1, self.labels[*index]),
                None => format!("object {} {}", reference.num, reference.gen),
            },
        };
        format!("{page} {}", fit_text(destination.fit))
    }
}

fn number(value: Option<f32>) -> String {
    value.map_or_else(|| "current".to_string(), |v| v.to_string())
}

fn fit_text(fit: Fit) -> String {
    match fit {
        Fit::Xyz { left, top, zoom } => format!(
            "/XYZ left {} top {} zoom {}",
            number(left),
            number(top),
            number(zoom)
        ),
        Fit::Fit => "/Fit".to_string(),
        Fit::FitH { top } => format!("/FitH top {}", number(top)),
        Fit::FitV { left } => format!("/FitV left {}", number(left)),
        Fit::FitR {
            left,
            bottom,
            right,
            top,
        } => format!("/FitR [{left} {bottom} {right} {top}]"),
        Fit::FitB => "/FitB".to_string(),
        Fit::FitBH { top } => format!("/FitBH top {}", number(top)),
        Fit::FitBV { left } => format!("/FitBV left {}", number(left)),
    }
}

/// Prints at most `max` of `lines`, then how many were left out.
fn print_capped(lines: &[String], max: usize) {
    for line in lines.iter().take(max) {
        println!("{line}");
    }
    if lines.len() > max {
        println!("  ... {} more", lines.len() - max);
    }
}

fn label_lines(ranges: &[PageLabel], pages: &Pages) -> Vec<String> {
    let mut lines: Vec<String> = ranges
        .iter()
        .map(|range| {
            let style = range.style.map_or("none", |s| s.code());
            let prefix = range.prefix.as_deref().unwrap_or("");
            format!(
                "  from page {:>4}: style {style:<4} prefix {prefix:?} start {}",
                range.first_page + 1,
                range.start_at
            )
        })
        .collect();
    let shown: Vec<&str> = pages
        .labels
        .iter()
        .take(MAX_LABELS_SHOWN)
        .map(String::as_str)
        .collect();
    let rest = pages.labels.len().saturating_sub(MAX_LABELS_SHOWN);
    let tail = if rest > 0 {
        format!(" ... {rest} more")
    } else {
        String::new()
    };
    lines.push(format!("  labels in page order: {}{tail}", shown.join(" ")));
    lines
}

/// How many page labels the summary line lists before counting the rest.
const MAX_LABELS_SHOWN: usize = 24;

fn outline_lines(items: &[OutlineItem], depth: usize, pages: &Pages, out: &mut Vec<String>) {
    for item in items {
        let target = item
            .destination
            .as_ref()
            .map_or_else(|| "no destination".to_string(), |d| pages.describe(d));
        let marks = format!(
            "{}{}",
            if item.bold { " bold" } else { "" },
            if item.italic { " italic" } else { "" }
        );
        out.push(format!(
            "  {}{} -> {target}{marks}",
            "  ".repeat(depth),
            item.title
        ));
        outline_lines(&item.children, depth + 1, pages, out);
    }
}

fn count_items(items: &[OutlineItem]) -> usize {
    items
        .iter()
        .map(|item| 1 + count_items(&item.children))
        .sum()
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: catalog <file.pdf> [max-lines]");
        std::process::exit(2);
    };
    let max: usize = args.next().map_or(Ok(20), |s| s.parse())?;
    let sections: Vec<String> = args.collect();
    let wanted = |section: &str| sections.is_empty() || sections.iter().any(|s| s == section);
    let doc = Document::open(&path)?;
    let pages = Pages::read(&doc);
    let name = Path::new(&path)
        .file_name()
        .map_or(path.clone(), |n| n.to_string_lossy().into_owned());
    println!("{name}: {} pages", doc.page_count());

    if let Some(ranges) = doc.page_labels().filter(|_| wanted("labels")) {
        println!("\nPage labels ({} ranges)", ranges.len());
        print_capped(&label_lines(&ranges, &pages), max);
    }

    let outline = doc.outline();
    if wanted("outline") && !outline.is_empty() {
        println!("\nOutline ({} items)", count_items(&outline));
        let mut lines = Vec::new();
        outline_lines(&outline, 0, &pages, &mut lines);
        print_capped(&lines, max);
    }

    let destinations = doc.named_destinations();
    if wanted("destinations") && !destinations.is_empty() {
        println!("\nNamed destinations ({})", destinations.len());
        let lines: Vec<String> = destinations
            .iter()
            .map(|(name, destination)| {
                format!(
                    "  {} -> {}",
                    String::from_utf8_lossy(name),
                    pages.describe(destination)
                )
            })
            .collect();
        print_capped(&lines, max);
    }

    let files = doc.embedded_files();
    if wanted("files") && !files.is_empty() {
        println!("\nEmbedded files ({})", files.len());
        let lines: Vec<String> = files
            .iter()
            .map(|file| {
                let data = doc.embedded_file_data(file);
                let size = data.as_ref().map_or_else(
                    |e| format!("unreadable: {e}"),
                    |bytes| format!("{} bytes", bytes.len()),
                );
                let modified = file
                    .modified
                    .map_or_else(String::new, |d| format!(" modified {}", d.to_iso8601()));
                let description = file
                    .spec
                    .description
                    .as_deref()
                    .map_or_else(String::new, |d| format!(" ({d})"));
                format!(
                    "  {}  {}  {size}{modified}{description}",
                    file.name,
                    file.mime.as_deref().unwrap_or("no MIME type")
                )
            })
            .collect();
        print_capped(&lines, max);
    }
    Ok(())
}
