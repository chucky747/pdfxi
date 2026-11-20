//! The `pdfxi api` command: a local line-delimited JSON automation API.
//!
//! stdin is read one line at a time and each line produces exactly one compact
//! JSON response line on stdout. No sockets are opened and no external service
//! is contacted; the caller is the local process that launched `pdfxi`. A
//! [`CliHost`] bridges the protocol to real action/toolset files and to the
//! same staging document executor used by `pdfxi action run --json`, so the
//! API and the CLI cannot drift apart.
//!
//! Lines are read with a hard cap of [`MAX_LINE_BYTES`](pdfxi_automation::MAX_LINE_BYTES)
//! bytes. An overlong line is consumed up to its newline so the stream stays
//! in sync, then the engine returns a `line_too_long` error response. Blank
//! lines carry no request and are skipped in stream mode.

use std::io::{self, BufReader, Read, Write};
use std::path::PathBuf;

use pdfxi_automation::{Action, ApiEngine, ApiHost, JsonValue};

/// Bridges the JSON API to local files and the CLI's staging executor.
struct CliHost;

impl ApiHost for CliHost {
    fn read_file(&self, path: &str) -> Result<String, String> {
        if path.as_bytes().contains(&0) {
            return Err("file path contains a NUL byte".to_owned());
        }
        std::fs::read_to_string(path).map_err(|error| format!("failed to read '{path}': {error}"))
    }

    fn run_action(&self, action: &Action, inputs: &[String]) -> Result<JsonValue, String> {
        let paths: Vec<PathBuf> = inputs.iter().map(PathBuf::from).collect();
        let json = crate::action::run_loaded_action_json(action, &paths, None)
            .map_err(|error| format!("{error:#}"))?;
        JsonValue::parse(&json).map_err(|error| format!("could not encode run result: {error}"))
    }
}

/// Reads at most `limit + 1` bytes of the next line from `reader`.
///
/// Bytes beyond the cap are discarded until the newline (or EOF), which keeps
/// a hostile line from allocating unbounded memory while preserving the
/// framing of subsequent lines. The returned string is UTF-8-lossy so even
/// invalid bytes produce one response instead of a process error. `Ok(None)`
/// means EOF with no bytes pending.
fn read_bounded_line<R: Read>(reader: &mut R, limit: usize) -> io::Result<Option<String>> {
    let mut bytes: Vec<u8> = Vec::new();
    let mut one = [0u8; 1];
    loop {
        let read = reader.read(&mut one)?;
        if read == 0 {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
            };
        }
        if one[0] == b'\n' {
            return Ok(Some(String::from_utf8_lossy(&bytes).into_owned()));
        }
        if bytes.len() <= limit {
            bytes.push(one[0]);
        }
    }
}

/// Runs the API loop over stdin/stdout.
///
/// In stream mode (the default) every non-blank line is answered and the loop
/// continues until EOF. With `once`, the first non-blank line is answered and
/// the command exits; an exhausted stdin produces no output.
pub fn run_api(once: bool) -> anyhow::Result<()> {
    let engine = ApiEngine::new();
    let host = CliHost;
    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = io::stdout();
    let mut writer = stdout.lock();

    loop {
        let line = read_bounded_line(&mut reader, pdfxi_automation::MAX_LINE_BYTES)
            .map_err(|error| anyhow::anyhow!("failed to read a request line: {error}"))?;
        let Some(mut line) = line else {
            break;
        };
        while line.ends_with('\r') {
            line.pop();
        }
        if line.trim().is_empty() {
            continue;
        }
        let response = engine.handle_line(&line, &host);
        writer.write_all(response.as_bytes())?;
        writer.write_all(b"\n")?;
        writer.flush()?;
        if once {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_reader_returns_a_line_without_a_trailing_newline() {
        let mut input = io::Cursor::new(b"hello".to_vec());
        assert_eq!(
            read_bounded_line(&mut input, 16).expect("read"),
            Some("hello".to_owned())
        );
        assert_eq!(read_bounded_line(&mut input, 16).expect("read"), None);
    }

    #[test]
    fn bounded_reader_caps_a_long_line_and_keeps_stream_framing() {
        let mut data = vec![b'x'; 40];
        data.push(b'\n');
        data.extend_from_slice(b"next\n");
        let mut input = io::Cursor::new(data);
        let first = read_bounded_line(&mut input, 8)
            .expect("read")
            .expect("line");
        assert_eq!(first.len(), 9, "one byte past the cap signals oversize");
        assert_eq!(
            read_bounded_line(&mut input, 8).expect("read"),
            Some("next".to_owned())
        );
    }
}
