use anyhow::{Context, Result};
use serde_json::Value;
use std::io::{BufRead, Write};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FramingMode {
    ContentLength,
    NewlineDelimited,
}

pub(crate) fn read_message(
    reader: &mut dyn BufRead,
    framing: &mut Option<FramingMode>,
) -> Result<Option<Value>> {
    let mode = match framing {
        Some(mode) => *mode,
        None => match detect_framing(reader)? {
            Some(mode) => {
                *framing = Some(mode);
                mode
            }
            None => return Ok(None),
        },
    };

    match mode {
        FramingMode::ContentLength => read_content_length_message(reader),
        FramingMode::NewlineDelimited => read_newline_message(reader),
    }
}

pub(crate) fn write_message(
    writer: &mut dyn Write,
    message: &Value,
    framing: FramingMode,
) -> Result<()> {
    let body = serde_json::to_vec(message)?;
    match framing {
        FramingMode::ContentLength => {
            write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
            writer.write_all(&body)?;
        }
        FramingMode::NewlineDelimited => {
            writer.write_all(&body)?;
            writer.write_all(b"\n")?;
        }
    }
    writer.flush()?;
    Ok(())
}

fn detect_framing(reader: &mut dyn BufRead) -> Result<Option<FramingMode>> {
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return Ok(None);
        }
        match buffer[0] {
            b'{' => return Ok(Some(FramingMode::NewlineDelimited)),
            b'C' | b'c' => return Ok(Some(FramingMode::ContentLength)),
            b'\n' | b'\r' | b' ' | b'\t' => {
                reader.consume(1);
            }
            _ => return Ok(Some(FramingMode::NewlineDelimited)),
        }
    }
}

fn read_content_length_message(reader: &mut dyn BufRead) -> Result<Option<Value>> {
    let mut content_length = None;

    loop {
        let mut line = String::new();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            return Ok(None);
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("Content-Length")
        {
            content_length = Some(value.trim().parse::<usize>()?);
        }
    }

    let length = content_length.context("missing Content-Length header")?;
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    let message = serde_json::from_slice(&body).context("failed to parse MCP JSON message")?;
    Ok(Some(message))
}

fn read_newline_message(reader: &mut dyn BufRead) -> Result<Option<Value>> {
    loop {
        let mut line = String::new();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            return Ok(None);
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let message = serde_json::from_str(trimmed).context("failed to parse MCP JSON line")?;
        return Ok(Some(message));
    }
}

#[cfg(test)]
mod tests {
    use super::{FramingMode, read_message, write_message};
    use serde_json::json;
    use std::io::Cursor;

    #[test]
    fn reads_content_length_messages() {
        let body = serde_json::to_vec(&json!({"jsonrpc":"2.0","id":1})).expect("json body");
        let payload = format!(
            "Content-Length: {}\r\n\r\n{}",
            body.len(),
            String::from_utf8_lossy(&body)
        );
        let mut reader = Cursor::new(payload.into_bytes());
        let mut framing = None;
        let message = read_message(&mut reader, &mut framing)
            .expect("read message")
            .expect("message");

        assert_eq!(framing, Some(FramingMode::ContentLength));
        assert_eq!(message, json!({"jsonrpc":"2.0","id":1}));
    }

    #[test]
    fn reads_newline_delimited_messages() {
        let mut payload = serde_json::to_vec(&json!({"jsonrpc":"2.0","id":1})).expect("json");
        payload.push(b'\n');
        let mut reader = Cursor::new(payload);
        let mut framing = None;
        let message = read_message(&mut reader, &mut framing)
            .expect("read message")
            .expect("message");

        assert_eq!(framing, Some(FramingMode::NewlineDelimited));
        assert_eq!(message, json!({"jsonrpc":"2.0","id":1}));
    }

    #[test]
    fn writes_newline_delimited_messages() {
        let mut output = Vec::new();
        let message = json!({"jsonrpc":"2.0","result":{}});
        write_message(&mut output, &message, FramingMode::NewlineDelimited).expect("write");

        let written = String::from_utf8(output).expect("utf8");
        let (json_line, newline) = written.split_at(written.len() - 1);
        assert_eq!(newline, "\n");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(json_line).expect("parse written json"),
            message
        );
    }
}
