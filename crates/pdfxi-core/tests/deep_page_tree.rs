//! A deeply nested page tree must produce a typed error, never a stack overflow.
//!
//! `Document::from_bytes` walks `/Pages` recursively. A crafted chain of a few
//! hundred megabytes is not needed: a few hundred nested objects is enough to
//! exhaust a small stack, and a stack overflow aborts the process, which the
//! panic hook cannot catch. The test runs on a deliberately small stack so the
//! bound is exercised even on a machine with a large default stack.

use pdfxi_core::{Document, Error};

/// Build a classic-table PDF whose page tree is `depth` nodes deep.
fn deep_page_tree(depth: usize) -> Vec<u8> {
    let mut out = String::from("%PDF-1.7\n");
    let mut offsets = Vec::with_capacity(depth + 2);
    offsets.push(out.len());
    out.push_str("1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    for level in 0..depth {
        let number = 2 + level;
        offsets.push(out.len());
        let parent = if level == 0 {
            String::new()
        } else {
            format!("/Parent {} 0 R ", number - 1)
        };
        out.push_str(&format!(
            "{number} 0 obj\n<< /Type /Pages {parent}/Kids [{} 0 R] /Count 1 >>\nendobj\n",
            number + 1
        ));
    }
    let page = 2 + depth;
    offsets.push(out.len());
    out.push_str(&format!(
        "{page} 0 obj\n<< /Type /Page /Parent {} 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        page - 1
    ));
    let xref = out.len();
    out.push_str(&format!("xref\n0 {}\n0000000000 65535 f \n", page + 1));
    for offset in &offsets {
        out.push_str(&format!("{offset:010} 00000 n \n"));
    }
    out.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        page + 1
    ));
    out.into_bytes()
}

fn open_on_small_stack(bytes: Vec<u8>) -> Result<usize, Error> {
    std::thread::Builder::new()
        .name("deep-page-tree".to_owned())
        .stack_size(256 * 1024)
        .spawn(move || Document::from_bytes(bytes).and_then(|document| document.page_count()))
        .expect("spawn parser thread")
        .join()
        .expect("parser thread must not overflow its stack")
}

#[test]
fn a_deep_but_bounded_page_tree_still_parses() {
    let pages = open_on_small_stack(deep_page_tree(50)).expect("50 levels parse");
    assert_eq!(pages, 1);
}

#[test]
fn a_hostile_page_tree_depth_is_a_typed_error() {
    for depth in [200, 20_000] {
        match open_on_small_stack(deep_page_tree(depth)) {
            Err(Error::MalformedPageTree(message)) => {
                assert!(
                    message.contains("nested deeper"),
                    "unexpected message at depth {depth}: {message}"
                );
            }
            other => panic!("depth {depth} must be rejected with MalformedPageTree, got {other:?}"),
        }
    }
}
