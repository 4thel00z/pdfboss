//! Canvas emission: turns paginated draw items into [`pdfboss_write::Page`]
//! values ready for [`pdfboss_write::Pdf`] assembly.

use pdfboss_write::{LinkAnnotation, Page, PageSize};

use crate::layout::{Item, LaidPage};
use crate::Error;

/// Emits one [`Page`] per laid-out page, painting every item onto its
/// canvas in order and collecting link items onto [`Page::links`].
pub(crate) fn emit(laid: Vec<LaidPage>, page_size: PageSize) -> Result<Vec<Page>, Error> {
    laid.into_iter()
        .map(|laid_page| {
            let mut page = Page::new(page_size);
            for item in laid_page.items {
                match item {
                    Item::Text {
                        x,
                        y,
                        text,
                        font,
                        size,
                        color,
                    } => {
                        page.canvas.set_fill(color);
                        page.canvas.text(&text, x, y, font, size)?;
                    }
                    Item::Rect { x, y, w, h, color } => {
                        page.canvas.set_fill(color);
                        page.canvas.rect(x, y, w, h);
                        page.canvas.fill();
                    }
                    Item::Stroke {
                        x1,
                        y1,
                        x2,
                        y2,
                        width,
                        color,
                    } => {
                        page.canvas.set_stroke(color);
                        page.canvas.set_line_width(width);
                        page.canvas.move_to(x1, y1);
                        page.canvas.line_to(x2, y2);
                        page.canvas.stroke();
                    }
                    Item::Frame {
                        x,
                        y,
                        w,
                        h,
                        width,
                        color,
                    } => {
                        page.canvas.set_stroke(color);
                        page.canvas.set_line_width(width);
                        page.canvas.rect(x, y, w, h);
                        page.canvas.stroke();
                    }
                    Item::Image { x, y, w, h, data } => {
                        let handle = page.canvas.add_image(data);
                        page.canvas.draw_image(handle, x, y, w, h);
                    }
                    Item::Link { x, y, w, h, uri } => {
                        page.links.push(LinkAnnotation {
                            rect: [x, y, x + w, y + h],
                            uri,
                        });
                    }
                }
            }
            Ok(page)
        })
        .collect()
}
