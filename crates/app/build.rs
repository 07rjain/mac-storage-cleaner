//! Makes `SENTRY_DSN` available to `option_env!`, from the environment or the workspace `.env`.

use std::path::Path;

fn main() {
    println!("cargo:rerun-if-env-changed=SENTRY_DSN");

    let env_file = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.env");
    if env_file.exists() {
        println!("cargo:rerun-if-changed={}", env_file.display());
    }

    if std::env::var_os("SENTRY_DSN").is_some() {
        return;
    }

    let Ok(contents) = std::fs::read_to_string(&env_file) else {
        return;
    };
    if let Some(dsn) = dotenv_value(&contents, "SENTRY_DSN") {
        println!("cargo:rustc-env=SENTRY_DSN={dsn}");
    }
}

fn dotenv_value(contents: &str, key: &str) -> Option<String> {
    contents.lines().find_map(|line| {
        let (name, value) = line.trim().split_once('=')?;
        if name.trim() != key {
            return None;
        }
        let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
        (!value.is_empty()).then(|| value.to_owned())
    })
}
