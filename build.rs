use std::{error::Error, fs, path::Path};

fn main() -> Result<(), Box<dyn Error>> {
    let path = Path::new("backend/provider/oauth-clients.toml");
    println!("cargo:rerun-if-changed={}", path.display());
    let source = fs::read_to_string(path)?;
    let config: toml::Value = toml::from_str(&source)?;
    for (provider, key, variable) in [
        ("codex", "client_id", "CODEX_OAUTH_CLIENT_ID"),
        ("claude", "client_id", "CLAUDE_OAUTH_CLIENT_ID"),
        ("cursor", "client_id", "CURSOR_OAUTH_CLIENT_ID"),
        ("codev", "client_id", "CODEV_OAUTH_CLIENT_ID"),
    ] {
        let value = config
            .get(provider)
            .and_then(|value| value.get(key))
            .and_then(toml::Value::as_str)
            .filter(|value| !value.trim().is_empty() && !value.contains(['\n', '\r']))
            .ok_or_else(|| format!("invalid OAuth client value: {provider}.{key}"))?;
        println!("cargo:rustc-env={variable}={value}");
    }
    let proto = "backend/provider/cursor/agent.proto";
    println!("cargo:rerun-if-changed={proto}");
    prost_build::Config::new()
        .protoc_executable(protoc_bin_vendored::protoc_bin_path()?)
        .compile_protos(&[proto], &["backend/provider/cursor"])?;
    Ok(())
}
