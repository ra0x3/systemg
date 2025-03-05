use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::Path;

/// Reads and displays the last `n` lines of a service's log file.
///
/// # Arguments
/// * `service_name` - The name of the service.
/// * `lines` - The number of log lines to display.
///
/// # Returns
/// * `Ok(())` if successful, or an error message if logs are unavailable.
pub fn show_logs(service_name: &str, lines: usize) -> io::Result<()> {
    let log_path = format!("/var/log/systemg/{}.log", service_name);

    if !Path::new(&log_path).exists() {
        eprintln!("Error: Log file for service '{}' not found.", service_name);
        return Ok(());
    }

    let file = File::open(log_path)?;
    let reader = BufReader::new(file);
    let all_lines: Vec<String> = reader.lines().map_while(Result::ok).collect();

    let start = if all_lines.len() > lines {
        all_lines.len() - lines
    } else {
        0
    };
    for line in &all_lines[start..] {
        println!("{}", line);
    }

    Ok(())
}
