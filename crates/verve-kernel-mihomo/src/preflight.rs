//! Configuration preflight validation using `<mihomo> -t -d <work_dir> -f <path>`.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;
use verve_kernel::ConfigDiagnostic;

/// Runs `mihomo -t -d <work_dir> -f <path>` and converts failures into structured diagnostics.
pub(crate) async fn run_preflight(
    binary_path: &Path,
    work_dir: &Path,
    config_path: &Path,
    timeout: Duration,
) -> Result<(), Vec<ConfigDiagnostic>> {
    let mut cmd = Command::new(binary_path);
    cmd.arg("-t")
        .arg("-d")
        .arg(work_dir)
        .arg("-f")
        .arg(config_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let output = match tokio::time::timeout(timeout, cmd.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(err)) => {
            return Err(vec![ConfigDiagnostic {
                line: None,
                column: None,
                message: format!(
                    "failed to spawn mihomo preflight `{}`: {err}",
                    binary_path.display()
                ),
            }]);
        }
        Err(_) => {
            return Err(vec![ConfigDiagnostic {
                line: None,
                column: None,
                message: format!("mihomo preflight timed out after {}ms", timeout.as_millis()),
            }]);
        }
    };

    if output.status.success() {
        return Ok(());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let diagnostics = parse_preflight_diagnostics(&stdout, &stderr, output.status.code());
    Err(diagnostics)
}

pub(crate) fn parse_preflight_diagnostics(
    stdout: &str,
    stderr: &str,
    exit_code: Option<i32>,
) -> Vec<ConfigDiagnostic> {
    let mut raw_lines = Vec::new();
    for text in [stderr, stdout] {
        for line in text.lines() {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                raw_lines.push(trimmed);
            }
        }
    }

    let mut error_diags = Vec::new();
    let mut all_diags = Vec::new();

    for raw in raw_lines {
        let is_info = raw.contains("level=info") || raw.contains("level=debug");
        let is_error_line = raw.contains("level=error")
            || raw.contains("level=fatal")
            || raw.contains("level=warn")
            || raw.to_ascii_lowercase().contains("failed")
            || raw.to_ascii_lowercase().contains("error");

        let extracted = extract_logrus_msg(raw).unwrap_or_else(|| raw.to_string());
        for subline in split_escaped_newlines(&extracted) {
            let clean = subline.trim();
            if clean.is_empty() {
                continue;
            }
            let (line, column) = extract_line_and_column(clean);
            let diag = ConfigDiagnostic {
                line,
                column,
                message: clean.to_string(),
            };
            if !is_info && (is_error_line || line.is_some()) {
                error_diags.push(diag.clone());
            }
            all_diags.push(diag);
        }
    }

    if !error_diags.is_empty() {
        return error_diags;
    }
    if !all_diags.is_empty() {
        return all_diags;
    }

    let code_text = exit_code.map_or_else(
        || "terminated by signal".to_string(),
        |code| format!("exit code {code}"),
    );
    vec![ConfigDiagnostic {
        line: None,
        column: None,
        message: format!("mihomo configuration preflight failed ({code_text})"),
    }]
}

fn extract_logrus_msg(raw: &str) -> Option<String> {
    let idx = raw.find("msg=\"")?;
    let after = &raw[idx + 5..];
    let mut out = String::with_capacity(after.len());
    let mut chars = after.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => {}
                Some('t') => out.push(' '),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => break,
            },
            '"' => break,
            _ => out.push(ch),
        }
    }
    Some(out)
}

fn split_escaped_newlines(input: &str) -> Vec<&str> {
    input.split('\n').collect()
}

fn extract_line_and_column(text: &str) -> (Option<usize>, Option<usize>) {
    let lower = text.to_ascii_lowercase();
    let line = extract_keyword_number(&lower, &["line ", "line:", "line="]);
    let mut column = extract_keyword_number(&lower, &["column ", "column:", "col ", "col:"]);

    if column.is_none()
        && let Some(line_num) = line
    {
        // Support `line 12:4` format when followed by `:<col>`.
        for prefix in ["line ", "line:", "line="] {
            let needle = format!("{prefix}{line_num}:");
            if let Some(pos) = lower.find(&needle) {
                let rest = &lower[pos + needle.len()..];
                let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
                if !digits.is_empty()
                    && let Ok(parsed) = digits.parse::<usize>()
                {
                    column = Some(parsed);
                    break;
                }
            }
        }
    }

    (line, column)
}

fn extract_keyword_number(lower: &str, prefixes: &[&str]) -> Option<usize> {
    for prefix in prefixes {
        let mut search_from = 0;
        while let Some(rel) = lower[search_from..].find(prefix) {
            let abs = search_from + rel;
            // Ensure word boundary before keyword if preceded by an ASCII alphanumeric char.
            let boundary_ok = abs == 0 || !lower.as_bytes()[abs - 1].is_ascii_alphanumeric();
            let num_start = abs + prefix.len();
            if boundary_ok && num_start < lower.len() {
                let rest = lower[num_start..].trim_start();
                let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
                if !digits.is_empty()
                    && let Ok(parsed) = digits.parse::<usize>()
                {
                    return Some(parsed);
                }
            }
            search_from = num_start;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_real_mihomo_yaml_syntax_and_unmarshal_errors() {
        let stdout = r#"time="2026-11-25T19:42:30+08:00" level=info msg="Start initial configuration in progress"
time="2026-11-25T19:42:30+08:00" level=error msg="yaml: unmarshal errors:\n  line 12: cannot unmarshal !!str `bad` into int"
configuration file C:\temp\broken.yaml test failed"#;

        let diags = parse_preflight_diagnostics(stdout, "", Some(1));
        assert!(diags.iter().all(|d| !d.message.contains("Start initial")));
        assert!(
            diags
                .iter()
                .any(|d| d.line == Some(12) && d.message.contains("cannot unmarshal"))
        );
    }

    #[test]
    fn parses_line_and_column_numbers() {
        let stderr = "yaml: line 7:4: mapping values are not allowed in this context\n";
        let diags = parse_preflight_diagnostics("", stderr, Some(1));
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].line, Some(7));
        assert_eq!(diags[0].column, Some(4));

        let col_word = parse_preflight_diagnostics("", "parse error at line 3, column 9", Some(2));
        assert_eq!(col_word[0].line, Some(3));
        assert_eq!(col_word[0].column, Some(9));
    }

    #[test]
    fn returns_fallback_diagnostic_when_output_is_empty() {
        let diags = parse_preflight_diagnostics("", "", Some(2));
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("exit code 2"));
    }
}
