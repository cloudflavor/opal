use owo_colors::OwoColorize;

pub struct LogFormatter {
    use_color: bool,
    line_prefix: String,
}

impl LogFormatter {
    pub fn new(use_color: bool) -> Self {
        let line_prefix = if use_color {
            format!("{}", "    │".dimmed())
        } else {
            "    │".to_string()
        };
        Self {
            use_color,
            line_prefix,
        }
    }

    pub fn line_prefix(&self) -> &str {
        &self.line_prefix
    }

    pub fn format(&self, timestamp: &str, line_no: usize, text: &str) -> String {
        let number = format!("{:04}", line_no);
        let timestamp = if self.use_color {
            format!("{}", timestamp.bold().blue())
        } else {
            timestamp.to_string()
        };
        let number = if self.use_color {
            format!("{}", number.bold().green())
        } else {
            number
        };
        format!("[{} {}] {}", timestamp, number, text)
    }
}

pub fn sanitize_fragments(line: &str) -> Vec<String> {
    expand_carriage_returns(line)
        .into_iter()
        .map(|fragment| strip_control_sequences(&fragment))
        .collect()
}

fn expand_carriage_returns(line: &str) -> Vec<String> {
    let mut parts = Vec::new();
    for fragment in line.split('\r') {
        if fragment.is_empty() {
            continue;
        }
        parts.push(fragment.to_string());
    }
    if parts.is_empty() {
        parts.push(String::new());
    }
    parts
}

fn strip_control_sequences(line: &str) -> String {
    let mut iter = line.bytes().peekable();
    let mut output = Vec::with_capacity(line.len());
    while let Some(b) = iter.next() {
        if b == 0x1b {
            match iter.peek().copied() {
                Some(b'[') => {
                    iter.next();
                    #[allow(clippy::while_let_on_iterator)]
                    while let Some(c) = iter.next() {
                        if (0x40..=0x7E).contains(&c) {
                            break;
                        }
                    }
                    continue;
                }
                Some(b']') => {
                    iter.next();
                    #[allow(clippy::while_let_on_iterator)]
                    while let Some(c) = iter.next() {
                        if c == 0x07 {
                            break;
                        }
                        if c == 0x1b && iter.peek().copied() == Some(b'\\') {
                            iter.next();
                            break;
                        }
                    }
                    continue;
                }
                Some(_) => {
                    iter.next();
                    continue;
                }
                None => break,
            }
        } else if b == b'\x08' {
            output.pop();
        } else {
            output.push(b);
        }
    }

    String::from_utf8_lossy(&output).into_owned()
}
