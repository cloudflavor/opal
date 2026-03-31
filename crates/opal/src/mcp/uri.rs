use anyhow::{Result, bail};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResourceUri {
    History,
    LatestRun,
    Run { run_id: String },
    JobLog { run_id: String, job_name: String },
    RuntimeSummary { run_id: String, job_name: String },
}

pub(crate) fn encode_path_segment(input: &str) -> String {
    let mut encoded = String::new();
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

pub(crate) fn decode_path_segment(input: &str) -> Result<String> {
    let mut decoded = Vec::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                bail!("invalid percent-encoded segment '{input}'");
            }
            let hex = &input[index + 1..index + 3];
            decoded.push(u8::from_str_radix(hex, 16)?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    Ok(String::from_utf8(decoded)?)
}

pub(crate) fn parse_resource_uri(uri: &str) -> Result<ResourceUri> {
    if uri == "opal://history" {
        return Ok(ResourceUri::History);
    }
    if uri == "opal://runs/latest" {
        return Ok(ResourceUri::LatestRun);
    }
    let Some(rest) = uri.strip_prefix("opal://runs/") else {
        bail!("unsupported resource URI '{uri}'");
    };
    let parts = rest.split('/').collect::<Vec<_>>();
    match parts.as_slice() {
        [run_id] => Ok(ResourceUri::Run {
            run_id: decode_path_segment(run_id)?,
        }),
        [run_id, "jobs", job_name, "log"] => Ok(ResourceUri::JobLog {
            run_id: decode_path_segment(run_id)?,
            job_name: decode_path_segment(job_name)?,
        }),
        [run_id, "jobs", job_name, "runtime-summary"] => Ok(ResourceUri::RuntimeSummary {
            run_id: decode_path_segment(run_id)?,
            job_name: decode_path_segment(job_name)?,
        }),
        _ => bail!("unsupported resource URI '{uri}'"),
    }
}

#[cfg(test)]
mod tests {
    use super::{ResourceUri, decode_path_segment, encode_path_segment, parse_resource_uri};

    #[test]
    fn path_segments_round_trip() {
        let original = "build:linux amd64";
        let encoded = encode_path_segment(original);
        let decoded = decode_path_segment(&encoded).expect("decode");
        assert_eq!(decoded, original);
    }

    #[test]
    fn parses_job_log_uri() {
        let uri = "opal://runs/run-1/jobs/build%3Alinux/log";
        let parsed = parse_resource_uri(uri).expect("parse uri");
        assert_eq!(
            parsed,
            ResourceUri::JobLog {
                run_id: "run-1".to_string(),
                job_name: "build:linux".to_string(),
            }
        );
    }
}
