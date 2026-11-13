use anyhow::{Context, Result};
use std::io::Write;

pub mod clock;
pub mod doctor;
pub mod forecast;
pub mod html;
pub mod model;
pub mod source;
pub mod status;
pub mod store;
pub mod timer;
pub mod tui;

fn write_slices_broken_pipe_safe(
    slices: &[&[u8]],
    err_context: &str,
    writer: &mut impl Write,
) -> Result<()> {
    let io_result = (|| -> std::io::Result<()> {
        for slice in slices {
            writer.write_all(slice)?;
        }
        writer.flush()
    })();
    match io_result {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(e) => Err(e).with_context(|| err_context.to_string()),
    }
}

/// Writes `bytes` and flushes `writer`, treating `BrokenPipe` as a clean no-op.
pub fn write_broken_pipe_safe(
    bytes: &[u8],
    err_context: &str,
    writer: &mut impl Write,
) -> Result<()> {
    write_slices_broken_pipe_safe(&[bytes], err_context, writer)
}

/// Writes `line` followed by `\n` and flushes `writer`, treating `BrokenPipe` as a clean no-op.
pub fn write_line_broken_pipe_safe(
    line: &str,
    err_context: &str,
    writer: &mut impl Write,
) -> Result<()> {
    write_slices_broken_pipe_safe(&[line.as_bytes(), b"\n"], err_context, writer)
}

/// Neutralizes ASCII/Unicode line/paragraph separators to `' '` and strips other control characters.
pub fn sanitize_single_line(s: &str) -> String {
    s.chars()
        .filter_map(|c| match c {
            '\n' | '\r' | '\t' | '\u{0085}' | '\u{2028}' | '\u{2029}' => Some(' '),
            _ if c.is_control() => None,
            _ => Some(c),
        })
        .collect()
}
