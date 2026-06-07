pub fn apply_header() {
    println!();
    println!("== Theme Manager+ Apply ==");
}

pub fn apply_section(label: &str) {
    println!("{label}");
}

pub fn apply_item(label: &str, value: impl AsRef<str>) {
    println!("  {:<10} {}", label, value.as_ref());
}

pub fn apply_step(label: &str, value: impl AsRef<str>) {
    println!("  {:<10} {}", format!("{label}:"), value.as_ref());
}

pub fn apply_warning(value: impl AsRef<str>) {
    eprintln!("  {:<10} {}", "warning:", value.as_ref());
}

pub fn apply_done() {
    println!("  {:<10} Complete", "status:");
}
