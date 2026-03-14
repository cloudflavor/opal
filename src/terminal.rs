use anyhow::Result;
use std::env;
use std::io::{self, BufRead, IsTerminal, Read};
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
        loop {
            let mut buf = String::new();
            match reader.read_line(&mut buf) {
                Ok(0) => break,
                Ok(_) => {
                    if buf.ends_with('\n') {
                        buf.pop();
                        if buf.ends_with('\r') {
                            buf.pop();
                        }
                    }
                    if tx.send(Ok(buf)).is_err() {
                        break;
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
