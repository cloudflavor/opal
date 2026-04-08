use anyhow::Result;
use std::env;
use std::io::{self, IsTerminal, Read};
use std::sync::mpsc::{self, Sender};
use std::thread;

pub fn should_use_color() -> bool {
    if env::var_os("NO_COLOR").is_some() {
        return false;
    }

    if env::var_os("CLICOLOR_FORCE").is_some_and(|v| v != "0") {
        return true;
    }

    match env::var("OPAL_COLOR") {
        Ok(val) if matches!(val.as_str(), "always" | "1" | "true") => return true,
        Ok(val) if matches!(val.as_str(), "never" | "0" | "false") => return false,
        _ => {}
    }

    if !io::stdout().is_terminal() {
        return false;
    }

    true
}

pub fn stream_lines<F>(
    stdout: impl Read + Send + 'static,
    stderr: impl Read + Send + 'static,
    mut on_line: F,
) -> Result<()>
where
    F: FnMut(String) -> Result<()>,
{
    let (tx, rx) = mpsc::channel::<Result<String, io::Error>>();
    spawn_reader(stdout, tx.clone());
    spawn_reader(stderr, tx.clone());
    drop(tx);

    for line in rx {
        let line = line?;
        on_line(line)?;
    }

    Ok(())
}

fn spawn_reader<R>(reader: R, tx: Sender<Result<String, io::Error>>)
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut reader = io::BufReader::new(reader);
        let mut chunk = [0u8; 4096];
        let mut buf = Vec::new();
        let mut skip_lf = false;

        loop {
            match reader.read(&mut chunk) {
                Ok(0) => {
                    if !emit_fragment(&tx, &mut buf) {
                        break;
                    }
                    break;
                }
                Ok(read) => {
                    for &byte in &chunk[..read] {
                        if skip_lf {
                            skip_lf = false;
                            if byte == b'\n' {
                                continue;
                            }
                        }

                        match byte {
                            b'\r' => {
                                if !emit_fragment(&tx, &mut buf) {
                                    return;
                                }
                                skip_lf = true;
                            }
                            b'\n' => {
                                if !emit_fragment(&tx, &mut buf) {
                                    return;
                                }
                            }
                            _ => buf.push(byte),
                        }
                    }
                }
                Err(err) => {
                    let _ = tx.send(Err(err));
                    break;
                }
            }
        }
    });
}

fn emit_fragment(tx: &Sender<Result<String, io::Error>>, buf: &mut Vec<u8>) -> bool {
    if buf.is_empty() {
        return true;
    }
    let line = String::from_utf8_lossy(buf).into_owned();
    buf.clear();
    tx.send(Ok(line)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::stream_lines;
    use anyhow::Result;
    use std::io::Cursor;

    #[test]
    fn stream_lines_emits_carriage_return_progress_immediately() -> Result<()> {
        let mut lines = Vec::new();
        stream_lines(
            Cursor::new(b"layer: downloading 10%\rlayer: downloading 50%\rlayer: pull complete\n"),
            Cursor::new(Vec::<u8>::new()),
            |line| {
                lines.push(line);
                Ok(())
            },
        )?;

        assert_eq!(
            lines,
            vec![
                "layer: downloading 10%",
                "layer: downloading 50%",
                "layer: pull complete",
            ]
        );
        Ok(())
    }

    #[test]
    fn stream_lines_treats_crlf_as_single_line_ending() -> Result<()> {
        let mut lines = Vec::new();
        stream_lines(
            Cursor::new(b"hello\r\nworld\n"),
            Cursor::new(Vec::<u8>::new()),
            |line| {
                lines.push(line);
                Ok(())
            },
        )?;

        assert_eq!(lines, vec!["hello", "world"]);
        Ok(())
    }
}
