//! Beginner example: read a commented config, change it, write it back
//! with every comment kept. (`k8s_manifest` is the advanced tour.)

use oneyaml::Document;

const CONFIG: &str = "\
# Service settings
name: web          # shown in the dashboard
port: 8080
debug: false
hosts:
  - a.example.com  # primary
";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    print!("{}", edit()?);
    Ok(())
}

fn edit() -> Result<String, Box<dyn std::error::Error>> {
    let mut doc = Document::parse(CONFIG)?;

    // Typed reads, by key or by path.
    let port = doc.get("port").and_then(|n| n.as_u64());
    let debug = doc.get_path("debug").and_then(|n| n.as_bool());
    assert_eq!((port, debug), (Some(8080), Some(false)));

    // Edits: string paths, values or references, a push onto a list.
    doc.set_path("port", 9090)?;
    doc.set_path("name", "api")?;
    doc.get_mut("hosts")?.push("b.example.com")?;

    // Everything else, comments and spacing included, is byte for byte.
    let out = doc.to_string();
    assert_eq!(
        out,
        "\
# Service settings
name: api          # shown in the dashboard
port: 9090
debug: false
hosts:
  - a.example.com  # primary
  - b.example.com
"
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn edit_config() {
        assert!(super::edit().is_ok());
    }
}
