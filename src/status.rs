use std::fs;
use std::path::Path;

/// Shows the status of a specific service or all services.
pub fn show_status(service: Option<&str>) {
    let service_dir = "/var/run/systemg";

    if !Path::new(service_dir).exists() {
        eprintln!("No services are currently running.");
        return;
    }

    let services: Vec<String> = fs::read_dir(service_dir)
        .unwrap()
        .filter_map(|entry| {
            entry
                .ok()
                .map(|e| e.file_name().to_string_lossy().to_string())
        })
        .collect();

    if let Some(service_name) = service {
        if services.contains(&service_name.to_string()) {
            println!("Service '{}' is running.", service_name);
        } else {
            println!("Service '{}' is NOT running.", service_name);
        }
    } else {
        println!("Active services:");
        for service in services {
            println!("- {}", service);
        }
    }
}
