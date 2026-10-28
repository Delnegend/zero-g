#[path = "../legacy_import.rs"]
mod legacy_import;
#[path = "../storage.rs"]
mod storage;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os();
    args.next();
    let (Some(source), Some(destination), None) = (args.next(), args.next(), args.next()) else {
        return Err("usage: zero-g-import-legacy SOURCE DESTINATION".into());
    };
    legacy_import::import(source.as_ref(), destination.as_ref()).await
}
